// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JUDGES OF `lab forever`'s COVERAGE CHAINS (`--mode cover`, `--mode
//! cap`, `--mode wall`): what the measured pass left unobserved, each judged from the tab's
//! own records ([`Seen`]: journal, ledger, `status`, `meta`, the task record,
//! the stub's dumps) and what the run did to it ([`Cover`]), never the screen
//! alone. Each passes beside the negative controls its tests fail
//! (`forever_tests.rs`): a broken product's records fail it.

use super::*;
use aterm_agent::supervise::policy::approval::RULE_LIMIT_RETRY;

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

/// A NEEDS YOU need raised on the tab: a `NEEDS` line, or the needs owner's
/// `ask:needs` entry in `meta` — not `status needs=ask` alone, which the
/// supervisor's own escalation raises too (CAP's: `attention_kinds=
/// -:supervisor`, measured on Claude Code 2.1.304).
fn need_raised(seen: &Seen) -> Option<String> {
    if let Some((_, l)) = seen.journal.iter().find(|(_, l)| l.starts_with("NEEDS ")) {
        return Some(format!("journal {}", crate::util::one_line(l, 100)));
    }
    seen.meta
        .iter()
        .find(|(_, m)| needs_up(m))
        .map(|(_, m)| format!("meta {}", crate::util::one_line(m, 120)))
}

/// A task record the run read where none was given (`OK task -` throughout
/// is none).
fn invented_record(seen: &Seen) -> Option<&str> {
    seen.task
        .iter()
        .map(|(_, r)| r.trim())
        .find(|r| *r != "OK task -")
}

/// A verdict of the same word, `said` before its words.
fn said_before(said: &str, v: Verdict) -> Verdict {
    match v {
        Verdict::Pass(w) => Verdict::Pass(format!("{said}; {w}")),
        Verdict::Fail(w) => Verdict::Fail(format!("{said}; {w}")),
        Verdict::Unobserved(w) => Verdict::Unobserved(format!("{said}; {w}")),
    }
}

