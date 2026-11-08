// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read the recorded hold, its status and its accessible tab description.
//! Holds never create a notification row. The lab rejects a reintroduced stop alert,
//! while preserving the checks for claim custody and explicit hold transitions.

use std::borrow::Cow;

use aterm_agent::harness::acts::{ATERM_DRIVES, FACT_SEPARATOR, HeldBy, StopForm, stop_words};
use aterm_agent::supervise::policy::turn_end;

use super::super::switch::SWITCH_SETTLE_MS;
use super::super::{
    Chain, PAST_NEXT_STEP_MS, Scenario, Seen, Tab, Verdict, act_rule, at, idle_words, word,
};
use super::{
    Ctx, ax_of, chip, compose, in_front_drawn, no_footer, observed, page_field, quoted,
    split_title, strip_tabs,
};
use crate::door::Door;
use crate::util::{now_ms, one_line};

/// When a held chain is brought in front after its hold's journal line: once its window has had
/// time to take the hold (as [`super::hold`]'s hover slack), and once more near the end of the
/// minute it is watched for, so a word said once and dropped again fails too.
pub const HELD_LOOKS_MS: [i64; 2] = [10_000, 40_000];

/// How long after the hold's line a read may still show the words from before it.
const SETTLE_MS: i64 = HELD_LOOKS_MS[0];

/// The want [`said`] composes under.
pub const SAID: &str = "held: within 60 s of its hold, each look in front leads with the stop \
                        (the chip's description and title where drawn, else the tab's hover)";

/// The want [`three_a`] composes under.
pub const ADDS: &str = "held: what 3a adds";

/// The want [`stands`] composes under.
pub const STANDS: &str = "held: the hold stands until something new reaches the agent";

/// What a look keeps where its tab's label was not in the front window's strip ([`look_step`]);
/// `-` is a look whose `chrome` held no `autopilot` line for the tab.
pub const NOT_IN_STRIP: &str = "not-in-strip";

/// Whether journal `line` is a hold's: `EXCEPTION … kind=stalled` (ruling F's same-reply and
/// no-work holds, `Exception::repeats`; 3a adds `when=now` and `held=` to the same line).
#[must_use]
pub fn is_stall(line: &str) -> bool {
    line.starts_with("EXCEPTION ") && word(line, "kind").as_deref() == Some("stalled")
}

/// The first hold journaled from `t0` on.
#[must_use]
pub fn stall_of(seen: &Seen, t0: i64) -> Option<&(i64, String)> {
    seen.journal.iter().find(|(t, l)| *t >= t0 && is_stall(l))
}

/// The arm the hold's journal line names (`held=`, 3a's), else the same reply: a build before 3a
/// names none, and ruling F's chains are held by the same reply.
#[must_use]
pub fn arm_of(line: &str) -> HeldBy {
    word(line, "held")
        .and_then(|w| HeldBy::of_word(&w))
        .unwrap_or(HeldBy::SameReply)
}

/// THE HOLD'S WORDS BEHIND A STOP: `text` with the stop that leads it in `form` (any arm's
/// [`stop_words`], with the separator after it) taken off, so a reader of a hold's words reads
/// them past the stop; `text` itself where no stop leads it. The title and the hover keep their
/// head (`autopilot: land — `, `driven by aterm (land: `).
#[must_use]
pub fn unstopped(text: &str, form: StopForm) -> Cow<'_, str> {
    let head_end = match form {
        StopForm::Title => text.find(" \u{2014} ").map(|i| i + " \u{2014} ".len()),
        StopForm::Hover => text.find(": ").map(|i| i + ": ".len()),
        StopForm::Line | StopForm::Tip | StopForm::Spoken => Some(0),
    };
    let Some(at) = head_end else {
        return Cow::Borrowed(text);
    };
    let (head, rest) = text.split_at(at);
    for (arm, switch) in HeldBy::ALL
        .into_iter()
        .flat_map(|arm| [(arm, true), (arm, false)])
    {
        let stop = stop_words(arm, form, switch);
        let Some(after) = rest.strip_prefix(stop.as_str()) else {
            continue;
        };
        return Cow::Owned(match form {
            StopForm::Hover => match after.strip_prefix("; ") {
                Some(why) => format!("{head}{why}"),
                // `driven by aterm (cruise: stopped typing)`: the mode alone.
                None => format!("{}{after}", head.strip_suffix(": ").unwrap_or(head)),
            },
            StopForm::Spoken => format!("{head}{}", after.trim_start()),
            _ => format!(
                "{head}{}",
                after.strip_prefix(FACT_SEPARATOR).unwrap_or(after)
            ),
        });
    }
    Cow::Borrowed(text)
}

