// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Judges for `lab forever` coverage chains (`--mode cover`, `--mode cap`, `--mode wall`) from tab records ([`Seen`], [`Cover`]).

use super::*;

/// How much earlier than the landing's journal line the manager's wait may
/// have returned and still be the landing's answer (the record is marked
/// done at the landing; the lab's clock reads the client's exit).
const AWAIT_SLACK_MS: i64 = 2_000;

/// One verdict of several judges' over the same records: FAIL where any
/// failed, else UNOBSERVED where any went unobserved, else PASS — each one's
/// words kept.
fn compose(said: &str, parts: &[(&str, Verdict)]) -> Verdict {
    let all = parts
        .iter()
        .map(|(id, v)| format!("{id}'s judge {}: {}", v.word(), v.why()))
        .collect::<Vec<_>>()
        .join(" | ");
    let text = format!("{said}; {all}");
    if parts.iter().any(|(_, v)| matches!(v, Verdict::Fail(_))) {
        Verdict::Fail(text)
    } else if parts
        .iter()
        .any(|(_, v)| matches!(v, Verdict::Unobserved(_)))
    {
        Verdict::Unobserved(text)
    } else {
        Verdict::Pass(text)
    }
}

/// The first `EVENT idle` after `t0` and before `until` whose words hold
/// `key`, else the words the first one read (for the report).
fn read_holding<'a>(
    seen: &'a Seen,
    t0: i64,
    until: i64,
    key: &str,
) -> Result<&'a (i64, String), String> {
    let lines = || {
        seen.journal
            .iter()
            .filter(move |(t, _)| *t > t0 && *t <= until)
    };
    if let Some(hit) = lines().find(|(_, l)| idle_words(l).is_some_and(|w| w.contains(key))) {
        return Ok(hit);
    }
    Err(lines()
        .find_map(|(t, l)| idle_words(l).map(|w| format!("+{} read \"{w}\"", at(t0, *t))))
        .unwrap_or_else(|| "no EVENT idle".to_string()))
}

/// The phase and words of a point's journal line, `EVENT <phase> seq=<n>
/// <words>`: `idle`, or the phase the loop read it in (`question`: Claude
/// Code's question at its yield is journaled so).
fn event_words(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("EVENT ")?;
    let (phase, rest) = rest.split_once(' ')?;
    let (seq, words) = rest.split_once(' ').unwrap_or((rest, ""));
    seq.starts_with("seq=").then_some((phase, words.trim()))
}

/// A NEEDS YOU need raised on the tab (`NEEDS` line or `ask:needs` in `meta`).
fn need_raised(seen: &Seen) -> Option<String> {
    if let Some((_, l)) = seen.journal.iter().find(|(_, l)| l.starts_with("NEEDS ")) {
        return Some(format!("journal {}", crate::util::one_line(l, 100)));
    }
    seen.meta
        .iter()
        .find(|(_, m)| needs_up(m))
        .map(|(_, m)| format!("meta {}", crate::util::one_line(m, 120)))
}

/// Puts on `bad` a task record the run read where none was given (`OK task -` throughout is
/// none), or says why the record is unknown: a `task` read whose reply failed records nothing,
/// and a run where none answered proves nothing about the record.
pub(super) fn invented_record(seen: &Seen, bad: &mut Vec<String>) -> Option<&'static str> {
    if seen.task.is_empty() {
        return Some("no task read answered, so the record is unknown");
    }
    if let Some(r) = seen
        .task
        .iter()
        .map(|(_, r)| r.trim())
        .find(|r| *r != "OK task -")
    {
        bad.push(format!("a task record where none was given: {r}"));
    }
    None
}

/// A verdict of the same word, `said` before its words.
fn said_before(said: &str, v: Verdict) -> Verdict {
    match v {
        Verdict::Pass(w) => Verdict::Pass(format!("{said}; {w}")),
        Verdict::Fail(w) => Verdict::Fail(format!("{said}; {w}")),
        Verdict::Unobserved(w) => Verdict::Unobserved(format!("{said}; {w}")),
    }
}

/// The first Land instruction needs no back-off. Its two writers stamp independently, so
/// sequence identity and each record stream's append order join its reply, point and act.
/// An earlier instruction, ambiguous act or real back-off takes the ordinary path.
fn first_land_at_point(seen: &Seen, tt: i64, point: &(i64, String), act: &(i64, String)) -> bool {
    if act_rule(&act.1) != Some(turn_end::RULE_LAND) {
        return false;
    }
    let Some(seq) = word(&point.1, "seq").and_then(|seq| seq.parse::<u64>().ok()) else {
        return false;
    };
    if word(&act.1, "seq").and_then(|seq| seq.parse::<u64>().ok()) != Some(seq) {
        return false;
    }
    let mut acts = seen.journal.iter().enumerate().filter(|(_, (_, line))| {
        act_rule(line) == Some(turn_end::RULE_LAND)
            && word(line, "seq").and_then(|seq| seq.parse::<u64>().ok()) == Some(seq)
    });
    let Some((act_index, recorded)) = acts.next() else {
        return false;
    };
    if recorded != act || acts.next().is_some() {
        return false;
    }
    let Some(point_index) = seen.journal.iter().position(|entry| entry == point) else {
        return false;
    };
    if point_index >= act_index {
        return false;
    }
    let typed_land = |row: &Row| row.rule == turn_end::RULE_LAND && row.decision == "typed";
    let mut typed = seen
        .ledger
        .iter()
        .enumerate()
        .filter(|(_, row)| typed_land(row) && row.box_seq == Some(seq));
    let Some((typed_index, typed_row)) = typed.next() else {
        return false;
    };
    if typed.next().is_some() {
        return false;
    }
    let prior_land = seen.journal[..act_index]
        .iter()
        .any(|(_, line)| act_rule(line) == Some(turn_end::RULE_LAND))
        || seen.ledger[..typed_index].iter().any(typed_land);
    let backoff = seen.journal[point_index..act_index]
        .iter()
        .any(|(_, line)| backoff_named(line));
    !prior_land
        && !backoff
        && typed_row.ts > tt
        && seen.ledger[..typed_index].iter().any(|row| {
            row.ts > tt
                && row.ts <= typed_row.ts
                && row.rule == RULE_POINT
                && row.decision == "waiting"
                && row.box_seq == Some(seq)
        })
}

/// Judges that the orchestrator's turn at `tt` was read and acted on at its judged point,
/// respecting a back-off where one was decided. The first Land instruction may go immediately.
fn judged_resumption<'a>(
    seen: &'a Seen,
    tt: i64,
    key: &str,
    t0: i64,
) -> Result<(String, &'a (i64, String)), Verdict> {
    let act_index = seen
        .journal
        .iter()
        .position(|(t, line)| *t > tt && act_rule(line).is_some());
    let act = act_index.map(|index| &seen.journal[index]);
    let before_act = &seen.journal[..act_index.unwrap_or(seen.journal.len())];
    let point_index = before_act.iter().position(|(t, line)| {
        *t > tt && event_words(line).is_some_and(|(_, words)| words.contains(key))
    });
    let point = point_index.map(|index| &seen.journal[index]);
    let waited = point_index.and_then(|index| {
        let seq = word(&seen.journal[index].1, "seq");
        before_act[index..]
            .iter()
            .find(|(_, line)| backoff_named(line) && word(line, "seq") == seq)
    });
    let read = point.map_or_else(
        || format!("its reply never read (no EVENT holding \"{key}\")"),
        |(te, el)| {
            format!(
                "its reply read at +{} ({}){}",
                at(t0, *te),
                crate::util::one_line(el, 90),
                waited.map_or_else(
                    || "; NO back-off decided at that point".to_string(),
                    |(tw, wl)| format!("; +{} {}", at(t0, *tw), crate::util::one_line(wl, 110))
                )
            )
        },
    );
    match (waited, act) {
        (Some((_, wl)), Some(a)) => {
            let rule = act_rule(&a.1).unwrap_or("?");
            if let Some(due) = backoff_due(wl)
                && a.0 < due - DUE_SLACK_MS
            {
                return Err(Verdict::Fail(format!(
                    "{read}; {rule} typed at +{}, {} BEFORE the due its back-off named",
                    at(t0, a.0),
                    at(0, due - a.0)
                )));
            }
            Ok((
                format!(
                    "{read}; {rule} typed at +{}, {} after the turn",
                    at(t0, a.0),
                    at(0, a.0 - tt)
                ),
                a,
            ))
        }
        (None, Some(a)) if point.is_some_and(|point| first_land_at_point(seen, tt, point, a)) => {
            Ok((
                format!(
                    "{read}; the first Land instruction typed at +{} at its recorded point, with no back-off owed",
                    at(t0, a.0)
                ),
                a,
            ))
        }
        (None, Some(a)) => Err(Verdict::Fail(format!(
            "{read}; {} typed at +{} after the turn at +{}, at no point judged at its reply",
            act_rule(&a.1).unwrap_or("?"),
            at(t0, a.0),
            at(t0, tt)
        ))),
        (_, None) => Err(match lost_after(seen, tt, t0) {
            Some(lost) => Verdict::Fail(format!("{read}: {lost}")),
            None => Verdict::Unobserved(format!(
                "{read}; no act after the turn within {}",
                at(t0, seen.end_ms)
            )),
        }),
    }
}