/// THE ORCHESTRATOR'S TURN JUDGED AND ACTED ON (L13, CAP; F8 of the review
/// of `b9/forever-gaps`, where any act after the turn passed): its reply's
/// point read after the turn at `tt` (an `EVENT` whose words hold `key`),
/// the back-off decided at THAT point (its `WAITING`, the same `seq=`), and
/// the first act after the turn typed only after that `WAITING`, at its due.
/// `Ok` with what it said and the act; `Err` with the verdict — FAIL for an
/// act typed at no such point or before its due, and for persistence lost
/// ([`lost_after`]); UNOBSERVED where the watch ended first.
fn judged_resumption<'a>(
    seen: &'a Seen,
    tt: i64,
    key: &str,
    t0: i64,
) -> Result<(String, &'a (i64, String)), Verdict> {
    let act = seen.all_acts().into_iter().find(|(t, _)| *t > tt);
    let before_act = |t: i64| act.is_none_or(|(ta, _)| t < *ta);
    let point = seen.journal.iter().find(|(t, l)| {
        *t > tt && before_act(*t) && event_words(l).is_some_and(|(_, w)| w.contains(key))
    });
    let waited = point.and_then(|(te, el)| {
        let seq = word(el, "seq");
        seen.journal
            .iter()
            .find(|(t, l)| *t >= *te && before_act(*t) && backoff_named(l) && word(l, "seq") == seq)
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

/// L12: a question in prose at the land act's yield. Its yield READ (an
/// `EVENT` line of the point holding the question: `idle`, or `question`
/// where the loop read it as one), the act at that point
/// `land-answer@v1` — never a nudge or a continuation, never a landing at the
/// question — journaled and ledgered, its text Land's answer with no suffix
/// (no open work, no repository, no needs carry-on: none stands), no need
/// raised; then the clean defense at THE ANSWER'S yield lands the task (a
/// land act's yield: `DONE … rule=land@v1`, nothing typed between), quiet
/// after it as L5's ([`landing_quiet`]), no task record invented.
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
    if let Some((tb, lb)) = acts
        .iter()
        .copied()
        .find(|(t, _)| *t > act.0 && *t < done.0)
    {
        return Verdict::Fail(format!(
            "{said}; but {} typed at +{} before the landing: it was not the answer's yield",
            act_rule(lb).unwrap_or("?"),
            at(t0, *tb)
        ));
    }
    let (mut bad, quiet, window) = landing_quiet(seen, done, false, t0);
    if let Some(r) = invented_record(seen) {
        bad.push(format!("a task record where none was given: {r}"));
    }
    landed_verdict(bad, format!("{said}; {quiet}"), seen, done.0, window)
}

/// L13: A PERSON'S OWN EXCHANGE — the first prompt typed as a person types
/// it (its draft standing, then their Enter at [`Cover::person_ms`]),
/// answered at once. Through the exchange (their Enter to the orchestrator's
/// turn, [`Cover::turn_ms`]): `LEFT … rule=left-to-person@v1` journaled,
/// `status task=theirs`, NOTHING TYPED (no journal act, no acting ledger row,
/// no harness text in the stub's dumps of it: [`Cover::window_stub`]), no
/// DONE, no need raised, for at least [`QUIET_UNDER_OFF_MS`] — past a 4 min
/// first step; then the orchestrator's turn ended the exchange, judged and
/// acted on ([`judged_resumption`]: its reply read, its back-off decided
/// there, Land's act at that due). Judged only where a person's hand reached the session
/// ([`Cover::person_stamp`] a stamp, not `-`): the lab's person types through
/// the development seam `send|key as=person`, or (`forever --window`) through
/// `hwkey`, the key press's own path ([`Cover::keyed`]), and a write that left
/// no stamp is nobody's by the product's rule, so what the harness did after
/// it says nothing of a person's exchange. Typed through `hwkey`, the stamp
/// must be the keys' and theirs alone ([`keys_unaccounted`]): `status
/// human_seq=` moved by exactly the keys posted, else UNOBSERVED.
pub(super) fn judge_theirs(seen: &Seen, t0: i64) -> Verdict {
    let Some(p) = seen.cover.person_ms else {
        return Verdict::Unobserved("the person's prompt was never typed".into());
    };
    let stamp = seen.cover.person_stamp.as_deref().unwrap_or("?");
    let path = if seen.cover.keyed.is_some() {
        "hwkey's keys"
    } else {
        "the `as=person` seam"
    };
    if stamp.parse::<u64>().is_err() {
        return Verdict::Unobserved(format!(
            "no person's hand reached the session: status human_ms={stamp} after the Enter at +{} \
             ({path} did not stamp it), so the prompt was nobody's",
            at(t0, p)
        ));
    }
    if let Some(why) = seen.cover.keyed.as_ref().and_then(keys_unaccounted) {
        return Verdict::Unobserved(format!(
            "{why} (the Enter at +{}), so whose hand the prompt was is not known",
            at(t0, p)
        ));
    }
    let typed = seen
        .cover
        .keyed
        .as_ref()
        .map_or_else(|| format!("typed through {path}"), keyed_line);
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

/// Why `hwkey`'s keys do not account for the person's stamp, if they do
/// not: `status human_seq=` (a person's gestures, which no control verb
/// moves) went from its count before the first key to its count after the
/// Enter by exactly the presses `hwkey` posted. Fewer: some key the real key
/// path took was not counted as a person's. More: another hand (a person at
/// the desk) reached the session too. Unread after: not known.
#[must_use]
pub fn keys_unaccounted(k: &Keyed) -> Option<String> {
    let Some(after) = k.seq_after else {
        return Some(format!(
            "hwkey posted {} key(s), and status human_seq= was not read after the Enter",
            k.posted
        ));
    };
    let moved = after.checked_sub(k.seq_before);
    if moved == Some(k.posted) && k.posted > 0 {
        return None;
    }
    let what = match moved {
        Some(n) if n < k.posted => "not every key the real key path took was counted as a person's",
        Some(n) if n > k.posted => "another hand reached the session beside hwkey's keys",
        Some(_) => "no key was posted",
        None => "the person's count went back",
    };
    Some(format!(
        "hwkey posted {} key(s), and status human_seq= went {} -> {after}: {what}",
        k.posted, k.seq_before
    ))
}

/// The line that says how `hwkey` typed the person's prompt.
#[must_use]
pub fn keyed_line(k: &Keyed) -> String {
    format!(
        "typed through hwkey: {} key(s) posted to the window's event queue ({} to the key window of the active app), status human_seq= {} -> {}",
        k.posted,
        k.key_window,
        k.seq_before,
        k.seq_after
            .map_or_else(|| "?".to_string(), |n| n.to_string())
    )
}

/// L14: P2 LIVE UNDER LAND — the work reply opening `Wrote the parser
/// module …` READ at the first point judged (the `EVENT` of the first
/// back-off's `seq=` holding it, before the land act: Muse Code's reader once
/// took such a head for its own tool row and read no words), and L1, L3 and
/// L5 on replies that each open `Wrote ` (the need and the defense read from
/// them, the landing).
pub(super) fn judge_wrote_land<'a>(
    seen: &'a Seen,
    land: Option<&(i64, String)>,
    nudge_after: &dyn Fn(i64) -> Option<&'a (i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let before = land.map_or(seen.end_ms, |(t, _)| *t);
    // THE FIRST POINT JUDGED read the reply's words (F15 of the review of
    // `b9/forever-gaps`): the point of the first back-off after the prompt,
    // not any line that read them, which a point judged as no words passed.
    let judged = seen
        .journal
        .iter()
        .find(|(t, l)| *t > t0 && *t < before && backoff_named(l));
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
            ("L3", judge_needs(seen, land, nudge_after, program, t0)),
            ("L5", judge_landed(seen, land, nudge_after, false, t0)),
        ],
    )
}