/// Whether journal `line` lifts a hold: `CLEARED … exception=stalled` (`Exception::repeats`'s
/// clears: "a different reply, someone's turn, or the agent working"), other than at the loop's own
/// end (`… the loop ended`, written as the instance goes down).
#[must_use]
pub fn is_lift(line: &str) -> bool {
    line.starts_with("CLEARED ")
        && word(line, "exception").as_deref() == Some("stalled")
        && !line.trim_end().ends_with("the loop ended")
}

/// The lift of the hold journaled at `t`, before any act after it ([`is_lift`]).
#[must_use]
pub fn lift_of(seen: &Seen, t: i64) -> Option<&(i64, String)> {
    let next_act = seen
        .all_acts()
        .into_iter()
        .find(|(at, _)| *at > t)
        .map_or(i64::MAX, |(at, _)| *at);
    seen.journal
        .iter()
        .find(|(at, l)| *at > t && *at <= next_act && is_lift(l))
}

/// When the hold journaled at `t` was lifted: its lift ([`lift_of`]), else the next act, else
/// past the watch.
fn lifted(seen: &Seen, t: i64) -> i64 {
    seen.journal
        .iter()
        .find(|(at, l)| *at > t && (is_lift(l) || act_rule(l).is_some()))
        .map_or(i64::MAX, |(at, _)| *at)
}

/// How many of a conversation's requests (`(ms, …)` each, the stub's turn dumps of it) the stub
/// read after `from` and by `until`: the lift's witness ([`Seen::lift_stub`]).
#[must_use]
pub fn requests_between(dumps: &[(i64, usize)], from: i64, until: i64) -> usize {
    dumps
        .iter()
        .filter(|(t, _)| *t > from && *t <= until)
        .count()
}

/// The decision journaled for the first point after `t`: a `WAITING` (not the take window's), the
/// hold, a `DONE` or a `LEFT`.
pub(in crate::forever) fn decided_after(seen: &Seen, t: i64) -> Option<&(i64, String)> {
    seen.journal.iter().find(|(at, l)| {
        *at > t
            && ((l.starts_with("WAITING ") && !l.contains(" the worker to take the "))
                || is_stall(l)
                || l.starts_with("DONE ")
                || l.starts_with("LEFT "))
    })
}

/// THE WATCH PAST A CHAIN'S LAST ACT: its last act's point decided (a `WAITING`, the hold, a
/// `DONE` or a `LEFT`), [`PAST_NEXT_STEP_MS`] past the later of the two, and, where the hold was
/// decided and the window is read, every look in front had ([`HELD_LOOKS_MS`]).
#[must_use]
pub fn watched_past(seen: &Seen, now: i64) -> bool {
    let Some((last, _)) = seen.all_acts().last().copied() else {
        return false;
    };
    let Some((decided, line)) = decided_after(seen, *last) else {
        return false;
    };
    let looked =
        !is_stall(line) || !seen.window.read || seen.window.held.len() >= HELD_LOOKS_MS.len();
    looked && now - (*decided).max(*last) >= PAST_NEXT_STEP_MS
}

/// THE LOOK IN FRONT: at each of [`HELD_LOOKS_MS`] past a same-reply chain's hold, its tab brought
/// in front of the chains' window, captured until its chip is drawn where its program draws one,
/// and its `autopilot` line (title and all) kept in [`Window::held`](super::Window::held)
/// ([`NOT_IN_STRIP`] where its label was not in the strip, `-` where `chrome` held no line for
/// it, nothing where that `chrome` read failed: the next pass looks again); then the tab labelled
/// `back` (the chains' window's front tab), or where the run has none the tab selected before the
/// look, in front again.
pub(in crate::forever) fn look_step(door: &Door<'_>, tab: &mut Tab, back: Option<&str>) {
    if !tab.chain.same_reply() {
        return;
    }
    let Some(stall) = stall_of(&tab.seen, tab.t0).map(|(t, _)| *t) else {
        return;
    };
    let now = now_ms();
    let due = HELD_LOOKS_MS.iter().filter(|d| now >= stall + **d).count();
    if tab.seen.window.held.len() >= due {
        return;
    }
    let restore = back_to(back, || door.out(&["chrome"]));
    let read = in_front_drawn(door, tab);
    let t = now_ms();
    let line = read
        .as_ref()
        .map_or_else(|| Ok(NOT_IN_STRIP.to_string()), |(_, l)| l.clone());
    let nodes = if matches!(read, Some((_, Ok(_)))) {
        ax_of(&door.out(&["chrome"]), &tab.sid)
    } else {
        "-".to_string()
    };
    let shown = match &line {
        Ok(l) => one_line(l, 400),
        Err(e) => format!(
            "its line unread, the next pass looks again: {}",
            one_line(e, 400)
        ),
    };
    println!(
        "  [{}] +{} held    in front {} s past its hold ({}): {shown}",
        tab.chain.name(),
        at(tab.t0, t),
        (t - stall) / 1000,
        read.as_ref()
            .map_or("not in the front window's strip", |(said, _)| said.as_str())
    );
    if let Ok(line) = line {
        tab.seen.window.held.push((t, line));
        tab.seen.window.held_ax.push((t, nodes));
    }
    if let Some(label) = restore
        && let Some(i) = strip_tabs(&door.out(&["chrome"]))
            .into_iter()
            .find(|s| s.label == label)
    {
        let _ = door.out(&["tab", &(i.index + 1).to_string()]);
    }
}