/// Judges L12: a question in prose at the land act's yield.
pub(super) fn judge_answer(seen: &Seen, t0: i64) -> Verdict {
    let Some(t1) = seen.acts(turn_end::RULE_LAND).first().map(|(t, _)| *t) else {
        return Verdict::Fail(format!("no land@v1 typed within {}", at(t0, seen.end_ms)));
    };
    let acts = seen.all_acts();
    let next = acts.iter().copied().find(|(t, _)| *t > t1);
    let until = next.map(|(t, _)| *t);
    let asked = seen.journal.iter().find(|(t, l)| {
        *t > t1
            && until.is_none_or(|n| *t < n)
            && event_words(l).is_some_and(|(_, w)| w.contains(QUESTION_KEY))
    });
    if let Some((d, l)) = seen
        .first_after("DONE ", t1)
        .filter(|(d, _)| until.is_none_or(|n| *d < n))
    {
        return Verdict::Fail(format!(
            "LANDED at +{} at the question's point, before any answer: {}",
            at(t0, *d),
            crate::util::one_line(l, 120)
        ));
    }
    if let Some((s, _)) = seen.status.iter().find(|(t, s)| {
        *t > t1 && until.is_none_or(|n| *t < n) && word(s, "task").as_deref() == Some("landed")
    }) {
        return Verdict::Fail(format!(
            "status said task=landed at +{} at the question's point",
            at(t0, *s)
        ));
    }
    let Some(act) = next else {
        if let Some(lost) = lost_after(seen, t1, t0) {
            return Verdict::Fail(format!(
                "land@v1 typed at +{}, {}: {lost}",
                at(t0, t1),
                asked.map_or_else(
                    || "its yield (the question) never read".to_string(),
                    |(te, _)| format!("the question read at +{}", at(t0, *te))
                )
            ));
        }
        return Verdict::Unobserved(match asked {
            Some((te, _)) => format!(
                "land@v1 typed at +{}, the question read at +{}, but nothing typed for it within {}",
                at(t0, t1),
                at(t0, *te),
                at(t0, seen.end_ms)
            ),
            None => format!(
                "land@v1 typed at +{}, but its yield (the question) not read within {}",
                at(t0, t1),
                at(t0, seen.end_ms)
            ),
        });
    };
    let rule = act_rule(&act.1).unwrap_or("?");
    if rule != turn_end::RULE_LAND_ANSWER {
        return Verdict::Fail(format!(
            "the question at land@v1's yield got {rule} at +{}, not land-answer@v1: {}",
            at(t0, act.0),
            crate::util::one_line(seen.act_text(rule, act), 300)
        ));
    }
    let Some((te, el)) = asked else {
        return Verdict::Fail(format!(
            "land-answer@v1 typed at +{}, but no EVENT line read the question (\"{QUESTION_KEY}\") before it",
            at(t0, act.0)
        ));
    };
    let Some(row) = seen.typed_row(turn_end::RULE_LAND_ANSWER, act.0) else {
        return Verdict::Fail(format!(
            "land-answer@v1 journaled at +{}, but NO ledger row typed",
            at(t0, act.0)
        ));
    };
    if !row.command.starts_with(turn_end::LAND_ANSWER) {
        return Verdict::Fail(format!(
            "the answer at +{} is not Land's answer: {}",
            at(t0, act.0),
            crate::util::one_line(&row.command, 300)
        ));
    }
    let stray: Vec<&str> = [OPEN_SUFFIX, REPO_SUFFIX, NEEDS_ACK]
        .into_iter()
        .filter(|w| row.command.contains(w))
        .collect();
    if !stray.is_empty() {
        return Verdict::Fail(format!(
            "the answer at +{} says {stray:?}: {}",
            at(t0, act.0),
            crate::util::one_line(&row.command, 300)
        ));
    }
    if let Some(n) = seen.needs_seen() {
        return Verdict::Fail(format!("a need was raised, though no reply named one: {n}"));
    }
    let said = format!(
        "land@v1 typed at +{}; the question read at +{} ({}); land-answer@v1 typed at +{}, ledgered, Land's answer with no suffix",
        at(t0, t1),
        at(t0, *te),
        crate::util::one_line(el, 90),
        at(t0, act.0)
    );
    let Some(done) = seen.first_after("DONE ", act.0) else {
        return Verdict::Fail(format!(
            "{said}; but no DONE within {} after the answer's clean defense",
            at(t0, seen.end_ms)
        ));
    };
    // The answer's defense gets the land check at its yield, and the check's `LANDED` lands
    // (§14.17 item 5): one check between the answer and the landing, and no other act.
    let between: Vec<&(i64, String)> = acts
        .iter()
        .copied()
        .filter(|(t, _)| *t > act.0 && *t < done.0)
        .collect();
    let checked = between.len() == 1 && act_rule(&between[0].1) == Some(turn_end::RULE_LAND_CHECK);
    if let Some((tb, lb)) = between.iter().copied().find(|_| !checked) {
        return Verdict::Fail(format!(
            "{said}; but {} typed at +{} before the landing: the answer's yield got {} act(s), not the one land check",
            act_rule(lb).unwrap_or("?"),
            at(t0, *tb),
            between.len()
        ));
    }
    let said = match between.first() {
        Some((tc, _)) => format!("{said}; the land check at +{}", at(t0, *tc)),
        None => said,
    };
    let (mut bad, quiet, window) = landing_quiet(seen, done, false, t0);
    let unread = invented_record(seen, &mut bad);
    let said = format!("{said}; {quiet}");
    match unread {
        Some(why) if bad.is_empty() => Verdict::Unobserved(format!("{said}: {why}")),
        _ => landed_verdict(bad, said, seen, done.0, window),
    }
}

/// Judges L13: a person's own exchange and follow-up question under Land.
pub(super) fn judge_theirs(seen: &Seen, t0: i64) -> Verdict {
    let Some(p) = seen.cover.person_ms else {
        return Verdict::Unobserved("the person's prompt was never typed".into());
    };
    if let Some(keyed) = &seen.cover.keyed {
        let delivery = keys_unaccounted(keyed).unwrap_or_else(|| keyed_line(keyed));
        return Verdict::Unobserved(format!(
            "{delivery}; hwkey is controller input, so L13's person's takeover was not measured; \
             draft effects and LEFT/task=theirs do not establish whose hand it was"
        ));
    }
    let stamp = seen.cover.person_stamp.as_deref().unwrap_or("?");
    let path = "the `as=person` seam";
    if stamp.parse::<u64>().is_err() {
        return Verdict::Unobserved(format!(
            "no person's hand reached the session: status human_ms={stamp} after the Enter at +{} \
             ({path} did not stamp it), so the prompt was nobody's",
            at(t0, p)
        ));
    }
    let typed = format!("typed through {path}");
    let until = seen.cover.turn_ms.unwrap_or(seen.end_ms);
    let inside = |t: i64| t > p && t < until;
    let left_rule = format!("rule={}", turn_end::RULE_LEFT);
    let left = seen
        .journal
        .iter()
        .find(|(t, l)| inside(*t) && l.starts_with("LEFT ") && l.contains(&left_rule));
    let theirs = seen
        .status
        .iter()
        .find(|(t, s)| *t >= p && *t <= until && word(s, "task").as_deref() == Some("theirs"));
    let acts = seen.all_acts();
    let typed_in: Vec<&(i64, String)> = acts.iter().copied().filter(|(t, _)| inside(*t)).collect();
    let rows_in = seen
        .ledger
        .iter()
        .filter(|r| inside(r.ts) && r.acted())
        .count();
    let done_in = seen
        .journal
        .iter()
        .find(|(t, l)| inside(*t) && l.starts_with("DONE "));
    let stub = seen.cover.window_stub.as_ref();
    let mut bad = Vec::new();
    if left.is_none() {
        bad.push(format!("NO LEFT … {left_rule} line"));
    }
    if theirs.is_none() {
        bad.push("status never said task=theirs".to_string());
    }
    if !typed_in.is_empty() || rows_in > 0 {
        bad.push(format!(
            "TYPED in the person's exchange: {} journal act(s){}, {rows_in} ledger act(s)",
            typed_in.len(),
            typed_in.first().map_or_else(String::new, |(t, l)| format!(
                " (the first {} at +{})",
                act_rule(l).unwrap_or("?"),
                at(t0, *t)
            ))
        ));
    }
    if let Some((d, _)) = done_in {
        bad.push(format!("a DONE at +{} in the exchange", at(t0, *d)));
    }
    if let Some(s) = stub.filter(|s| s.new_turns > 0) {
        bad.push(format!(
            "the stub saw {} request(s) holding a harness text in the exchange",
            s.new_turns
        ));
    }
    if let Some(n) = seen.needs_seen() {
        bad.push(format!("a need was raised, though no reply named one: {n}"));
    }
    let said = format!(
        "{typed}; the person's Enter at +{}; {}; {}; {} for {} after it; the stub {}; the orchestrator's turn {}",
        at(t0, p),
        left.map_or_else(
            || "NO LEFT".to_string(),
            |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 90))
        ),
        theirs.map_or_else(
            || "never task=theirs".to_string(),
            |(t, _)| format!("+{} status task=theirs", at(t0, *t))
        ),
        if typed_in.is_empty() && rows_in == 0 {
            "nothing typed"
        } else {
            "TYPED"
        },
        at(0, until - p),
        stub.map_or_else(
            || "UNREAD".to_string(),
            |s| format!(
                "read {} request(s) of the exchange, {} with a harness text",
                s.read, s.new_turns
            )
        ),
        seen.cover
            .turn_ms
            .map_or_else(|| "not typed".to_string(), |t| format!("at +{}", at(t0, t)))
    );
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if stub.is_none() {
        return Verdict::Unobserved(format!(
            "{said}: the stub's dumps of the exchange were not read"
        ));
    }
    if until - p < QUIET_UNDER_OFF_MS {
        return Verdict::Unobserved(format!(
            "{said}: watched under {} s",
            QUIET_UNDER_OFF_MS / 1000
        ));
    }
    let Some(tt) = seen.cover.turn_ms else {
        return Verdict::Unobserved(format!("{said}: the orchestrator's turn never typed"));
    };
    match judged_resumption(seen, tt, WORK_KEY, t0) {
        Ok((went, (ta, _))) => {
            let landed = seen.first_after("DONE ", *ta).map_or_else(
                || "; not landed within the watch".to_string(),
                |(d, _)| format!("; its clean defense landed it at +{}", at(t0, *d)),
            );
            Verdict::Pass(format!(
                "{said}; the orchestrator's turn: {went}, only after the exchange ended{landed}"
            ))
        }
        Err(v) => said_before(&format!("{said}; the orchestrator's turn"), v),
    }
}

/// Returns why native controller delivery was not proved without changing person provenance.
#[must_use]
pub fn keys_unaccounted(k: &Keyed) -> Option<String> {
    if k.posted == 0 {
        return Some("no native key was posted".to_string());
    }
    if k.key_window != k.posted {
        return Some(format!(
            "hwkey posted {} key(s), but only {} to the active key window",
            k.posted, k.key_window
        ));
    }
    if !k.draft_stood || !k.draft_taken {
        return Some(format!(
            "native composer effects were not read: draft stood={}, draft taken={}",
            k.draft_stood, k.draft_taken
        ));
    }
    let Some(after) = k.seq_after else {
        return Some(format!(
            "hwkey posted {} key(s), and status human_seq= was not read after the Enter",
            k.posted
        ));
    };
    if after == k.seq_before {
        return None;
    }
    Some(format!(
        "hwkey posted {} controller key(s), but status human_seq= changed {} -> {after}",
        k.posted, k.seq_before
    ))
}

/// Formats native controller posting, independent composer effects and unchanged person count.
#[must_use]
pub fn keyed_line(k: &Keyed) -> String {
    format!(
        "controller hwkey: {} key(s) posted ({} to the active key window), draft stood={}, draft taken={}, status human_seq= {} -> {}",
        k.posted,
        k.key_window,
        k.draft_stood,
        k.draft_taken,
        k.seq_before,
        k.seq_after
            .map_or_else(|| "?".to_string(), |n| n.to_string())
    )
}

/// Judges L14: P2 live under Land with a work reply before landing; the defense's land check
/// carries the needs ack (L3, `check_after`), and its `LANDED` lands (L5).
pub(super) fn judge_wrote_land<'a>(
    seen: &'a Seen,
    land: Option<&(i64, String)>,
    check_after: &dyn Fn(i64) -> Option<&'a (i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let before = land.map_or(seen.end_ms, |(t, _)| *t);
    // The first point's decision: its back-off, else Land's instruction typed at it at once (v6
    // §14.10).
    let judged = seen
        .journal
        .iter()
        .find(|(t, l)| *t > t0 && *t < before && backoff_named(l))
        .or(land);
    let point = judged.and_then(|(tw, wl)| {
        let seq = word(wl, "seq");
        seen.journal.iter().rfind(|(t, l)| {
            *t > t0 && *t <= *tw && l.starts_with("EVENT ") && word(l, "seq") == seq
        })
    });
    let (tr, lr) = match (judged, point) {
        (_, Some(hit)) if event_words(&hit.1).is_some_and(|(_, w)| w.contains(W_WORK_KEY)) => hit,
        (Some((tw, wl)), _) => {
            return Verdict::Fail(format!(
                "the first point judged (+{} {}) read {}, not the reply opening \"{W_WORK_KEY} …\"",
                at(t0, *tw),
                crate::util::one_line(wl, 100),
                point.map_or_else(
                    || "no EVENT of its seq".to_string(),
                    |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 90))
                )
            ));
        }
        (None, _) => {
            let unread = read_holding(seen, t0, before, W_WORK_KEY).map_or_else(
                |unread| unread,
                |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 90)),
            );
            return Verdict::Fail(format!(
                "no point judged before +{} on the reply opening \"{W_WORK_KEY} …\": {unread}",
                at(t0, before)
            ));
        }
    };
    let said = format!(
        "its words read at +{} ({})",
        at(t0, *tr),
        crate::util::one_line(lr, 90)
    );
    let done = seen.first_after("DONE ", t0);
    compose(
        &said,
        &[
            ("L1", judge_land_act(seen, land, done, t0)),
            ("L3", judge_needs(seen, land, check_after, program, t0)),
            ("L5", judge_landed(seen, land, false, t0)),
        ],
    )
}

