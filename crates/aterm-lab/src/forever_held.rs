// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOLD SAID (§14.2's 2a): what `lab forever` wants once aterm stops typing into a chain.
//!
//! Under ruling F (`4cefe18775`) the loop stops typing at a reply that comes again with no work
//! and journals `EXCEPTION … kind=stalled`. A chain whose script makes that hold
//! ([`Chain::same_reply`]) is watched [`PAST_NEXT_STEP_MS`] past it ([`watched_past`]), brought in
//! front twice inside that minute ([`look_step`]), and wants two things of it ([`with_held`]):
//!
//! * what it says, on surfaces every build since 5d prints: each look in front carries a stop word
//!   ([`STOP_WORD`]) on the chip's description and title where the chip is drawn, else on the
//!   tab's hover ([`said`]);
//! * what 3a adds (`status held=`, the state line's stop words, the `Stopped typing` row in `ctl
//!   messages`): named skips until 3a is on main ([`three_a`]).
//!
//! And the hold stands (limit 1): a lift ([`lift_of`]) with no request of its conversation read by
//! the stub between the hold and the lift fails it ([`stands`]).
//!
//! Any other chain of the measured pass or the coverage chains fails at a `kind=stalled`
//! ([`stall_fault`]).

use aterm_agent::supervise::policy::turn_end;

use super::super::{
    Chain, PAST_NEXT_STEP_MS, Repo, Scenario, Seen, Tab, Verdict, act_rule, at, idle_words, word,
};
use super::{
    Ctx, chip, compose, in_front_drawn, no_footer, observed, quoted, split_title, strip_tabs,
};
use crate::door::Door;
use crate::util::{now_ms, one_line};

/// The stop word a held chain's surfaces must carry (§14.3: the title's and the state line's
/// `stopped typing:`, the description's `Stopped typing:`, the hover's `stopped typing;`), read
/// without regard to case.
pub const STOP_WORD: &str = "stopped typing";

/// When a held chain is brought in front after its hold's journal line: once its window has had
/// time to take the hold (as [`super::hold`]'s hover slack), and once more near the end of the
/// minute it is watched for, so a word said once and dropped again fails too.
pub const HELD_LOOKS_MS: [i64; 2] = [10_000, 40_000];

/// How long after the hold's line a read may still show the words from before it.
const SETTLE_MS: i64 = HELD_LOOKS_MS[0];

/// The want [`said`] composes under.
pub const SAID: &str = "held: within 60 s of its hold, each look in front says `stopped typing` \
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