/// The tab a look puts back in front ([`look_step`]): `back`, the chains' window's front tab, or
/// where the run has none (no chain of [`super::FRONT`]) the tab the strip showed selected before
/// the look (`chrome`, read only then).
#[must_use]
pub fn back_to(back: Option<&str>, chrome: impl FnOnce() -> String) -> Option<String> {
    match back {
        Some(label) => Some(label.to_string()),
        None => strip_tabs(&chrome())
            .into_iter()
            .find(|s| s.selected)
            .map(|s| s.label),
    }
}

/// A chain of the measured pass or the coverage chains held where no hold belongs, so whatever its
/// own judge says, it failed: a chain that is no same-reply chain (aterm stopped typing into a
/// conversation whose script answers its acts otherwise), or a same-reply chain under Cruise,
/// which holds no repeated report since the continuous-forward design of 2026-11-01 (it paces it
/// on the ladder and asks for a different approach). `same-cruise`'s hold is HC's own
/// ([`super::super::paced`]: STOPPED FOR GOOD); the other modes' chains (the walls, the cap, the
/// residuals, the ladder) keep their own judges.
#[must_use]
pub fn stall_fault(chain: Chain, seen: &Seen, t0: i64) -> Option<String> {
    if !(Chain::ALL.contains(&chain) || Chain::COVER.contains(&chain)) || chain == Chain::SameCruise
    {
        return None;
    }
    let cruise = chain.same_reply() && chain.autopilot() == "cruise";
    if chain.same_reply() && !cruise {
        return None;
    }
    let (t, l) = stall_of(seen, t0)?;
    let why = if cruise {
        "STOPPED FOR GOOD, though Cruise holds no repeated report (the continuous-forward design \
         of 2026-11-01: it waits the ladder's step and asks for a different approach)"
    } else {
        "though its script answers no act it meets with a reply it gave before"
    };
    Some(format!(
        "aterm stopped typing into it at +{} ({}), {why}",
        at(t0, *t),
        one_line(l, 160)
    ))
}

/// HL: the chain's acts (land@v1, then Land's next act) answered alike (the reply each act's point
/// read, word for word), the hold at the second answer (ruling F, which stands under Land), never
/// landed or ended, no need, and nothing typed past the hold. Cruise holds no repeated report
/// (the continuous-forward design of 2026-11-01): HC is [`super::super::paced`]'s, and a chain
/// whose first act is no land@v1 fails here.
#[must_use]
pub fn judge_hold(seen: &Seen, t0: i64) -> Verdict {
    let first_rule = turn_end::RULE_LAND;
    let acts = seen.all_acts();
    let Some(first) = acts.first() else {
        return Verdict::Fail(format!("no act typed within {}", at(t0, seen.end_ms)));
    };
    if act_rule(&first.1) != Some(first_rule) {
        return Verdict::Fail(format!(
            "its first act at +{} was {}, not {first_rule}",
            at(t0, first.0),
            act_rule(&first.1).unwrap_or("?")
        ));
    }
    if let Some((d, l)) = seen.first_after("DONE ", t0) {
        return Verdict::Fail(format!("ENDED at +{}: {}", at(t0, *d), one_line(l, 120)));
    }
    if seen.status_said("task", "landed", t0) {
        return Verdict::Fail("status said task=landed".into());
    }
    if let Some(n) = seen.needs_seen() {
        return Verdict::Fail(format!("a need was raised, though no reply named one: {n}"));
    }
    let answer = |t: i64| {
        seen.journal
            .iter()
            .find(|(at, l)| *at > t && idle_words(l).is_some_and(|w| !w.is_empty() && w != "-"))
            .map_or("-", |(_, l)| idle_words(l).unwrap_or("-"))
    };
    let typed: Vec<String> = acts
        .iter()
        .map(|(t, l)| {
            format!(
                "+{} {} answered \"{}\"",
                at(t0, *t),
                act_rule(l).unwrap_or("?"),
                one_line(answer(*t), 70)
            )
        })
        .collect();
    let Some((ts, sl)) = stall_of(seen, t0) else {
        return match decided_after(seen, acts.last().map_or(t0, |(t, _)| *t)) {
            Some((td, dl)) if acts.len() >= 2 => Verdict::Fail(format!(
                "NOT HELD: {}; its last answer decided at +{} ({}), no hold",
                typed.join("; "),
                at(t0, *td),
                one_line(dl, 110)
            )),
            _ => Verdict::Unobserved(format!(
                "{}; no hold within {}",
                typed.join("; "),
                at(t0, seen.end_ms)
            )),
        };
    };
    let before: Vec<&str> = acts
        .iter()
        .filter(|(t, _)| t < ts)
        .map(|(t, _)| answer(*t))
        .collect();
    let held = format!("held at +{} ({})", at(t0, *ts), one_line(sl, 140));
    if before.len() != 2 {
        return Verdict::Fail(format!(
            "{held} after {} act(s), not at the second answer: {}",
            before.len(),
            typed.join("; ")
        ));
    }
    if before[0].trim() != before[1].trim() {
        return Verdict::Fail(format!(
            "{held} at the second answer, but the two answers differ: {}",
            typed.join("; ")
        ));
    }
    // Past same-land's person's turn the hold is lifted by it, and Land may type again.
    let person = seen.stop_person_ms.unwrap_or(i64::MAX);
    if let Some((ta, la)) = acts.iter().find(|(t, _)| t > ts && *t < person) {
        return Verdict::Fail(format!(
            "{held}, yet {} typed at +{}: {}",
            act_rule(la).unwrap_or("?"),
            at(t0, *ta),
            typed.join("; ")
        ));
    }
    Verdict::Pass(format!(
        "{}; {held}; nothing typed after it within {}",
        typed.join("; "),
        at(t0, seen.end_ms)
    ))
}