/// Judges C3: P2 live under Cruise with a done claim before continuation.
pub(super) fn judge_wrote_cruise(seen: &Seen, done: Option<&(i64, String)>, t0: i64) -> Verdict {
    let first = seen.all_acts().first().copied();
    let before = first.map_or(seen.end_ms, |(t, _)| *t);
    let (tr, lr) = match read_holding(seen, t0, before, W_DONE_KEY) {
        Ok(hit) => hit,
        Err(unread) => {
            return Verdict::Fail(format!(
                "the done claim opening \"{W_DONE_KEY} …\" was never read before +{}: {unread}",
                at(t0, before)
            ));
        }
    };
    if let Some((t, l)) = first
        && act_rule(l) != Some(turn_end::RULE_DONE_CHECK)
    {
        return Verdict::Fail(format!(
            "the done claim got {} at +{}, not the done check",
            act_rule(l).unwrap_or("?"),
            at(t0, *t)
        ));
    }
    let said = format!(
        "the claim read at +{} ({})",
        at(t0, *tr),
        crate::util::one_line(lr, 90)
    );
    compose(
        &said,
        &[
            ("C1", judge_check(seen, t0)),
            ("C2", judge_cruise_goes_on(seen, done, t0)),
        ],
    )
}

/// Judges C4: the owner's two-paragraph continuation ([`OWNER_CONTINUE`]) under Cruise.
pub(super) fn judge_owner_continuation(
    seen: &Seen,
    done: Option<&(i64, String)>,
    t0: i64,
) -> Verdict {
    let Some(act) = seen.acts(turn_end::RULE_CONTINUE).first().copied() else {
        return match lost_after(seen, t0, t0) {
            Some(lost) => Verdict::Fail(format!("no continuation was typed: {lost}")),
            None => Verdict::Unobserved(format!(
                "no continuation was typed within {}",
                at(t0, seen.end_ms)
            )),
        };
    };
    let typed = seen.act_text(turn_end::RULE_CONTINUE, act);
    let mut bad = Vec::new();
    if !(typed.contains(OWNER_KEY) && typed.contains(OWNER_TAIL)) {
        bad.push(format!(
            "the act typed is not the owner's text: {}",
            crate::util::one_line(typed, 120)
        ));
    }
    if typed.contains(['\n', '\r']) {
        bad.push("the act's text holds a line break".to_string());
    }
    let reached = match &seen.cover.owner_texts {
        None => None,
        Some(texts) => {
            if texts.is_empty() {
                bad.push("no request of the conversation carried the owner's words".into());
            }
            if let Some(t) = texts.iter().find(|t| t.contains(['\n', '\r'])) {
                bad.push(format!(
                    "the owner's words reached the agent with a line break: {t:?}"
                ));
            }
            if !texts.is_empty()
                && !texts
                    .iter()
                    .any(|t| t.contains(OWNER_KEY) && t.contains(OWNER_TAIL))
            {
                bad.push("no text that reached the agent holds both paragraphs".into());
            }
            Some(texts.len())
        }
    };
    if let Some(n) = need_raised(seen) {
        bad.push(format!("a need was raised, though no reply named one: {n}"));
    }
    if let Some((d, l)) = done {
        bad.push(format!(
            "ENDED at +{}: {}",
            at(t0, *d),
            crate::util::one_line(l, 90)
        ));
    }
    if seen.status_said("task", "landed", t0) {
        bad.push("status said task=landed under Cruise".into());
    }
    let next = seen
        .all_acts()
        .into_iter()
        .find(|(t, _)| *t > act.0)
        .map(|(t, _)| *t);
    let reads = || {
        seen.journal
            .iter()
            .filter(move |(t, _)| *t > act.0 && next.is_none_or(|n| *t < n))
    };
    if let Some((t, l)) = reads().find(|(_, l)| {
        idle_words(l).is_some_and(|w| w.contains(OWNER_KEY) || w.contains(OWNER_TAIL))
    }) {
        bad.push(format!(
            "the act's own words read as the agent's at +{}: {}",
            at(t0, *t),
            crate::util::one_line(l, 120)
        ));
    }
    let reply = reads().find(|(_, l)| idle_words(l).is_some_and(|w| w.contains(DONE_CLAIM_KEY)));
    let said = format!(
        "continue@v1 at +{} ({}); {}",
        at(t0, act.0),
        crate::util::one_line(typed, 90),
        reached.map_or_else(
            || "the dumps not read".to_string(),
            |n| format!("{n} text(s) of it reached the agent")
        )
    );
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    let Some((tr, lr)) = reply else {
        return match lost_after(seen, act.0, t0) {
            Some(lost) => Verdict::Fail(format!("{said}; its reply never read: {lost}")),
            None => Verdict::Unobserved(format!(
                "{said}; its reply (\"{DONE_CLAIM_KEY} …\") not read{}",
                next.map_or_else(
                    || format!(" within {}", at(t0, seen.end_ms)),
                    |n| format!(" before the next act at +{}", at(t0, n))
                )
            )),
        };
    };
    let said = format!(
        "{said}; its reply read at +{} ({})",
        at(t0, *tr),
        crate::util::one_line(lr, 90)
    );
    if reached.is_none() {
        return Verdict::Unobserved(format!("{said}; what reached the agent unread"));
    }
    Verdict::Pass(said)
}

/// A task record's age (`since_ms=`, ms since its `since`).
fn age(record: &str) -> Option<i64> {
    word(record, "since_ms").and_then(|v| v.parse().ok())
}

/// Judges P5: a worker's task record given before its agent started.
pub(super) fn judge_startup(seen: &Seen, t0: i64) -> Verdict {
    let state = |r: &str| word(r, "state").unwrap_or_default();
    let records: Vec<&(i64, String)> = seen
        .task
        .iter()
        .filter(|(_, r)| r.contains("state="))
        .collect();
    let Some((tg, _)) = records.iter().copied().find(|(_, r)| state(r) == "working") else {
        return Verdict::Unobserved(format!(
            "the task record never read working (the mark did not take): {}",
            seen.task.first().map_or("not read", |(_, r)| r.as_str())
        ));
    };
    let done_at = records
        .iter()
        .find(|(_, r)| state(r) == "done")
        .map_or(i64::MAX, |(t, _)| *t);
    let mut bad = Vec::new();
    if let Some((t, r)) = records
        .iter()
        .copied()
        .find(|(t, r)| *t < done_at && state(r) != "working")
    {
        bad.push(format!(
            "the record read state={}{} at +{} (age {}): {}",
            state(r),
            word(r, "why").map_or_else(String::new, |w| format!(" why={w}")),
            at(t0, *t),
            age(r).map_or_else(|| "?".to_string(), |a| at(0, a)),
            crate::util::one_line(r, 200)
        ));
    }
    let gaps = rev_gaps(seen, t0);
    if !gaps.is_empty() {
        bad.push(format!(
            "the record changed between looks unread: {}",
            gaps.join(", ")
        ));
    }
    let said_never = |text: &str| pct_decode(text).contains("never started");
    if let Some((t, _)) = seen
        .task
        .iter()
        .chain(&seen.meta)
        .find(|(t, x)| *t < done_at && said_never(x))
    {
        bad.push(format!(
            "an attention said the worker never started at +{}",
            at(t0, *t)
        ));
    }
    if let Some((t, s)) = seen
        .status
        .iter()
        .find(|(t, s)| *t < done_at && word(s, "needs").is_some_and(|n| n != "-"))
    {
        bad.push(format!(
            "status read needs={} at +{}",
            word(s, "needs").unwrap_or_default(),
            at(t0, *t)
        ));
    }
    let oldest = records
        .iter()
        .filter(|(t, r)| *t < done_at && state(r) == "working")
        .filter_map(|(t, r)| Some((*t, age(r)?)))
        .max_by_key(|(_, a)| *a);
    let landed = (done_at != i64::MAX).then(|| {
        records
            .iter()
            .find(|(t, _)| *t == done_at)
            .map_or_else(String::new, |(_, r)| {
                format!(
                    "; then done{} at +{}",
                    word(r, "why").map_or_else(String::new, |w| format!(" why={w}")),
                    at(t0, done_at)
                )
            })
    });
    let said = format!(
        "the record working from +{}{}; the deadline {} s, judged past it by {} s{}",
        at(t0, *tg),
        oldest.map_or_else(String::new, |(t, a)| format!(
            " to age {} at +{}",
            at(0, a),
            at(t0, t)
        )),
        STARTUP_DEADLINE_MS / 1000,
        PAST_DEADLINE_MS / 1000,
        landed.unwrap_or_default()
    );
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    match oldest {
        Some((_, a)) if a >= STARTUP_DEADLINE_MS + PAST_DEADLINE_MS => Verdict::Pass(format!(
            "{said}; never stalled, needs=- throughout, no never-started attention"
        )),
        _ => Verdict::Unobserved(format!(
            "{said}: never read working past the deadline and {} s more",
            PAST_DEADLINE_MS / 1000
        )),
    }
}

/// Where a worker of `program` takes its result from, as `result`'s header
/// names it: Claude Code's transcript (`source=transcript`), every other
/// harness's screen (`source=screen program=<program>`).
fn result_source(program: &str) -> (&'static str, Option<&str>) {
    if program == Harness::Claude.name() {
        ("transcript", None)
    } else {
        ("screen", Some(program))
    }
}