/// Whether `text` carries the stop word.
#[must_use]
pub fn says_stop(text: &str) -> bool {
    text.to_lowercase().contains(STOP_WORD)
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
fn decided_after(seen: &Seen, t: i64) -> Option<&(i64, String)> {
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
/// it); then the tab labelled `back` (the chains' window's front tab), or where the run has none
/// the tab selected before the look, in front again.
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
        .map_or_else(|| NOT_IN_STRIP.to_string(), |(_, l)| l.clone());
    println!(
        "  [{}] +{} held    in front {} s past its hold ({}): {}",
        tab.chain.name(),
        at(tab.t0, t),
        (t - stall) / 1000,
        read.as_ref()
            .map_or("not in the front window's strip", |(said, _)| said.as_str()),
        one_line(&line, 400)
    );
    tab.seen.window.held.push((t, line));
    if let Some(label) = restore
        && let Some(i) = strip_tabs(&door.out(&["chrome"]))
            .into_iter()
            .find(|s| s.label == label)
    {
        let _ = door.out(&["tab", &i.index.to_string()]);
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

/// A chain that is no same-reply chain, of the measured pass or the coverage chains, held: aterm
/// stopped typing into a conversation whose script answers its acts otherwise, so whatever its own
/// judge says, it failed. The other modes' chains (the walls, the cap, the residuals, the ladder)
/// keep their own judges.
#[must_use]
pub fn stall_fault(chain: Chain, seen: &Seen, t0: i64) -> Option<String> {
    if chain.same_reply() || !(Chain::ALL.contains(&chain) || Chain::COVER.contains(&chain)) {
        return None;
    }
    let (t, l) = stall_of(seen, t0)?;
    Some(format!(
        "aterm stopped typing into it at +{} ({}), though its script answers no act it meets with \
         a reply it gave before",
        at(t0, *t),
        one_line(l, 160)
    ))
}

/// HL and HC: the chain's acts (land@v1 then the nudge under Land, two done checks under Cruise)
/// answered alike (the reply each act's point read, word for word), the hold at the second answer
/// (ruling F), never landed or ended, no need, and nothing typed past the hold.
#[must_use]
pub fn judge_hold(chain: Chain, seen: &Seen, t0: i64) -> Verdict {
    let first_rule = if chain.autopilot() == "land" {
        turn_end::RULE_LAND
    } else {
        turn_end::RULE_DONE_CHECK
    };
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
    if let Some((ta, la)) = acts.iter().find(|(t, _)| t > ts) {
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

/// Folds the hold's wants into the verdict `v` of a same-reply chain's last scenario `s`, where a
/// hold was journaled ([`said`], [`stands`], [`three_a`]); every other verdict as it was. Under
/// `--no-surfaces` what it says is a named skip that keeps `v`'s word, as the window's wants are
/// ([`super::with_window`]); whether it stands reads the journal and the stub, so it is judged
/// either way.
#[must_use]
pub fn with_held(s: Scenario, seen: &Seen, cx: &Ctx<'_>, v: Verdict) -> Verdict {
    if !cx.chain.same_reply() || cx.chain.scenarios().last() != Some(&s) {
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
/// act ([`lift_of`]), or one on a request of its conversation the stub read between the hold and
/// the lift (the agent working, someone's turn: [`Seen::lift_stub`]). A lift with none is aterm
/// reading a reply nobody gave (an agent redrawing its tall reply as its tab comes in front):
/// HELD, THEN LIFTED WITH NO NEW REPLY.
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

/// WHAT IT SAYS: every look in front from [`SETTLE_MS`] past the hold to its lift carries
/// [`STOP_WORD`] on the chip's description and title where the chip is drawn, and, where a look
/// read the footless line (`face=- drawn=- why=no-footer`: a program whose composer draws no
/// footer), every hover read in that span does; at least one look within [`PAST_NEXT_STEP_MS`]
/// of the hold. A look whose chip a footer would draw but did not (`why=not-drawn` past the
/// capture's hang detector, `hidden`, `switch-only`) reads nothing: a named UNOBSERVED, never the
/// hover in its place.
#[must_use]
pub fn said(seen: &Seen, cx: &Ctx<'_>, (ts, line): &(i64, String)) -> Verdict {
    let w = &seen.window;
    let ts = *ts;
    let until = lifted(seen, ts);
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
        let mut missing = Vec::new();
        if !says_stop(&description) {
            missing.push(format!(
                "the description \"{}\"",
                one_line(&description, 160)
            ));
        }
        if !says_stop(&title) {
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
                "{when}: {} carry no `{STOP_WORD}`",
                missing.join(" and ")
            ));
        }
    }
    if by_hover {
        let hovers = observed(&w.hover, &w.hover_reads, ts + SETTLE_MS - 1, until);
        if hovers.is_empty() {
            unread.push("no hover read where no chip is drawn".into());
        }
        let mut quiet = hovers.iter().filter(|(_, h)| !says_stop(h)).peekable();
        if quiet.peek().is_some() {
            bad.extend(quiet.map(|(t, h)| {
                format!(
                    "+{} the hover \"{}\" carries no `{STOP_WORD}`",
                    at(cx.t0, *t),
                    one_line(h, 160)
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

/// WHAT 3A ADDS (§14.3): `status held=same-reply|no-work`, the state line's stop words and, under
/// Land with a repository leg in a session no manager waits on, one `Stopped typing` row in `ctl
/// messages` (none for the others). They read fields 3a brings, so they are a named UNOBSERVED skip
/// until 3a is on main, whatever this build's `status` prints.
#[must_use]
pub fn three_a(seen: &Seen, cx: &Ctx<'_>, (ts, _): &(i64, String)) -> Verdict {
    let printed = seen
        .status
        .iter()
        .filter(|(t, _)| *t >= *ts)
        .find_map(|(_, s)| word(s, "held"));
    let row = if cx.chain.autopilot() == "land" && cx.chain.repo() != Repo::Clean {
        "one `Stopped typing` row in `ctl messages`"
    } else {
        "no `Stopped typing` row in `ctl messages`"
    };
    Verdict::Unobserved(format!(
        "SKIPPED until 3a is on main: `status held=`, the state line's stop words, {row} ({})",
        printed.map_or_else(
            || "status prints no held=".to_string(),
            |v| format!("status prints held={v}; 3a's judge reads it")
        )
    ))
}

#[cfg(test)]
#[path = "forever_held_tests.rs"]
mod tests;