/// Folds the hold's wants into the verdict `v` of a Land same-reply chain's last scenario `s`, where
/// a hold was journaled ([`said`], [`stands`], [`three_a`]); every other verdict as it was (a
/// Cruise chain's hold is a fault of its records, [`stall_fault`], and HC's its own). Under
/// `--no-surfaces` what it says is a named skip that keeps `v`'s word, as the window's wants are
/// ([`super::with_window`]); whether it stands reads the journal and the stub, so it is judged
/// either way.
#[must_use]
pub fn with_held(s: Scenario, seen: &Seen, cx: &Ctx<'_>, v: Verdict) -> Verdict {
    if !cx.chain.same_reply()
        || cx.chain.autopilot() != "land"
        || cx.chain.scenarios().last() != Some(&s)
    {
        return v;
    }
    let Some(stall) = stall_of(seen, cx.t0) else {
        return v;
    };
    let v = if seen.window.read {
        compose(v, SAID, said(seen, cx, stall))
    } else {
        let skip = format!("; {SAID}: SKIPPED (--no-surfaces)");
        match v {
            Verdict::Pass(w) => Verdict::Pass(w + &skip),
            Verdict::Fail(w) => Verdict::Fail(w + &skip),
            Verdict::Unobserved(w) => Verdict::Unobserved(w + &skip),
        }
    };
    let v = compose(v, STANDS, stands(seen, cx, stall));
    compose(v, ADDS, three_a(seen, cx, stall))
}

/// THE HOLD STANDS (limit 1, §14.10: a tab aterm stopped stays stopped): no lift before the next
/// act ([`lift_of`]), or one at the person's switch to Land over it (v6 §14.17 item 3; their press,
/// never `meta set`'s), or one at
/// same-land's person's turn ([`person_due`]), or one on a request of its conversation the stub
/// read between the hold and the lift (the agent working, someone's turn: [`Seen::lift_stub`]).
/// A lift with none is aterm reading a reply nobody gave (an agent redrawing its tall reply as
/// its tab comes in front): HELD, THEN LIFTED WITH NO NEW REPLY.
#[must_use]
pub fn stands(seen: &Seen, cx: &Ctx<'_>, (ts, _): &(i64, String)) -> Verdict {
    let Some((tl, ll)) = lift_of(seen, *ts) else {
        return Verdict::Pass(format!(
            "no lift from +{} to +{}",
            at(cx.t0, *ts),
            at(cx.t0, seen.end_ms)
        ));
    };
    let look = seen
        .window
        .held
        .iter()
        .rfind(|(t, _)| *t <= *tl)
        .map_or_else(String::new, |(t, _)| {
            format!(
                ", {} after the look in front at +{}",
                at(*t, *tl),
                at(cx.t0, *t)
            )
        });
    let lifted = format!("at +{} ({}){look}", at(cx.t0, *tl), one_line(ll, 140));
    // The person's switch to Land over the hold is their turn (v6 §14.17 item 3); `meta set`'s
    // is no person's.
    if let Some(sw) = seen
        .cover
        .words
        .iter()
        .find(|w| w.took && w.person && w.word == "land" && w.at > *ts && w.at <= *tl)
    {
        return Verdict::Pass(format!(
            "lifted {lifted}, on the person's switch to Land at +{}",
            at(cx.t0, sw.at)
        ));
    }
    if let Some(p) = seen.stop_person_ms.filter(|p| tl >= p) {
        return Verdict::Pass(format!(
            "lifted {lifted}, on the person's turn at +{}{}",
            at(cx.t0, p),
            seen.stop_person_sent_ms
                .map_or_else(String::new, |t| format!(" (sent at +{})", at(cx.t0, t)))
        ));
    }
    match seen.lift_stub {
        Some(0) => Verdict::Fail(format!(
            "HELD, THEN LIFTED WITH NO NEW REPLY {lifted}: the stub read no request of its \
             conversation from the hold at +{} to the lift",
            at(cx.t0, *ts)
        )),
        Some(n) => Verdict::Pass(format!(
            "lifted {lifted}, on {n} request(s) of its conversation the stub read since the hold"
        )),
        None => Verdict::Unobserved(format!(
            "lifted {lifted}; the stub's requests between the hold and the lift were not read"
        )),
    }
}