/// C3: P2 LIVE UNDER CRUISE — the done claim opening `Wrote the parser and
/// pushed …` READ before the first act, that act the done check (a claim
/// read as no words gets `continue@v1`), and C1 and C2 on it (the check's
/// defense, opening `Wrote `, read and waited on).
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

/// A task record's age (`since_ms=`, ms since its `since`).
fn age(record: &str) -> Option<i64> {
    word(record, "since_ms").and_then(|v| v.parse().ok())
}

/// P5: a worker's record given before its agent started (`task mark working
/// from=ctl`), its agent at its composer: read `working` at an age past its
/// startup deadline and [`PAST_DEADLINE_MS`] more — never `stalled` (the
/// deadline's `why=startup` included) nor another state before its `done`,
/// every change of the record read ([`rev_gaps`]: a stall between two looks
/// moves its rev by two),
/// no `status needs=` but `-`, no "never started" in the record's or the
/// supervisor's attention.
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
    // every change of the record read (F13 of the review of
    // `b9/forever-gaps`): a stall raised and lifted between two looks moves
    // its rev by two, and shows nowhere else
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

/// L15: a worker of the harness under test (`program`), given by `spawn
/// agent=<program>` aimed at a manager tab running it ([`Cover::starter`]):
/// the spawn named that agent ([`Cover::agent`]) and `meta worker=` names it
/// ([`Cover::worker`]); its record `working from=<mgr>` at its first read,
/// `autopilot=land` (a spawn's default) at the first `status` naming one
/// (`-` until the agent runs), Land's landing as L7's (the record
/// `done why=landed`, `TASK … done why=landed`, quiet after it as a managed
/// worker's: [`judge_tasked`]); the manager's `await task @<w> since=0`
/// RETURNED at the landing (not before it), exit 0, the worker's row
/// `state=done`; and `result` its report, a blank line and its defense byte
/// for byte, from the source its harness has ([`result_source`]).
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
    // The spawn's default Autopilot, as `status` names it once the tab runs
    // an agent the harness supervises: `autopilot=-` before that (a read
    // racing the agent's start: Gemini CLI 0.46.0's, measured, is `node`
    // until the status sweep publishes it), the first word after it `land`.
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

/// Whether CAP's tab has been watched to its verdict ([`finished`]): an act
/// typed inside the hour after the first (the cap did not hold), or an act
/// after the orchestrator's turn, a minute after it.
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