/// Judges L15: a spawned worker (`spawn agent=<program>`) carried to its landed task record.
pub(super) fn judge_spawned(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let (Some(mgr), Some(spawn)) = (&seen.cover.starter, &seen.cover.spawn) else {
        return Verdict::Unobserved("no worker was spawned".into());
    };
    let w = spawn
        .strip_prefix("OK ")
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or("?");
    let mut bad = Vec::new();
    if seen.cover.agent.as_deref() != Some(program) {
        bad.push(format!(
            "the spawn named agent={}, not the harness under test's {program}",
            seen.cover.agent.as_deref().unwrap_or("-")
        ));
    }
    if seen.cover.worker.as_deref() != Some(program) {
        bad.push(format!(
            "meta names worker={}, not {program}",
            seen.cover.worker.as_deref().unwrap_or("(not read)")
        ));
    }
    let (source, by) = result_source(program);
    let head = seen.cover.result_head.as_deref().unwrap_or("");
    if word(head, "source").as_deref() != Some(source) || word(head, "program").as_deref() != by {
        bad.push(format!(
            "result's header is not source={source}{}: {head:?}",
            by.map_or_else(String::new, |p| format!(" program={p}"))
        ));
    }
    let first = seen.task.iter().find(|(_, r)| r.contains("state="));
    let from_mgr = first.is_some_and(|(_, r)| {
        word(r, "state").as_deref() == Some("working")
            && word(r, "from").as_deref() == Some(mgr.as_str())
    });
    if !from_mgr {
        bad.push(format!(
            "its record's first read is not working from={mgr}: {}",
            first.map_or("not read", |(_, r)| r.as_str())
        ));
    }
    let named = seen
        .status
        .iter()
        .find(|(_, s)| word(s, "autopilot").is_some_and(|w| w != "-"));
    if !named.is_some_and(|(_, s)| word(s, "autopilot").as_deref() == Some("land")) {
        bad.push(format!(
            "its first status naming an Autopilot is not autopilot=land: {}",
            named.map_or("none named one", |(_, s)| s.as_str())
        ));
    }
    let done = seen.first_after("DONE ", t0).map(|(t, _)| *t);
    let waited = match &seen.cover.awaited {
        None => {
            bad.push("the manager's await task never returned within the watch".into());
            "NOT returned".to_string()
        }
        Some((ta, code, out)) => {
            let head = format!("@{w} ");
            let row = out.lines().find(|l| l.starts_with(&head));
            if *code != 0 {
                bad.push(format!("the manager's await task exited {code}"));
            }
            if row.is_none_or(|r| word(r, "state").as_deref() != Some("done")) {
                bad.push(format!(
                    "the manager's await task row is not state=done: {}",
                    crate::util::one_line(out, 200)
                ));
            }
            if let Some(d) = done
                && *ta < d - AWAIT_SLACK_MS
            {
                bad.push(format!(
                    "the manager's await task returned at +{}, before the landing at +{}",
                    at(t0, *ta),
                    at(t0, d)
                ));
            }
            format!(
                "returned at +{} ({})",
                at(t0, *ta),
                crate::util::one_line(row.unwrap_or(out), 140)
            )
        }
    };
    let want = format!("{WORK}\n\n{CLEAN}\n");
    match &seen.cover.result {
        Some(r) if *r == want => {}
        Some(r) => bad.push(format!(
            "result is not the report and the defense byte for byte: {r:?}"
        )),
        None => bad.push("its result was never read".into()),
    }
    let said = format!(
        "spawned agent={} from @{mgr} ({spawn}); the manager's await task {waited}; result {} \
         ({})",
        seen.cover.agent.as_deref().unwrap_or("-"),
        if seen.cover.result.as_deref() == Some(want.as_str()) {
            "byte for byte"
        } else {
            "NOT as stubbed"
        },
        crate::util::one_line(head, 100)
    );
    let l7 = judge_tasked(seen, land, t0);
    if bad.is_empty() {
        compose(&said, &[("L7", l7)])
    } else {
        Verdict::Fail(format!(
            "{said}: {}; L7's judge {}: {}",
            bad.join("; "),
            l7.word(),
            l7.why()
        ))
    }
}

/// Whether CAP's tab has been watched to its verdict ([`finished`]).
pub(super) fn cap_watched(s: &Seen, now: i64) -> bool {
    let acts = s.all_acts();
    let Some((t1, _)) = acts.first().copied() else {
        return false;
    };
    let window_end = t1 + cap_window_ms();
    acts.iter()
        .any(|(t, _)| *t > t1 + AT_DONE_MS && *t < window_end)
        || s.cover
            .turn_ms
            .is_some_and(|tt| acts.iter().any(|(t, _)| *t > tt && now - *t >= 60_000))
}

/// Judges CAP (`continue_per_hour = 1`) escalation after the hourly continuation budget is spent.
pub(super) fn judge_cap(seen: &Seen, t0: i64) -> Verdict {
    let acts = seen.all_acts();
    let Some(first) = acts.first().copied() else {
        return Verdict::Fail(format!("no act typed within {}", at(t0, seen.end_ms)));
    };
    let t1 = first.0;
    let window_end = t1 + cap_window_ms();
    let in_hour = |t: i64| t > t1 + AT_DONE_MS && t < window_end;
    let turn = seen.cover.turn_ms;
    let quiet_end = turn.unwrap_or(seen.end_ms);
    let quiet = |t: i64| t > t1 + AT_DONE_MS && t < quiet_end;
    let typed_in: Vec<&(i64, String)> = acts.iter().copied().filter(|(t, _)| quiet(*t)).collect();
    let rows_in = seen
        .ledger
        .iter()
        .filter(|r| quiet(r.ts) && matches!(r.decision.as_str(), "typed" | "approved" | "declined"))
        .count();
    let escalated = seen
        .journal
        .iter()
        .find(|(t, l)| in_hour(*t) && l.starts_with("ESCALATED "));
    let reason = cap_reason();
    let named = seen
        .meta
        .iter()
        .find(|(t, m)| *t > t1 && *t < window_end && attention_text(m).contains(&reason));
    let stub = seen.cover.window_stub.as_ref();
    let mut bad = Vec::new();
    if !typed_in.is_empty() || rows_in > 0 {
        bad.push(format!(
            "TYPED after the first act, before any new point: {} journal act(s){}, {rows_in} ledger act(s)",
            typed_in.len(),
            typed_in.first().map_or_else(String::new, |(t, l)| format!(
                " (the first {} at +{})",
                act_rule(l).unwrap_or("?"),
                at(t0, *t)
            ))
        ));
    }
    if escalated.is_none() {
        bad.push("no ESCALATED after the first act within the hour".into());
    }
    if named.is_none() {
        bad.push(format!("the attention never named \"{reason}\""));
    }
    if let Some(s) = stub.filter(|s| s.new_turns > 0) {
        bad.push(format!(
            "the stub saw {} request(s) holding a new harness text within the hour",
            s.new_turns
        ));
    }
    if let Some(n) = need_raised(seen) {
        bad.push(format!("a need was raised, though no reply named one: {n}"));
    }
    let said = format!(
        "{} typed at +{} (the hour's one act); {}; {}; nothing typed to +{} (the hour ends +{}); the stub {}; the orchestrator's turn {}",
        act_rule(&first.1).unwrap_or("?"),
        at(t0, t1),
        escalated.map_or_else(
            || "NO ESCALATED".to_string(),
            |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 80))
        ),
        named.map_or_else(
            || "the cap never named".to_string(),
            |(t, m)| format!(
                "+{} attention {}",
                at(t0, *t),
                crate::util::one_line(attention_text(m), 140)
            )
        ),
        at(t0, quiet_end),
        at(t0, window_end),
        stub.map_or_else(
            || "UNREAD".to_string(),
            |s| format!(
                "read {} request(s) of the hour, {} with a new harness text",
                s.read, s.new_turns
            )
        ),
        turn.map_or_else(|| "not typed".to_string(), |t| format!("at +{}", at(t0, t)))
    );
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if seen.end_ms < window_end {
        return Verdict::Unobserved(format!("{said}: watched under the hour"));
    }
    let Some(tt) = turn else {
        return Verdict::Unobserved(format!("{said}: no turn typed after the hour"));
    };
    if tt < window_end {
        return Verdict::Unobserved(format!("{said}: the turn came inside the hour"));
    }
    if stub.is_none() {
        return Verdict::Unobserved(format!("{said}: the stub's dumps were not read"));
    }
    let (went, (ta, la)) = match judged_resumption(seen, tt, OPEN_KEY, t0) {
        Ok(hit) => hit,
        Err(v) => return said_before(&format!("{said}; the orchestrator's turn"), v),
    };
    let rule = act_rule(la).unwrap_or("?");
    if seen.typed_row(rule, *ta).is_none() {
        return Verdict::Fail(format!(
            "{said}; {rule} journaled at +{} after it, but NO ledger row typed",
            at(t0, *ta)
        ));
    }
    Verdict::Pass(format!("{said}; resumed: {went}"))
}

/// Whether `line` is a journal point that reads the usage wall.
pub(super) fn wall_point(line: &str) -> bool {
    line.starts_with("EVENT limited ")
        || (line.starts_with("EVENT prompt ")
            && word(line, "kind").as_deref() == Some("usage-limit"))
}