/// The heads a held chain's surfaces lead with under `mode` (`land`, `cruise`) and `arm`: the
/// description's, the title's and the hover's ([`stop_words`]), naming the person's switch to Land
/// where it asks ([`names_switch`]).
#[must_use]
pub fn stop_heads(mode: &str, arm: HeldBy) -> [String; 3] {
    let switch = names_switch(mode);
    [
        stop_words(arm, StopForm::Spoken, switch),
        format!(
            "autopilot: {mode} \u{2014} {}",
            stop_words(arm, StopForm::Title, switch)
        ),
        format!(
            "{ATERM_DRIVES} ({mode}: {}",
            stop_words(arm, StopForm::Hover, switch)
        ),
    ]
}

/// WHETHER A HELD CHAIN'S STOP NAMES THE PERSON'S SWITCH TO LAND under `mode` (v6 §14.17 item
/// 3): under Cruise, where the switch asks once more over the agent's own reply (every chain's
/// hold is one: no wall's), and never under Land, where there is no switch to make.
#[must_use]
pub fn names_switch(mode: &str) -> bool {
    mode == "cruise"
}

/// THE WORDS A READ AT `t` MAY SHOW: the Autopilot word the tab carried then (the last switch
/// taken by `t`, the person's or `meta set`'s ([`super::super::switch::Switched`]), else the
/// chain's `opening` word) and, within [`SWITCH_SETTLE_MS`] past that switch, the word before it,
/// which a read that close may still show. A held tab's stop names the switch under the one and
/// not the other ([`names_switch`]), so each read is held to the words of its own moment.
#[must_use]
pub fn modes_at(seen: &Seen, opening: &'static str, t: i64) -> Vec<&'static str> {
    let taken: Vec<(i64, &'static str)> = seen
        .cover
        .words
        .iter()
        .filter(|w| w.took && w.at <= t)
        .map(|w| (w.at, w.word))
        .collect();
    let Some(&(at, now)) = taken.last() else {
        return vec![opening];
    };
    let before = taken.len().checked_sub(2).map_or(opening, |i| taken[i].1);
    if t < at + SWITCH_SETTLE_MS && before != now {
        vec![now, before]
    } else {
        vec![now]
    }
}

/// The stop's heads a read at `t` may lead with ([`stop_heads`] under each of [`modes_at`]).
fn heads_at(seen: &Seen, cx: &Ctx<'_>, arm: HeldBy, t: i64) -> Vec<[String; 3]> {
    modes_at(seen, cx.chain.autopilot(), t)
        .into_iter()
        .map(|m| stop_heads(m, arm))
        .collect()
}

/// WHAT IT SAYS: every look in front from [`SETTLE_MS`] past the hold to its lift leads the
/// chip's description and title with the stop ([`stop_heads`], under the word of that look's
/// moment: [`modes_at`]) where the chip is drawn, and,
/// where a look read the footless line (`face=- drawn=- why=no-footer`: a program whose composer
/// draws no footer), every hover read in that span does; at least one look within
/// [`PAST_NEXT_STEP_MS`] of the hold. A look whose chip a footer would draw but did not
/// (`why=not-drawn` past the capture's hang detector, `hidden`, `switch-only`) reads nothing: a
/// named UNOBSERVED, never the hover in its place.
#[must_use]
pub fn said(seen: &Seen, cx: &Ctx<'_>, (ts, line): &(i64, String)) -> Verdict {
    let w = &seen.window;
    let ts = *ts;
    let until = lifted(seen, ts);
    let arm = arm_of(line);
    let held = format!("held at +{} ({})", at(cx.t0, ts), one_line(line, 140));
    let looks: Vec<&(i64, String)> = w
        .held
        .iter()
        .filter(|(t, _)| *t >= ts + SETTLE_MS && *t < until)
        .collect();
    let mut bad = Vec::new();
    let mut read = Vec::new();
    let mut unread = Vec::new();
    let mut by_hover = false;
    for (t, l) in &looks {
        let when = format!("+{} in front", at(cx.t0, *t));
        if l == NOT_IN_STRIP {
            unread.push(format!(
                "{when}: its label was not in the front window's strip"
            ));
            continue;
        }
        if l == "-" {
            unread.push(format!("{when}: `chrome` held no `autopilot` line for it"));
            continue;
        }
        let (rest, title) = split_title(l);
        let c = chip(&rest);
        if no_footer(&c) {
            by_hover = true;
            read.push(format!("{when}: no chip drawn ({})", one_line(&rest, 120)));
            continue;
        }
        if c.drawn.is_none() {
            unread.push(format!(
                "{when}: its chip was not drawn (why={}), so neither its description nor its \
                 title was read",
                c.why.as_deref().unwrap_or("-")
            ));
            continue;
        }
        let description = quoted(&rest, "description").unwrap_or_else(|| "-".into());
        let title = title.unwrap_or_else(|| "?".into());
        let heads = heads_at(seen, cx, arm, *t);
        let [spoken, titled, _] = &heads[0];
        let mut missing = Vec::new();
        if !heads
            .iter()
            .any(|[s, _, _]| description.starts_with(s.as_str()))
        {
            missing.push(format!(
                "the description \"{}\"",
                one_line(&description, 160)
            ));
        }
        if !heads.iter().any(|[_, h, _]| title.starts_with(h.as_str())) {
            missing.push(format!("the title \"{}\"", one_line(&title, 200)));
        }
        if missing.is_empty() {
            read.push(format!(
                "{when}: the description \"{}\", the title \"{}\"",
                one_line(&description, 120),
                one_line(&title, 160)
            ));
        } else {
            bad.push(format!(
                "{when}: {} lead with no stop ({:?}, {:?})",
                missing.join(" and "),
                one_line(spoken, 60),
                one_line(titled, 80)
            ));
        }
    }
    if by_hover {
        let hovers = observed(&w.hover, &w.hover_reads, ts + SETTLE_MS - 1, until);
        if hovers.is_empty() {
            unread.push("no hover read where no chip is drawn".into());
        }
        let mut quiet = hovers
            .iter()
            .filter(|(t, h)| {
                !heads_at(seen, cx, arm, *t)
                    .iter()
                    .any(|[_, _, hovered]| h.starts_with(hovered.as_str()))
            })
            .peekable();
        if quiet.peek().is_some() {
            bad.extend(quiet.map(|(t, h)| {
                format!(
                    "+{} the hover \"{}\" leads with no stop ({:?})",
                    at(cx.t0, *t),
                    one_line(h, 160),
                    heads_at(seen, cx, arm, *t)[0][2]
                )
            }));
        } else if let Some((t, h)) = hovers.first() {
            read.push(format!(
                "+{} the hover \"{}\"",
                at(cx.t0, *t),
                one_line(h, 160)
            ));
        }
    }
    if !bad.is_empty() {
        return Verdict::Fail(format!("{held}; HELD, NOT SAID: {}", bad.join("; ")));
    }
    if !looks.iter().any(|(t, _)| *t <= ts + PAST_NEXT_STEP_MS) {
        unread.push(format!(
            "no look in front within {} s of it",
            PAST_NEXT_STEP_MS / 1000
        ));
    }
    let said = format!(
        "{held}; {}",
        if read.is_empty() {
            "-".to_string()
        } else {
            read.join("; ")
        }
    );
    if unread.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Unobserved(format!("{said}: {}", unread.join("; ")))
    }
}

/// The rows `ctl messages tag=harness` printed of tab `sid`'s resume ask (`harness.resume-ask.<sid>`,
/// the row a `Now` exception raises): `<id> state=<state> title=<title>` each, `|` apart, its
/// title decoded; `-` where none.
#[must_use]
pub fn resume_ask_rows(messages: &str, sid: &str) -> String {
    let key = format!("harness.resume-ask.{sid}");
    let rows: Vec<String> = messages
        .lines()
        .filter_map(|l| l.strip_prefix("message "))
        .filter(|l| word(l, "key").as_deref() == Some(key.as_str()))
        .map(|l| {
            format!(
                "{} state={} title={}",
                l.split_whitespace().next().unwrap_or("?"),
                word(l, "state").unwrap_or_else(|| "?".into()),
                super::super::pct_decode(&word(l, "title").unwrap_or_default())
            )
        })
        .collect();
    if rows.is_empty() {
        "-".to_string()
    } else {
        rows.join(" | ")
    }
}

/// Any live resume-ask row is an alert regression, irrespective of its title.
#[must_use]
pub fn stop_rows(rows: &str) -> Vec<&str> {
    rows.split(" | ")
        .filter(|row| word(row, "state").as_deref() == Some("live"))
        .collect()
}

/// When same-land's person types their turn into its held tab (§14.3: the row is withdrawn once
/// the hold lifts, and someone's turn lifts it): once it was held and watched past its hold
/// ([`watched_past`]), the looks had; never twice.
#[must_use]
pub fn person_due(chain: Chain, seen: &Seen, t0: i64, now: i64) -> bool {
    chain == Chain::SameLand
        && seen.stop_person_ms.is_none()
        && stall_of(seen, t0).is_some()
        && seen.all_acts().len() >= 2
        && watched_past(seen, now)
}

/// The person's turn same-land types once its hold was watched ([`person_due`]): plain words,
/// with no key a composer reads as a shortcut of its own.
pub const PERSON_TURN: &str = "tell me what is still open here";

/// Records same-land's person's turn on `seen`: `began`, when the lab began typing it, is the
/// turn (their first key lifts the hold: aterm journals `a person is typing`), and `sent` when its
/// Enter was read back (`None`: the composer did not take it), which the verdicts name beside it.
pub fn person_typed(seen: &mut Seen, began: i64, sent: Option<i64>) {
    seen.stop_person_ms = Some(began);
    seen.stop_person_sent_ms = sent;
}

/// Verify the diagnostic journal, machine status and accessible tab state while held.
/// No reading may contain the retired stop alert.
#[must_use]
pub fn three_a(seen: &Seen, cx: &Ctx<'_>, (ts, line): &(i64, String)) -> Verdict {
    let ts = *ts;
    let arm = arm_of(line);
    let until = lifted(seen, ts);
    let from = ts + SETTLE_MS - 1;
    let mut bad: Vec<String> = Vec::new();
    let mut read: Vec<String> = Vec::new();
    let mut unread: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let when = "later";
    match (word(line, "when"), word(line, "held")) {
        (Some(w), Some(h)) if w == when && h == arm.word() => {
            read.push(format!("journaled when={w} held={h}"));
        }
        (w, h) => bad.push(format!(
            "its journal line reads when={} held={}, not when={when} held={}",
            w.as_deref().unwrap_or("-"),
            h.as_deref().unwrap_or("(none)"),
            arm.word()
        )),
    }
    let status = observed(&seen.status, &seen.status_reads, from, until);
    let off: Vec<String> = status
        .iter()
        .filter(|(_, s)| word(s, "held").as_deref() != Some(arm.word()))
        .map(|(t, s)| {
            format!(
                "+{} held={}",
                at(cx.t0, *t),
                word(s, "held").unwrap_or_else(|| "(none)".into())
            )
        })
        .collect();
    if status.is_empty() {
        unread.push("no `status` read while it held".into());
    } else if off.is_empty() {
        read.push(format!(
            "{} status read(s) held={}",
            status.len(),
            arm.word()
        ));
    } else {
        bad.push(format!(
            "status did not print held={}: {}",
            arm.word(),
            off.join(", ")
        ));
    }
    let w = &seen.window;
    if w.read {
        let leads = |t: i64| -> Vec<String> {
            modes_at(seen, cx.chain.autopilot(), t)
                .into_iter()
                .map(|m| stop_words(arm, StopForm::Line, names_switch(m)))
                .collect()
        };
        let rows = observed(&w.page, &w.page_reads, from, until);
        let drawn: Vec<(i64, &str)> = rows.iter().copied().filter(|(_, p)| *p != "-").collect();
        let lines: Vec<(i64, &str)> = drawn
            .iter()
            .filter_map(|(t, p)| Some((*t, page_field(p, "state")?)))
            .collect();
        match lines
            .iter()
            .find(|(t, l)| !leads(*t).iter().any(|lead| l.starts_with(lead.as_str())))
        {
            _ if rows.is_empty() => {
                unread.push("the Harness page was not read while it held".into())
            }
            // The page draws its first sessions' rows (as the hold's legs read them, [`super::hold`]).
            _ if drawn.is_empty() => skipped.push(format!(
                "the state line: SKIPPED (its row was not drawn on the Harness page in {} read(s) \
                 while it held)",
                rows.len()
            )),
            _ if lines.is_empty() => bad.push(format!(
                "+{} its row on the Harness page drew no state line",
                at(cx.t0, drawn[0].0)
            )),
            Some((t, l)) => bad.push(format!(
                "+{} the state line \"{}\" leads with no stop ({:?})",
                at(cx.t0, *t),
                one_line(l, 200),
                leads(*t)[0]
            )),
            None => read.push(format!("the state line \"{}\"", one_line(lines[0].1, 160))),
        }
        heard(
            seen,
            cx,
            (ts, until),
            arm,
            [&mut read, &mut bad, &mut unread],
        );
    } else {
        skipped.push("the state line: SKIPPED (--no-surfaces)".into());
    }
    let asks = observed(&seen.harness, &seen.harness_reads, from, until);
    if asks.is_empty() {
        unread.push("`ctl messages` was not read while it held".into());
    }
    {
        let after = observed(&seen.harness, &seen.harness_reads, ts - 1, seen.end_ms + 1);
        match after.iter().find(|(_, b)| !stop_rows(b).is_empty()) {
            Some((t, b)) => bad.push(format!(
                "+{} a removed `Stopped typing` alert row: {}",
                at(cx.t0, *t),
                one_line(b, 200)
            )),
            None if !after.is_empty() => read.push("no `Stopped typing` row".into()),
            None => {}
        }
    }
    if cx.chain == Chain::SameLand {
        match seen.stop_person_ms {
            None => unread.push("no person's turn was typed past its looks".into()),
            Some(p) => {
                let lift = seen.journal.iter().find(|(t, l)| *t >= p && is_lift(l));
                let past = lift.map(|(tl, _)| {
                    observed(&seen.harness, &seen.harness_reads, *tl, seen.end_ms + 1)
                });
                match (lift, past) {
                    (Some((tl, _)), Some(past)) if past.is_empty() => unread.push(format!(
                        "the person's turn at +{} lifted the hold at +{}; `ctl messages` was not \
                         read past the lift",
                        at(cx.t0, p),
                        at(cx.t0, *tl)
                    )),
                    (Some((tl, _)), Some(past)) => {
                        match past.iter().find(|(_, b)| stop_rows(b).is_empty()) {
                            Some((tg, _)) => read.push(format!(
                                "the person's turn at +{}, the lift at +{}, the row gone by +{}",
                                at(cx.t0, p),
                                at(cx.t0, *tl),
                                at(cx.t0, *tg)
                            )),
                            None => bad.push(format!(
                                "the person's turn at +{} lifted the hold at +{}, yet the `Stopped \
                             typing` row stood to +{}",
                                at(cx.t0, p),
                                at(cx.t0, *tl),
                                at(cx.t0, seen.end_ms)
                            )),
                        }
                    }
                    (_, _) => bad.push(format!(
                        "the person's turn at +{} lifted no hold by +{}",
                        at(cx.t0, p),
                        at(cx.t0, seen.end_ms)
                    )),
                }
            }
        }
    }
    let mut said = if read.is_empty() {
        "-".to_string()
    } else {
        read.join("; ")
    };
    for s in skipped {
        said = format!("{said}; {s}");
    }
    if !bad.is_empty() {
        Verdict::Fail(format!("{said}; {}", bad.join("; ")))
    } else if unread.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Unobserved(format!("{said}: {}", unread.join("; ")))
    }
}