/// CAP: `continue_per_hour = 1`. The first act (Land's instruction) is the
/// hour's one; the next point is ESCALATED (journaled) with the attention
/// naming the cap ([`cap_reason`]); NOTHING TYPED from the act to the
/// orchestrator's turn past the hour's end (no journal act, no typed,
/// approved or declined ledger row, no new harness text in the stub's dumps:
/// [`Cover::window_stub`]): the escalated point is never decided again on a
/// timer once the hour frees, no NEEDS YOU need raised ([`need_raised`]: the
/// escalation's own `needs=ask` face is the cap's); and after the hour the policy
/// acts again at a new point — the orchestrator's turn ([`Cover::turn_ms`],
/// typed past the hour) judged and acted on ([`judged_resumption`]: its reply
/// read, its back-off decided there, the act at that due), the act ledgered.
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

/// A journal point that reads the usage wall: an `EVENT limited` line (a
/// wall on the screen at the turn's end), or an `EVENT prompt … kind=usage-limit`
/// one (the program's usage-limit dialog, a box: Gemini CLI's).
fn wall_point(line: &str) -> bool {
    line.starts_with("EVENT limited ")
        || (line.starts_with("EVENT prompt ")
            && word(line, "kind").as_deref() == Some("usage-limit"))
}

/// W1: A USAGE WALL UNDER LAND — the harness's own, drawn by the stub's
/// `limit` ([`wall_claim`]) raised at the first point ([`Cover::wall`]): Muse
/// Code's notice, agy's quota error, Claude Code's session limit (on its
/// subscriber path), Gemini CLI's usage-limit dialog. Land's act MEETS it (typed
/// after the raise, before the reset); the wall READ as one (a point after
/// the act that reads it, [`wall_point`]: a point whose words say the usage
/// limit, read otherwise, fails); NOTHING TYPED into it (no journal act, no
/// acting ledger row from the act to the reset, no new harness text in the
/// stub's dumps of it: [`Cover::window_stub`]), not handed to a person (no
/// escalation of it: a stall), no landing while it stands (no DONE, no
/// `task=landed` before the lab lifts it, [`Cover::lifted_ms`]), no need
/// raised. After the reset the harness GOES ON — its act typed and ledgered
/// past the reset, or at Gemini CLI's usage-limit dialog, which takes no
/// typing, its `Keep trying` pressed past the reset the dialog names
/// ([`RULE_LIMIT_RETRY`] ledgered `approved`; one pressed in the wall is an
/// act in it: FAIL). Where the program went on by itself ([`Cover::went_on_ms`],
/// Claude Code's own continuation at the reset, read in the stub's requests),
/// nothing typed and its answer landing is the harness leaving it be, and an
/// act of the loop's after it is typed ON TOP of it, a double continue
/// (`r21amc15b`'s class): FAIL. The program's own retry landing it with no
/// continuation read shows nothing of the harness: UNOBSERVED. The clean
/// defense lands the task, quiet after as L5's ([`landing_quiet`]), no task
/// record invented. Nothing going on, or no landing, within [`WALL_LOST_MS`]
/// of the reset and `program`'s grace ([`wall_grace_ms`]) is persistence
/// lost: FAIL.
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
    let in_wall = |t: i64| t > t1 + AT_DONE_MS && t < reset;
    // the stub answers again from the lift (F14 of the review of
    // `b9/forever-gaps`): a landing from there is the program's or the
    // harness's past the wall, no landing in it
    let lift = seen
        .cover
        .lifted_ms
        .unwrap_or(reset - WALL_LIFT_EARLY_MS)
        .min(reset);
    let acts = seen.all_acts();
    let typed_in: Vec<&(i64, String)> = acts.iter().copied().filter(|(t, _)| in_wall(*t)).collect();
    let rows_in: Vec<&Row> = seen
        .ledger
        .iter()
        .filter(|r| in_wall(r.ts) && r.acted() && r.decision != "escalated")
        .collect();
    // The wall handed to a person while it stood: the loop's escalation of it
    // (Gemini CLI's usage-limit dialog, which no rule presses) stalls the
    // session until someone answers, whatever comes after.
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
    // The program's own continuation past the wall ([`Cover::went_on_ms`]:
    // Claude Code goes on by itself at the reset).
    let went_on = seen.cover.went_on_ms;
    let first_act = acts.iter().copied().find(|(t, _)| *t >= reset);
    // Gemini CLI's usage-limit dialog takes no typing: the harness goes on
    // past it by pressing its `Keep trying` ([`RULE_LIMIT_RETRY`]), a key
    // ledgered `approved` — the first after the reset, before any act.
    let press = seen
        .ledger
        .iter()
        .filter(|r| r.ts >= reset && r.rule == RULE_LIMIT_RETRY && r.decision == "approved")
        .min_by_key(|r| r.ts)
        .filter(|r| first_act.is_none_or(|(ta, _)| r.ts < *ta));
    let resumed = match first_act {
        _ if let Some(r) = press => {
            if r.ts > lost_at {
                return Verdict::Fail(format!(
                    "{said}; PERSISTENCE LOST: the first press after the reset came {} after it, past the {} deadline",
                    at(0, r.ts - reset),
                    at(0, lost_at - reset)
                ));
            }
            if let Some(tv) = went_on.filter(|tv| *tv < r.ts) {
                return Verdict::Fail(format!(
                    "{said}; PRESSED ON TOP of the program's own continuation: it went on at +{} ({} past the reset), and {} was pressed at +{}",
                    at(t0, tv),
                    at(reset, tv),
                    r.rule,
                    at(t0, r.ts)
                ));
            }
            format!(
                "gone on: {} pressed at +{} ({} past the reset), ledgered: {}",
                r.rule,
                at(t0, r.ts),
                at(0, r.ts - reset),
                crate::util::one_line(&r.command, 60)
            )
        }
        Some((ta, la)) => {
            if *ta > lost_at {
                return Verdict::Fail(format!(
                    "{said}; PERSISTENCE LOST: the first act after the reset came {} after it, past the {} deadline",
                    at(0, ta - reset),
                    at(0, lost_at - reset)
                ));
            }
            let rule = act_rule(la).unwrap_or("?");
            // ON TOP OF THE PROGRAM'S OWN CONTINUATION: the program went on
            // by itself, and the loop's act came after it — a double
            // continue (`r21amc15b`'s class), into a conversation the
            // vendor had already carried past the wall.
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
        // the harness's going on is the scenario: a landing nothing of its
        // brought shows nothing of it (F14), unless the program's own
        // continuation was read, which the harness rightly left be
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
    if let Some(r) = invented_record(seen) {
        bad.push(format!("a task record where none was given: {r}"));
    }
    landed_verdict(
        bad,
        format!("{said}; {resumed}; {quiet}"),
        seen,
        done.0,
        window,
    )
}

/// W2: THE REMEMBERED WALL — W1's wall ([`judge_wall`]: Land's act meets it,
/// it is read as one, nothing typed into it to its reset, gone on past it with
/// a typed row, landed, quiet after), and its NOTICE LEFT THE SCREEN while the
/// wait stood ([`Cover::hidden`]: the harness's own local command — Muse
/// Code's `/rules`, agy's `/title` ([`hide_of`]) — printed until it scrolled
/// the notice out of sight before the reset, after the land act). The loop
/// READ a screen its reader found no wall on: a point journaled that is not
/// the limit's, after the first command's Enter and before the reset (a
/// reader may let the notice go before its last row scrolls away — Muse
/// Code's does — so that point may come at a screen still drawing a row of
/// it). And what went on past the reset was the
/// loop's own act decided at a screen without the notice: typed after the
/// hiding Enter, at the screen seq the lab read without it or later (the
/// content seq only grows, and nothing draws the notice again), never at a
/// screen from before the notice left. Any FAIL of W1's judge is W2's; a
/// notice that never left, left at or after the reset, no wall-less point
/// read, or a resume not typed from the hidden screen (none typed, the
/// program's own retry; one typed at the notice's own screen) did not drive
/// the remembered wall: UNOBSERVED, never a pass. The loop's waits read after the hide are
/// named (an unread conversation's look, `limit-conversation-unread`, among
/// them).
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
    // The point the loop reads once its reader finds no wall on the screen: a
    // reader may stop reading the notice before the last of its rows has
    // scrolled away (Muse Code's does; agy's reads none after the first
    // command's `> /title` row, its last user row), so that point may come at
    // a screen still drawing a row of it, after the first command and before
    // the reset.
    let read = seen.journal.iter().find(|(t, l)| {
        *t > h.first_ms && *t < reset && l.starts_with("EVENT ") && !l.starts_with("EVENT limited")
    });
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
    let Some((ta, la)) = seen
        .all_acts()
        .into_iter()
        .find(|(t, _)| *t >= reset)
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