/// Judges W1: a usage wall ([`WALL_CLAIM`]) encountered and waited out under Land.
pub(super) fn judge_wall(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let Some((raised, reset_s)) = seen.cover.wall else {
        return Verdict::Unobserved(
            "the stub's wall was never raised (no first point read)".into(),
        );
    };
    let reset = reset_s * 1000;
    let Some(&(t1, _)) = land else {
        return Verdict::Fail(format!("no land@v1 typed within {}", at(t0, seen.end_ms)));
    };
    if t1 < raised || t1 >= reset {
        return Verdict::Unobserved(format!(
            "land@v1 typed at +{}, outside the wall (raised at +{}, reset at +{})",
            at(t0, t1),
            at(t0, raised),
            at(t0, reset)
        ));
    }
    let lift = seen
        .cover
        .lifted_ms
        .unwrap_or(reset - WALL_LIFT_EARLY_MS)
        .min(reset);
    // A land check past the lift answers the program's own continuation's defense, read on a
    // screen without the wall (§14.17 item 5): no act into the wall.
    let in_wall = |t: i64, rule: &str| {
        t > t1 + AT_DONE_MS && t < reset && !(rule == turn_end::RULE_LAND_CHECK && t >= lift)
    };
    let acts = seen.all_acts();
    let typed_in: Vec<&(i64, String)> = acts
        .iter()
        .copied()
        .filter(|(t, l)| in_wall(*t, act_rule(l).unwrap_or("")))
        .collect();
    let rows_in: Vec<&Row> = seen
        .ledger
        .iter()
        .filter(|r| in_wall(r.ts, &r.rule) && r.acted() && r.decision != "escalated")
        .collect();
    let escalated = seen
        .ledger
        .iter()
        .find(|r| r.ts > t1 && r.ts < reset && r.decision == "escalated");
    let points = || {
        seen.journal
            .iter()
            .filter(move |(t, _)| *t > t1 && *t < reset)
    };
    let read = points().find(|(_, l)| wall_point(l));
    let misread = points().find(|(_, l)| {
        !wall_point(l)
            && event_words(l).is_some_and(|(_, w)| w.to_ascii_lowercase().contains("usage limit"))
    });
    let waited = read.and_then(|(rt, rl)| {
        let seq = word(rl, "seq");
        seen.journal
            .iter()
            .find(|(t, l)| *t >= *rt && l.starts_with("WAITING ") && word(l, "seq") == seq)
    });
    let done_in = points().find(|(t, l)| *t < lift && l.starts_with("DONE "));
    let landed_in = seen
        .status
        .iter()
        .find(|(t, s)| *t > t1 && *t < lift && word(s, "task").as_deref() == Some("landed"));
    let stub = seen.cover.window_stub.as_ref();
    let mut bad = Vec::new();
    if !typed_in.is_empty() || !rows_in.is_empty() {
        bad.push(format!(
            "TYPED into the wall: {} journal act(s){}, {} ledger act(s){}",
            typed_in.len(),
            typed_in.first().map_or_else(String::new, |(t, l)| format!(
                " (the first {} at +{})",
                act_rule(l).unwrap_or("?"),
                at(t0, *t)
            )),
            rows_in.len(),
            rows_in.first().map_or_else(String::new, |r| format!(
                " (the first {} {} at +{})",
                r.rule,
                r.decision,
                at(t0, r.ts)
            ))
        ));
    }
    if let Some(r) = escalated {
        bad.push(format!(
            "ESCALATED the wall to a person at +{}: {}",
            at(t0, r.ts),
            crate::util::one_line(&r.reason, 140)
        ));
    }
    if read.is_none()
        && let Some((t, l)) = misread
    {
        bad.push(format!(
            "the wall at +{} never read as one: {}",
            at(t0, *t),
            crate::util::one_line(l, 140)
        ));
    }
    if let Some((d, l)) = done_in {
        bad.push(format!(
            "LANDED at +{} while the wall stood: {}",
            at(t0, *d),
            crate::util::one_line(l, 100)
        ));
    }
    if let Some((s, _)) = landed_in {
        bad.push(format!(
            "status said task=landed at +{} while the wall stood",
            at(t0, *s)
        ));
    }
    if let Some(s) = stub.filter(|s| s.new_turns > 0) {
        bad.push(format!(
            "the stub saw {} request(s) holding a new harness text in the wall",
            s.new_turns
        ));
    }
    if let Some(n) = seen.needs_seen() {
        bad.push(format!("a need was raised, though no reply named one: {n}"));
    }
    let said = format!(
        "the wall raised at +{} (resets +{}); land@v1 typed at +{} met it; {}{}; nothing typed to the reset; the stub {}",
        at(t0, raised),
        at(t0, reset),
        at(t0, t1),
        read.map_or_else(
            || "NO limited point".to_string(),
            |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 140))
        ),
        waited.map_or_else(String::new, |(t, l)| format!(
            "; +{} {}",
            at(t0, *t),
            crate::util::one_line(l, 100)
        )),
        stub.map_or_else(
            || "UNREAD".to_string(),
            |s| format!(
                "read {} request(s) of the wall, {} with a new harness text",
                s.read, s.new_turns
            )
        )
    );
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if read.is_none() {
        return Verdict::Unobserved(format!(
            "{said}: the wall never drawn as a limit point before its reset"
        ));
    }
    if stub.is_none() {
        return Verdict::Unobserved(format!(
            "{said}: the stub's dumps of the wall were not read"
        ));
    }
    let lost_at = reset + wall_grace_ms(program) + WALL_LOST_MS;
    let done = seen.first_after("DONE ", lift);
    let went_on = seen.cover.went_on_ms;
    // The land check is typed only at a land act's yield with a defense, never at a wall: the
    // program's own continuation's defense gets it, which is no act on top of that continuation.
    let first_act = acts
        .iter()
        .copied()
        .find(|(t, l)| *t >= reset && act_rule(l) != Some(turn_end::RULE_LAND_CHECK));
    let resumed = match first_act {
        Some((ta, la)) => {
            if *ta > lost_at {
                return Verdict::Fail(format!(
                    "{said}; PERSISTENCE LOST: the first act after the reset came {} after it, past the {} deadline",
                    at(0, ta - reset),
                    at(0, lost_at - reset)
                ));
            }
            let rule = act_rule(la).unwrap_or("?");
            if let Some(tv) = went_on.filter(|tv| tv < ta) {
                return Verdict::Fail(format!(
                    "{said}; TYPED ON TOP of the program's own continuation: it went on at +{} ({} past the reset), and {rule} was typed at +{}, {} after it",
                    at(t0, tv),
                    at(reset, tv),
                    at(t0, *ta),
                    at(tv, *ta)
                ));
            }
            if seen.typed_row(rule, *ta).is_none() {
                return Verdict::Fail(format!(
                    "{said}; {rule} journaled at +{} after the reset, but NO ledger row typed",
                    at(t0, *ta)
                ));
            }
            format!(
                "gone on: {rule} typed at +{} ({} past the reset), ledgered",
                at(t0, *ta),
                at(0, ta - reset)
            )
        }
        None if let Some((d, _)) = done => {
            if *d > lost_at {
                return Verdict::Fail(format!(
                    "{said}; nothing typed after the reset; PERSISTENCE LOST: the landing came {} after the reset, past the {} deadline",
                    at(0, d - reset),
                    at(0, lost_at - reset)
                ));
            }
            let Some(tv) = went_on else {
                return Verdict::Unobserved(format!(
                    "{said}; nothing typed after the reset: the program's own retry landed it at +{}, so the harness going on past the wall is unobserved",
                    at(t0, *d)
                ));
            };
            format!(
                "gone on: nothing typed after the reset, the program's own continuation at +{} ({} past the reset) answered",
                at(t0, tv),
                at(reset, tv)
            )
        }
        None if seen.end_ms >= lost_at => {
            return Verdict::Fail(format!(
                "{said}; PERSISTENCE LOST: nothing typed and no landing in the {} after the reset",
                at(0, seen.end_ms - reset)
            ));
        }
        None => {
            return Verdict::Unobserved(format!(
                "{said}; watched to +{}, under {} past the reset",
                at(t0, seen.end_ms),
                at(0, lost_at - reset)
            ));
        }
    };
    let Some(done) = done else {
        if seen.end_ms >= lost_at {
            return Verdict::Fail(format!(
                "{said}; {resumed}; but NO landing in the {} after the reset",
                at(0, seen.end_ms - reset)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; {resumed}; not landed by +{}",
            at(t0, seen.end_ms)
        ));
    };
    if done.0 > lost_at {
        return Verdict::Fail(format!(
            "{said}; {resumed}; PERSISTENCE LOST: the landing came {} after the reset, past the {} deadline",
            at(0, done.0 - reset),
            at(0, lost_at - reset)
        ));
    }
    let (mut bad, quiet, window) = landing_quiet(seen, done, false, t0);
    let unread = invented_record(seen, &mut bad);
    let said = format!("{said}; {resumed}; {quiet}");
    match unread {
        Some(why) if bad.is_empty() => Verdict::Unobserved(format!("{said}: {why}")),
        _ => landed_verdict(bad, said, seen, done.0, window),
    }
}

/// Judges W2: a remembered usage wall ([`judge_wall`]) after the wall scrolls off screen.
pub(super) fn judge_wall_hidden(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let (said, driven) = hidden_facts(seen, land, t0);
    match (judge_wall(seen, land, program, t0), driven) {
        (Verdict::Fail(w), _) => Verdict::Fail(format!("{said}; W1's judge: {w}")),
        (_, Err(why)) => {
            Verdict::Unobserved(format!("{said}: the remembered wall was not driven: {why}"))
        }
        (Verdict::Unobserved(w), Ok(())) => Verdict::Unobserved(format!("{said}; W1's judge: {w}")),
        (Verdict::Pass(w), Ok(())) => Verdict::Pass(format!("{said}; W1's judge: {w}")),
    }
}

/// What W2's records say of the hidden notice (the words, each with its time),
/// and whether they show the remembered wall driven ([`judge_wall_hidden`]).
fn hidden_facts(
    seen: &Seen,
    land: Option<&(i64, String)>,
    t0: i64,
) -> (String, Result<(), String>) {
    let Some((_, reset_s)) = seen.cover.wall else {
        return (
            "no wall".to_string(),
            Err("the stub's wall was never raised".to_string()),
        );
    };
    let reset = reset_s * 1000;
    let Some(h) = seen.cover.hidden.as_ref() else {
        let why = seen.cover.hide_gave_up.as_deref().map_or_else(
            || "the notice never left the screen".to_string(),
            |w| format!("the notice never left the screen ({w})"),
        );
        return ("the notice stood".to_string(), Err(why));
    };
    let seq_of = |l: &str| word(l, "seq").and_then(|s| s.parse::<u64>().ok());
    let on_hidden = |t: i64, l: &str| {
        t > h.enter_ms && h.seq.is_none_or(|hs| seq_of(l).is_some_and(|s| s >= hs))
    };
    let without_notice = |l: &str| l.starts_with("EVENT ") && !l.starts_with("EVENT limited");
    let read = seen
        .journal
        .iter()
        .find(|(t, l)| *t > h.first_ms && *t < reset && without_notice(l));
    let waits: Vec<String> = seen
        .journal
        .iter()
        .filter(|(t, l)| *t > h.enter_ms && l.starts_with("WAITING "))
        .take(4)
        .map(|(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 120)))
        .collect();
    let mut said = format!(
        "the notice out of sight at +{} ({} {} from +{}, the hiding Enter at +{}, screen seq={}), {} before the reset",
        at(t0, h.seen_ms),
        h.commands,
        h.command,
        at(t0, h.first_ms),
        at(t0, h.enter_ms),
        h.seq.map_or_else(|| "?".to_string(), |s| s.to_string()),
        at(0, reset - h.seen_ms)
    );
    match read {
        Some((t, l)) => said.push_str(&format!(
            "; read +{} {}",
            at(t0, *t),
            crate::util::one_line(l, 100)
        )),
        None => said.push_str("; NO point read without the wall before the reset"),
    }
    if !waits.is_empty() {
        said.push_str(&format!("; waits after: {}", waits.join(" | ")));
    }
    if h.seen_ms >= reset {
        return (
            said,
            Err(format!(
                "it left at +{}, not before the reset at +{}",
                at(t0, h.seen_ms),
                at(t0, reset)
            )),
        );
    }
    if land.is_none_or(|(t1, _)| h.first_ms <= *t1) {
        return (
            said,
            Err("it left before the land act met the wall".to_string()),
        );
    }
    if read.is_none() {
        return (
            said,
            Err("no point was read on a screen without the wall before the reset".to_string()),
        );
    }
    // The land check answers the agent's own defense, never the remembered wall (§14.17 item 5).
    let Some((ta, la)) = seen
        .all_acts()
        .into_iter()
        .find(|(t, l)| *t >= reset && act_rule(l) != Some(turn_end::RULE_LAND_CHECK))
        .map(|(t, l)| (*t, l.as_str()))
    else {
        return (
            said,
            Err(
                "nothing typed past the reset: no resume decided from the remembered wall"
                    .to_string(),
            ),
        );
    };
    said.push_str(&format!(
        "; past the reset {} typed at +{} (seq={})",
        act_rule(la).unwrap_or("?"),
        at(t0, ta),
        seq_of(la).map_or_else(|| "?".to_string(), |s| s.to_string())
    ));
    if !on_hidden(ta, la) {
        return (
            said,
            Err(
                "the act past the reset was typed at a screen from before the notice left"
                    .to_string(),
            ),
        );
    }
    (said, Ok(()))
}