/// Check that each drawn chip exposes the same hold description to a screen reader.
fn heard(
    seen: &Seen,
    cx: &Ctx<'_>,
    (ts, until): (i64, i64),
    arm: HeldBy,
    [read, bad, unread]: [&mut Vec<String>; 3],
) {
    let w = &seen.window;
    let spoken = |t: i64| -> Vec<String> {
        modes_at(seen, cx.chain.autopilot(), t)
            .into_iter()
            .map(|m| stop_words(arm, StopForm::Spoken, names_switch(m)))
            .collect()
    };
    let mut chip_heard: Option<(i64, String)> = None;
    for (i, (t, l)) in w.held.iter().enumerate() {
        if *t < ts + SETTLE_MS || *t >= until || l == NOT_IN_STRIP || l == "-" {
            continue;
        }
        let (rest, _) = split_title(l);
        if chip(&rest).drawn.is_none() {
            continue;
        }
        let when = format!("+{} in front", at(cx.t0, *t));
        let nodes = w
            .held_ax
            .get(i)
            .filter(|(ta, _)| ta == t)
            .map_or("-", |(_, n)| n.as_str());
        if nodes == "-" {
            unread.push(format!(
                "{when}: `chrome` printed no `a11y` node, so the screen reader's was not read"
            ));
            continue;
        }
        let described = quoted(&rest, "description");
        match nodes.lines().find(|n| n.contains(" autopilot sid=")) {
            None => bad.push(format!(
                "{when}: its chip was drawn, yet the window's accessibility tree held no node \
                 for it"
            )),
            Some(n) => {
                let d = quoted(n, "description");
                if d.is_some()
                    && d == described
                    && d.as_deref()
                        .is_some_and(|d| spoken(*t).iter().any(|s| d.starts_with(s.as_str())))
                {
                    chip_heard.get_or_insert((*t, d.unwrap_or_default()));
                } else {
                    bad.push(format!(
                        "{when}: the chip's node says {:?}, not the stop `chrome` read ({:?})",
                        d.as_deref()
                            .map_or_else(|| "-".to_string(), |d| one_line(d, 120)),
                        described
                            .as_deref()
                            .map_or_else(|| "-".to_string(), |d| one_line(d, 80))
                    ));
                }
            }
        }
    }
    if let Some((t, d)) = chip_heard {
        read.push(format!(
            "+{} the chip's node says \"{}\"",
            at(cx.t0, t),
            one_line(&d, 100)
        ));
    }
}

#[cfg(test)]
#[path = "forever_held_tests.rs"]
pub(in crate::forever) mod tests;