/// Judges F18 live: a person's prompt answered at once mid-chain, under Cruise. Cruise never
/// parks (it replaced an older want, `LEFT` to the person and
/// nothing typed): the reply is a turn end like any other, judged at a point of its
/// own on the short back-off ladder. The person's turn adds no short answer of the agent's (v6
/// §14.17.5.7: the streak counts only the agent's own), so that back-off names the streak the
/// one standing at the person's Enter named; nothing reaches their exchange before its due (no
/// act, and no harness text at the stub: [`Cover::window_stub`], read to the due); no `LEFT`,
/// no `task=theirs`, no end and no need; and the work goes on at the due. P4C's chain is its
/// negative control: the same prompt typed by the control socket is the conversation's own turn,
/// which lengthens the streak.
pub(super) fn judge_person_instant(seen: &Seen, t0: i64) -> Verdict {
    let Some(asked) = seen.asked_ms else {
        return Verdict::Unobserved(match seen.ask_refused_ms {
            Some(t) => format!(
                "the person's prompt typed at +{} was not taken by the composer",
                at(t0, t)
            ),
            None => "the person's prompt was never typed (the done check's defense never decided)"
                .into(),
        });
    };
    let stamp = seen.cover.ask_stamp.as_deref().unwrap_or("?");
    if stamp.parse::<u64>().is_err() {
        return Verdict::Unobserved(format!(
            "no person's hand reached the session: status human_ms={stamp} after the Enter at +{} \
             (the `as=person` seam did not stamp it), so the prompt was nobody's",
            at(t0, asked)
        ));
    }
    let head: String = ASK_REPLY.chars().take(24).collect();
    let read = seen.journal.iter().find(|(t, l)| {
        *t > asked && event_words(l).is_some_and(|(_, w)| w.contains(head.as_str()))
    });
    let left = seen
        .journal
        .iter()
        .find(|(t, l)| *t > asked && l.starts_with("LEFT "));
    let decided = own_backoff_after(seen, asked);
    let due = decided.and_then(|(_, l)| backoff_due(l));
    let judged_by = decided.map_or(seen.end_ms, |(t, _)| *t);
    let ran = seen.ledger.iter().find(|r| {
        r.rule == aterm_agent::supervise::approvals::RULE_TURN_RAN
            && r.ts > asked
            && r.ts <= judged_by
    });
    if let Some(r) = ran {
        return Verdict::Unobserved(format!(
            "the person's Enter at +{}: a read saw the turn run (+{} {}), so it was no instant turn",
            at(t0, asked),
            at(t0, r.ts),
            r.rule
        ));
    }
    let standing = seen
        .journal
        .iter()
        .rfind(|(t, l)| *t <= asked && backoff_named(l))
        .and_then(|(_, l)| backoff_streak(l));
    let theirs = seen
        .status
        .iter()
        .find(|(t, s)| *t >= asked && word(s, "task").as_deref() == Some("theirs"));
    let next = seen
        .all_acts()
        .into_iter()
        .find(|(t, _)| *t > asked)
        .map(|(t, l)| (*t, l.as_str()));
    let stub = seen.cover.window_stub.as_ref();
    let mut said = vec![
        format!(
            "the person's Enter at +{} (status human_ms={stamp})",
            at(t0, asked)
        ),
        read.map_or_else(
            || "its reply's words never read".to_string(),
            |(t, l)| {
                format!(
                    "its reply read at +{} ({})",
                    at(t0, *t),
                    crate::util::one_line(l, 80)
                )
            },
        ),
    ];
    let mut bad = Vec::new();
    if let Some((tl, ll)) = left {
        bad.push(format!(
            "LEFT to the person at +{} ({}): Cruise parks no exchange",
            at(t0, *tl),
            crate::util::one_line(ll, 90)
        ));
    }
    if let Some((t, _)) = theirs {
        bad.push(format!("status said task=theirs at +{}", at(t0, *t)));
    }
    if let Some((d, _)) = seen.first_after("DONE ", asked) {
        bad.push(format!(
            "a DONE at +{} after the person's prompt",
            at(t0, *d)
        ));
    }
    if let Some(n) = seen.needs_seen() {
        bad.push(format!("a need was raised, though no reply named one: {n}"));
    }
    match (decided, due) {
        (Some((tw, wl)), Some(due)) => {
            said.push(format!(
                "its back-off decided at +{} ({}), due +{}",
                at(t0, *tw),
                crate::util::one_line(wl, 110),
                at(t0, due)
            ));
            let streak = backoff_streak(wl);
            if streak != standing {
                bad.push(format!(
                    "ITS BACK-OFF NAMES {} SHORT TURN(S), the one standing at the Enter {}: the \
                     person's turn counted as the agent's short answer",
                    streak.map_or_else(|| "?".to_string(), |n| n.to_string()),
                    standing.map_or_else(|| "none".to_string(), |n| n.to_string())
                ));
            }
            match next {
                Some((ta, la)) if ta < due - STEP_SLACK_MS => bad.push(format!(
                    "TYPED into the person's exchange at +{} ({}), before its back-off's due at +{}",
                    at(t0, ta),
                    act_rule(la).unwrap_or("?"),
                    at(t0, due)
                )),
                Some((ta, la)) if ta > due + PERSISTENCE_GRACE_MS => bad.push(format!(
                    "{} typed at +{}, {} past its back-off's due",
                    act_rule(la).unwrap_or("?"),
                    at(t0, ta),
                    at(0, ta - due)
                )),
                Some((ta, la)) => said.push(format!(
                    "{} typed at +{}, at its due",
                    act_rule(la).unwrap_or("?"),
                    at(t0, ta)
                )),
                None => {
                    if let Some(lost) = lost_after(seen, asked, t0) {
                        bad.push(lost);
                    }
                }
            }
        }
        _ => {
            if let Some((ta, la)) = next {
                bad.push(format!(
                    "TYPED into the person's exchange at +{} ({}) with no back-off decided at its \
                     reply",
                    at(t0, ta),
                    act_rule(la).unwrap_or("?")
                ));
            } else if let Some(lost) = lost_after(seen, asked, t0) {
                bad.push(lost);
            }
        }
    }
    if let Some(s) = stub.filter(|s| s.new_turns > 0) {
        bad.push(format!(
            "the stub saw {} request(s) holding a harness text before the back-off's due",
            s.new_turns
        ));
    }
    let said = said.join("; ");
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if read.is_none() {
        return Verdict::Unobserved(format!(
            "{said}: no read saw the person's reply after the Enter"
        ));
    }
    if decided.is_none() {
        return Verdict::Unobserved(format!(
            "{said}: no back-off decided at its reply within {}",
            at(t0, seen.end_ms)
        ));
    }
    if next.is_none() {
        return Verdict::Unobserved(format!(
            "{said}: watched to +{}, the act at its due not observed",
            at(t0, seen.end_ms)
        ));
    }
    match stub {
        None => Verdict::Unobserved(format!(
            "{said}: the stub's dumps before the due were not read"
        )),
        Some(s) => Verdict::Pass(format!(
            "{said}; the stub read {} request(s) before the due, none with a harness text: judged \
             on the ladder, the person's turn no short answer, the work went on",
            s.read
        )),
    }
}

/// Whether the lab's resize took ([`Cover::resized`]): `dims` after it
/// answered the size asked (`OK <rows> <cols> …`).
fn resize_took(how: &str) -> bool {
    let (cols, rows) = RESIZED;
    how.contains(&format!("dims OK {rows} {cols} "))
}

/// Judges F1 and F2 live: a judged turn's opener re-wrapped into sight without submission.
pub(super) fn judge_rewrap(seen: &Seen, t0: i64) -> Verdict {
    let Some((tr, how)) = seen.cover.resized.as_ref() else {
        return Verdict::Unobserved(
            "the tab was never resized (its first point's back-off never decided, or an act came first)"
                .into(),
        );
    };
    let tr = *tr;
    if !resize_took(how) {
        return Verdict::Unobserved(format!(
            "the resize did not take: {}",
            crate::util::one_line(how, 200)
        ));
    }
    let Some((tf, fl)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > t0 && *t < tr && backoff_named(l))
    else {
        return Verdict::Unobserved("no back-off decided before the resize".into());
    };
    let until = seen.cover.turn_ms.unwrap_or(seen.end_ms);
    let w = not_judged_again(seen, t0, (tr, until), fl, "the resize");
    let said = format!(
        "the first point's back-off +{} ({}); resized at +{} ({}); {} read(s) journaled in the {} to the orchestrator's turn",
        at(t0, *tf),
        crate::util::one_line(fl, 100),
        at(t0, tr),
        crate::util::one_line(how, 120),
        w.reads,
        at(0, until - tr)
    );
    if !w.bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", w.bad.join("; ")));
    }
    if w.read_points.is_empty() {
        return Verdict::Unobserved(format!(
            "{said}: no point read after the resize and before the orchestrator's turn"
        ));
    }
    if until - tr < 20_000 {
        return Verdict::Unobserved(format!("{said}: watched under 20 s after the resize"));
    }
    Verdict::Pass(format!(
        "{said}: nothing judged again, nothing typed, the first point's back-off told again at each of the {} point(s) read",
        w.read_points.len()
    ))
}

/// What the harness did between a drive of the screen and the end of its
/// watch ([`not_judged_again`]).
struct AfterDrive {
    /// The readings journaled there (`EVENT …`).
    reads: usize,
    /// When each point was read there (`EVENT idle`/`question`).
    read_points: Vec<i64>,
    /// What says the judged point was judged again, or its back-off moved
    /// or dropped.
    bad: Vec<String>,
}

/// Checks `(from, until)` for re-judging, typed acts, or dropped back-off after a screen drive.
fn not_judged_again(
    seen: &Seen,
    t0: i64,
    (from, until): (i64, i64),
    fl: &str,
    what: &str,
) -> AfterDrive {
    let first_due = backoff_due(fl);
    let inside = |t: i64| t > from && t < until;
    let reads = seen
        .journal
        .iter()
        .filter(|(t, l)| inside(*t) && l.starts_with("EVENT "))
        .count();
    let points: Vec<&Row> = seen
        .ledger
        .iter()
        .filter(|r| inside(r.ts) && r.rule == RULE_POINT)
        .collect();
    let acts: Vec<&(i64, String)> = seen
        .all_acts()
        .into_iter()
        .filter(|(t, _)| inside(*t))
        .collect();
    let moved: Vec<&(i64, String)> = seen
        .journal
        .iter()
        .filter(|(t, l)| {
            inside(*t)
                && backoff_named(l)
                && (backoff_streak(l) != Some(1) || first_due.is_none() || !same_backoff(l, fl))
        })
        .collect();
    let read_points: Vec<i64> = seen
        .journal
        .iter()
        .filter(|(t, l)| {
            inside(*t) && (l.starts_with("EVENT idle ") || l.starts_with("EVENT question "))
        })
        .map(|(t, _)| *t)
        .collect();
    let dropped = read_points.iter().enumerate().find_map(|(i, &te)| {
        let next = read_points.get(i + 1).copied().unwrap_or(until);
        let told = seen
            .journal
            .iter()
            .any(|(t, l)| *t >= te && *t < next && same_backoff(l, fl));
        (!told)
            .then(|| {
                seen.journal
                    .iter()
                    .find(|(t, l)| *t == te && l.starts_with("EVENT "))
                    .map_or_else(String::new, |(_, l)| crate::util::one_line(l, 90))
            })
            .map(|line| (te, next, line))
    });
    let mut bad = Vec::new();
    if let Some(r) = points.first() {
        bad.push(format!(
            "JUDGED AGAIN: {} point record(s) after {what}, the first +{} ({})",
            points.len(),
            at(t0, r.ts),
            crate::util::one_line(&r.reason, 90)
        ));
    }
    if let Some((t, l)) = acts.first() {
        bad.push(format!(
            "{} typed at +{} with nothing submitted",
            act_rule(l).unwrap_or("?"),
            at(t0, *t)
        ));
    }
    if let Some((t, l)) = moved.first() {
        bad.push(format!(
            "a back-off of another point at +{} ({})",
            at(t0, *t),
            crate::util::one_line(l, 110)
        ));
    }
    if let Some((te, next, line)) = &dropped {
        bad.push(format!(
            "THE FIRST POINT'S BACK-OFF DROPPED: the point read at +{te_at} ({line}) told it not again by +{next_at} — no due standing, nothing to type at it",
            te_at = at(t0, *te),
            next_at = at(t0, *next),
        ));
    }
    AfterDrive {
        reads,
        read_points,
        bad,
    }
}

/// Judges F1 live by scroll: a judged turn's opener scrolled into sight from history without submission.
pub(super) fn judge_scrolled(seen: &Seen, t0: i64) -> Verdict {
    let Some(sc) = seen.cover.scrolled.as_ref() else {
        return Verdict::Unobserved(
            "the wheel was never turned (its first point's back-off never decided, or an act came first)"
                .into(),
        );
    };
    let how = format!(
        "{} notch(es) up from +{} to +{} ({}; lines {}){}",
        sc.notches,
        at(t0, sc.from),
        at(t0, sc.at),
        sc.modes,
        sc.history,
        sc.refused
            .as_deref()
            .map_or_else(String::new, |r| format!(", refused: {r}"))
    );
    if sc.before {
        return Verdict::Unobserved(format!(
            "{how}: the opener was in sight before the wheel (the reply did not push it into the history)"
        ));
    }
    if let Some(vp) = sc.viewport.as_deref() {
        let again = seen
            .journal
            .iter()
            .find(|(t, l)| *t > t0 && *t < sc.from && backoff_named(l))
            .map(|(_, fl)| not_judged_again(seen, t0, (sc.from, seen.end_ms), fl, "the scroll"));
        if let Some(w) = again.filter(|w| !w.bad.is_empty()) {
            return Verdict::Fail(format!("{how}: {}", w.bad.join("; ")));
        }
        return Verdict::Unobserved(format!(
            "{how}: the agent takes no wheel, so it scrolled the tab's viewport ({vp}), the opener {} there, and the live screen {}: the harness reads the live screen (`text --json`), which no viewport's scroll moves, so an opener scrolled into a person's sight there is out of the harness's on this renderer",
            if sc.after { "in sight" } else { "not in sight" },
            if sc.moved {
                "moved (the agent's own drawing)"
            } else {
                "unmoved"
            }
        ));
    }
    if !sc.after {
        return Verdict::Unobserved(format!(
            "{how}: the wheel brought the opener into sight at no notch; the screen's last rows: {}",
            last_rows(&sc.rows, 4)
        ));
    }
    let Some((tf, fl)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > t0 && *t < sc.from && backoff_named(l))
    else {
        return Verdict::Unobserved(format!("{how}: no back-off decided before the wheel"));
    };
    let until = seen.end_ms;
    let w = not_judged_again(seen, t0, (sc.from, until), fl, "the scroll");
    let in_sight = w.read_points.iter().filter(|&&t| t > sc.at).count();
    let held = match sc.held {
        Some(true) => ", still in sight 10 s later",
        Some(false) => ", out of sight 10 s later",
        None => "",
    };
    let said = format!(
        "the first point's back-off +{} ({}); {how}: the opener in sight{held}; {} read(s) journaled from the first notch, in the {} watched after it",
        at(t0, *tf),
        crate::util::one_line(fl, 100),
        w.reads,
        at(0, until - sc.from)
    );
    if !w.bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", w.bad.join("; ")));
    }
    if in_sight == 0 {
        return Verdict::Unobserved(format!(
            "{said}: no point read on the scrolled screen, whose last rows were {}",
            last_rows(&sc.rows, 4)
        ));
    }
    if until - sc.at < 20_000 {
        return Verdict::Unobserved(format!("{said}: watched under 20 s after the scroll"));
    }
    Verdict::Pass(format!(
        "{said}: nothing judged again, nothing typed, the first point's back-off told again at each of the {} point(s) read ({in_sight} after the last notch)",
        w.read_points.len()
    ))
}

/// The last `n` of `rows`, joined with ` / `, each cut to 80 characters.
fn last_rows(rows: &[String], n: usize) -> String {
    rows[rows.len().saturating_sub(n)..]
        .iter()
        .map(|r| crate::util::one_line(r, 80))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Judges F1/T1: an orchestrator turn ([`Cover::turn_ms`]) reusing the same opener words.
pub(super) fn judge_same_words(seen: &Seen, t0: i64) -> Verdict {
    let Some(tt) = seen.cover.turn_ms else {
        return Verdict::Unobserved("the orchestrator's turn was never typed".into());
    };
    let Some((tf, fl)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > t0 && *t < tt && backoff_named(l))
    else {
        return Verdict::Unobserved("no back-off decided before the orchestrator's turn".into());
    };
    let old = format!(
        "the first point's back-off +{} ({})",
        at(t0, *tf),
        crate::util::one_line(fl, 100)
    );
    let (went, act) = match judged_resumption(seen, tt, WORK_KEY, t0) {
        Ok(ok) => ok,
        Err(v) => {
            return said_before(
                &format!("{old}; the orchestrator's turn at +{}", at(t0, tt)),
                v,
            );
        }
    };
    let said = format!("{old}; the orchestrator's turn at +{}: {went}", at(t0, tt));
    let Some((tw, wl)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > tt && *t < act.0 && backoff_named(l))
    else {
        return Verdict::Fail(format!("{said}; but no back-off decided after the turn"));
    };
    let mut bad = Vec::new();
    let want = backoff_streak(fl).map(|n| n + 1);
    if backoff_streak(wl) != want || want.is_none() {
        bad.push(format!(
            "its back-off +{} names {} short turn(s), not {} (the turn taken for the one before it)",
            at(t0, *tw),
            backoff_streak(wl).map_or_else(|| "?".to_string(), |n| n.to_string()),
            want.map_or_else(|| "?".to_string(), |n| n.to_string())
        ));
    }
    match (backoff_due(wl), backoff_due(fl)) {
        (Some(new), Some(old_due)) if new > old_due + 1000 => {}
        (new, old_due) => bad.push(format!(
            "its back-off is due at {}, the first point's at {}: no later",
            new.map_or_else(|| "?".to_string(), |d| format!("+{}", at(t0, d))),
            old_due.map_or_else(|| "?".to_string(), |d| format!("+{}", at(t0, d)))
        )),
    }
    if bad.is_empty() {
        Verdict::Pass(format!(
            "{said}; its own point: +{} {}",
            at(t0, *tw),
            crate::util::one_line(wl, 110)
        ))
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// How long after the stub's question reaches the screen its answer may take where no person
/// types: a one-minute hang detector, never a latency budget.
const DIALOG_ANSWER_MS: i64 = 60_000;

/// L20 (`land`) and C5: the first turn's question dialog ([`dialog_call`]). Its answer rows are
/// the Autopilot's (`answer-land@v1` under Land, `answer-recommended@v1` under Cruise), approved,
/// none of the other's and none handed over, the first within [`DIALOG_ANSWER_MS`] of the
/// question's reply; the tool result the agent sent back carries the land policy text under Land
/// and the Recommended option under Cruise, and no need is raised. Under Land the chain then lands
/// through the land check ([`judge_clean_landing`]).
pub(super) fn judge_dialog(seen: &Seen, land: bool, t0: i64) -> Verdict {
    use aterm_agent::supervise::policy::question::LAND_TEXT;
    use aterm_agent::supervise::policy::{RULE_ANSWER_LAND, RULE_ANSWER_RECOMMENDED};
    let (rule, other, carried) = if land {
        (RULE_ANSWER_LAND, RULE_ANSWER_RECOMMENDED, LAND_TEXT)
    } else {
        (
            RULE_ANSWER_RECOMMENDED,
            RULE_ANSWER_LAND,
            DIALOG_NEW_WORK.trim_end_matches(" (Recommended)"),
        )
    };
    let rows: Vec<&Row> = seen
        .ledger
        .iter()
        .filter(|r| r.ts >= t0 && r.rule.starts_with("answer-"))
        .collect();
    if let Some(r) = rows
        .iter()
        .find(|r| matches!(r.decision.as_str(), "escalated" | "deferred"))
    {
        return Verdict::Fail(format!(
            "the dialog was handed over at +{} ({} {}): {}",
            at(t0, r.ts),
            r.rule,
            r.decision,
            crate::util::one_line(&r.reason, 200)
        ));
    }
    if let Some(r) = rows.iter().find(|r| r.rule == other) {
        return Verdict::Fail(format!(
            "the dialog was answered under {other} at +{}, not {rule}",
            at(t0, r.ts)
        ));
    }
    let Some(first) = rows
        .iter()
        .find(|r| r.rule == rule && r.decision == "approved")
    else {
        return Verdict::Unobserved(format!(
            "no {rule} row approved within {}",
            at(t0, seen.end_ms)
        ));
    };
    let asked = t0 + Chain::LandDialog.first_reply_ms();
    if first.ts - asked > DIALOG_ANSWER_MS {
        return Verdict::Fail(format!(
            "{rule} first approved at +{}, more than {} s after the question's reply (+{})",
            at(t0, first.ts),
            DIALOG_ANSWER_MS / 1000,
            at(t0, asked)
        ));
    }
    let Some(answers) = seen.cover.dialog_answers.as_ref() else {
        return Verdict::Unobserved("the stub's dumps were not read".into());
    };
    let head: String = carried.chars().take(60).collect();
    if !answers.iter().any(|a| a.contains(head.as_str())) {
        return Verdict::Fail(format!(
            "no tool result carried {head:?} back: {}",
            answers
                .iter()
                .map(|a| crate::util::one_line(a, 160))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if let Some((t, m)) = seen.meta.iter().find(|(t, m)| *t >= t0 && needs_up(m)) {
        return Verdict::Fail(format!(
            "a need was raised at +{}: {}",
            at(t0, *t),
            crate::util::one_line(m, 160)
        ));
    }
    let said = format!(
        "{} {rule} row(s) approved from +{}, the tool result carrying {head:?}",
        rows.iter().filter(|r| r.decision == "approved").count(),
        at(t0, first.ts)
    );
    if !land {
        return Verdict::Pass(said);
    }
    let land_act = seen.acts(turn_end::RULE_LAND).first().copied();
    compose(
        &said,
        &[("landed", judge_clean_landing(seen, land_act, t0))],
    )
}

/// The head of every instruction the harness types (`[aterm harness] Autopilot is Land: …`), as a
/// program's transcript echoes it.
const TYPED_HEAD: &str = "[aterm harness]";

/// F3's first turn on the screen read at its first point ([`Cover::first_screen`]): the rows under
/// the opener's echo (the row holding `token`, [`Chain::token`]) up to the first echo of an
/// instruction the harness typed, or else to the composer (its top edge, the screen's last rule row
/// but one), with blank rows, rule rows (agy draws one under each turn) and a program's own
/// furniture ([`turn_furniture`]) dropped; `None` where that screen holds no echo of the opener.
pub(super) fn first_turn<'a>(rows: &'a [String], token: &str) -> Option<Vec<&'a str>> {
    let opener = rows.iter().position(|r| r.contains(token))?;
    let rules: Vec<usize> = (0..rows.len()).filter(|&i| is_rule_row(&rows[i])).collect();
    let composer = rules
        .len()
        .checked_sub(2)
        .map_or(rows.len(), |i| rules[i])
        .max(opener + 1);
    Some(
        rows[opener + 1..composer]
            .iter()
            .map(String::as_str)
            .take_while(|r| !r.contains(TYPED_HEAD))
            .filter(|r| !r.trim().is_empty() && !is_rule_row(r) && !turn_furniture(r))
            .collect(),
    )
}

/// A row of box-drawing alone (a composer's or a turn's edge), a label set in it allowed (Muse
/// Code's `── Voice input (⌥ + v to start) ───…`).
fn is_rule_row(row: &str) -> bool {
    let row = row.trim();
    row.starts_with("──") && row.ends_with("──")
}

/// A program's own row under a turn, no words of the agent's: Claude Code's turn timer (`✻ Worked
/// for 0s · done 9:56 PM`).
fn turn_furniture(row: &str) -> bool {
    row.trim_start().starts_with('✻')
}

/// Whether `row` is F3's tool's ([`read_call`]: a read of the chain's `README.md`) as each program
/// drew it on 2026-11-05: Claude Code 2.1.318's `Read 1 file (ctrl+o to expand)`, agy 1.5.1's `●
/// Explored 1 file (README.md) (ctrl+o to expand)` and Muse Code 1.4.14's `Read README.md`.
fn is_tool_row(row: &str) -> bool {
    row.contains("README.md") || row.contains("Read 1 file")
}

/// F3's premise read off its first turn ([`first_turn`]): the turn ended on its tool's row, no
/// words after it, so the point judged is a tool-ended one; else why not.
fn tool_ended_premise(seen: &Seen, t0: i64) -> Result<String, String> {
    let Some((t, rows)) = seen.cover.first_screen.as_ref() else {
        return Err("the screen at the first point not read".into());
    };
    let Some(turn) = first_turn(rows, &Chain::ToolEnd.token()) else {
        return Err(format!(
            "the screen read at +{} holds no echo of the opener ({} rows, the last {})",
            at(t0, *t),
            rows.len(),
            rows.last().map_or("-", |r| r.trim())
        ));
    };
    match turn.last() {
        Some(last) if is_tool_row(last) => Ok(format!(
            "the first turn's last row, read at +{}: {}",
            at(t0, *t),
            crate::util::one_line(last.trim(), 80)
        )),
        _ => Err(format!(
            "the first turn did not end on the tool's row: its rows read at +{}: {}",
            at(t0, *t),
            if turn.is_empty() {
                "none".to_string()
            } else {
                turn.iter()
                    .map(|r| crate::util::one_line(r.trim(), 80))
                    .collect::<Vec<_>>()
                    .join(" / ")
            }
        )),
    }
}

/// Judges F3 live: a first reply ending on a read-only tool box, judged at that point: a back-off
/// naming one short turn, or Land's instruction typed there at once (v6 §14.10), then landed.
/// UNOBSERVED where the screen read at the first point does not show the first turn ending on the
/// tool's row ([`tool_ended_premise`]): the point judged was then not a tool-ended one.
pub(super) fn judge_tool_end(seen: &Seen, land: Option<&(i64, String)>, t0: i64) -> Verdict {
    let results = seen.cover.tool_results.unwrap_or(0);
    let premise = tool_ended_premise(seen, t0);
    let screen = match &premise {
        Ok(s) | Err(s) => s.clone(),
    };
    if results == 0 {
        return Verdict::Unobserved(format!(
            "no request carried the tool's result back: the agent never ran F3's call, so its turn did not end on a tool's row ({screen})"
        ));
    }
    if premise.is_err() {
        return Verdict::Unobserved(format!(
            "the tool's result carried back in {results} request(s), but {screen}"
        ));
    }
    let point = seen
        .ledger
        .iter()
        .find(|r| r.rule == RULE_POINT && r.ts >= t0);
    let first_act = seen.all_acts().first().map(|(t, _)| *t);
    let backoff = seen
        .journal
        .iter()
        .find(|(t, l)| *t > t0 && first_act.is_none_or(|a| *t < a) && backoff_named(l));
    // The decision at the tool-ended point: its back-off, else Land's instruction typed at that
    // point at once (v6 §14.10, as L1 reads it): the land act of the first point's seq, its first
    // act, within [`switch::AT_ONCE_MS`] of the point.
    let idle = seen
        .journal
        .iter()
        .find(|(t, l)| *t > t0 && l.starts_with("EVENT idle "));
    let at_point = land.filter(|(ta, l)| {
        first_act == Some(*ta)
            && point.is_some_and(|r| *ta - r.ts <= switch::AT_ONCE_MS)
            && idle.is_some_and(|(te, el)| te <= ta && word(el, "seq") == word(l, "seq"))
    });
    let decided = backoff.or(at_point);
    let said = format!(
        "the tool's result carried back in {results} request(s); {}; {}; {screen}",
        point.map_or_else(
            || "no point record".to_string(),
            |r| format!(
                "the first point +{} ({})",
                at(t0, r.ts),
                crate::util::one_line(&r.reason, 70)
            )
        ),
        decided.map_or_else(
            || "no back-off".to_string(),
            |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 100))
        ),
    );
    let Some((tw, wl)) = decided else {
        return match (first_act, lost_after(seen, t0, t0)) {
            (Some(a), _) => Verdict::Fail(format!(
                "{said}: an act typed at +{} with no back-off decided before it, nor land@v1 typed at the first point within {} s",
                at(t0, a),
                switch::AT_ONCE_MS / 1000
            )),
            (None, Some(lost)) => {
                Verdict::Fail(format!("{said}: THE TOOL-ENDED TURN NEVER JUDGED; {lost}"))
            }
            (None, None) => Verdict::Unobserved(format!(
                "{said}: nothing judged within {}",
                at(t0, seen.end_ms)
            )),
        };
    };
    if backoff.is_some() && backoff_streak(wl) != Some(1) {
        return Verdict::Fail(format!(
            "{said}: the first back-off +{} names {} short turn(s), not 1",
            at(t0, *tw),
            backoff_streak(wl).map_or_else(|| "?".to_string(), |n| n.to_string())
        ));
    }
    let Some((ta, la)) = land else {
        if let Some(a) = first_act {
            return Verdict::Fail(format!(
                "{said}: an act typed at +{}, not Land's",
                at(t0, a)
            ));
        }
        return match lost_after(seen, t0, t0) {
            Some(lost) => Verdict::Fail(format!("{said}: {lost}")),
            None => Verdict::Unobserved(format!(
                "{said}: no land act within {}",
                at(t0, seen.end_ms)
            )),
        };
    };
    if first_act != Some(*ta) {
        return Verdict::Fail(format!(
            "{said}: {} typed before the land act",
            first_act.map_or_else(|| "?".to_string(), |a| format!("an act at +{}", at(t0, a)))
        ));
    }
    let acted = format!(
        "{said}; {} typed at +{}",
        act_rule(la).unwrap_or("?"),
        at(t0, *ta)
    );
    match seen.first_after("DONE ", *ta) {
        Some((d, l)) => Verdict::Pass(format!(
            "{acted}; landed at +{} ({})",
            at(t0, *d),
            crate::util::one_line(l, 80)
        )),
        None if seen.end_ms - ta >= TOOL_END_LANDS_MS => Verdict::Fail(format!(
            "{acted}: ITS CLEAN DEFENSE LANDED NOTHING — no DONE in the {} watched after it",
            at(0, seen.end_ms - ta)
        )),
        None => Verdict::Unobserved(format!(
            "{acted}: watched {} after it, under {} s, and not landed yet",
            at(0, seen.end_ms - ta),
            TOOL_END_LANDS_MS / 1000
        )),
    }
}

/// How long after `tool-end`'s land act its clean defense, answered at once
/// by the stub, must have landed the task ([`judge_tool_end`]): the chain's
/// watch runs 30 s past the act ([`finished`]).
const TOOL_END_LANDS_MS: i64 = 25_000;

/// Judges `--mode ladder`: short-turn back-off past its fourth step.
pub(super) fn judge_ladder(seen: &Seen, scale: f64, t0: i64) -> Verdict {
    let steps = ladder_steps(seen, t0);
    let shipped_first = ladder_step_ms(1, 1.0);
    let measured = |st: &Step| st.due - st.at;
    let said = steps
        .iter()
        .map(|st| format!("{}: {}", st.streak, at(0, measured(st))))
        .collect::<Vec<_>>()
        .join(", ");
    let said = format!("steps (short turns: until less its decision) [{said}]");
    if let Some(first) = steps.first()
        && (measured(first) - shipped_first).abs() <= STEP_SLACK_MS
        && scale < 1.0
    {
        return Verdict::Fail(format!(
            "{said}: the first step is the shipped {} s, the seam not taken",
            shipped_first / 1000
        ));
    }
    let acts = seen.all_acts();
    let mut bad = Vec::new();
    for (i, st) in steps.iter().take(LADDER_STEPS as usize).enumerate() {
        let n = u32::try_from(i + 1).unwrap_or(u32::MAX);
        if st.streak != n {
            bad.push(format!(
                "step {n} at +{} names {} short turn(s)",
                at(t0, st.at),
                st.streak
            ));
            continue;
        }
        let want = ladder_step_ms(n, scale);
        if (measured(st) - want).abs() > STEP_SLACK_MS {
            bad.push(format!(
                "step {n} at +{} waits {} ms, not {want} ms ({})",
                at(t0, st.at),
                measured(st),
                crate::util::one_line(&st.line, 100)
            ));
        }
        let next = steps.get(i + 1).map_or(seen.end_ms, |s| s.at);
        match acts.iter().find(|(t, _)| *t > st.at && *t < next) {
            Some((ta, la)) if *ta > st.due + PERSISTENCE_GRACE_MS => bad.push(format!(
                "after step {n} {} typed at +{}, {} past its due",
                act_rule(la).unwrap_or("?"),
                at(t0, *ta),
                at(0, ta - st.due)
            )),
            Some(_) => {}
            None if steps.get(i + 1).is_some() => bad.push(format!(
                "NOTHING TYPED after step {n} (+{}, due +{}) before step {} at +{}: a point judged again, no act",
                at(t0, st.at),
                at(t0, st.due),
                n + 1,
                at(t0, next)
            )),
            None => {}
        }
    }
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if steps.len() < LADDER_STEPS as usize {
        let since = acts.last().map_or(t0, |(t, _)| *t);
        return match lost_after(seen, since, t0) {
            Some(lost) => Verdict::Fail(format!("{said}: {lost}")),
            None => Verdict::Unobserved(format!(
                "{said}: {} of {LADDER_STEPS} steps decided within {}",
                steps.len(),
                at(t0, seen.end_ms)
            )),
        };
    }
    Verdict::Pass(format!(
        "{said}: doubled from {} s to the {} s cap and the cap again, each act at its due",
        ladder_step_ms(1, scale) / 1000,
        ladder_step_ms(LADDER_STEPS, scale) / 1000
    ))
}

#[cfg(test)]
#[path = "forever_cover_first_land_evidence_tests.rs"]
mod first_land_evidence_tests;
