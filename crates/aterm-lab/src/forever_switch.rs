// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LAND ACTS AT ONCE (v6 §14.10's 10a): what `lab forever` wants of Land's instruction once only
//! a back-off stood before it.
//!
//! * On the measured pass's Land chains ([`at_once`]) land@v1 comes within [`AT_ONCE_MS`] of the
//!   first point ([`land_at_once`]). Main typed it at +124.3 s: the first short turn's 2-minute
//!   back-off was waited out before the instruction.
//! * `--mode switch` ([`Chain::SWITCH`]) switches a running Cruise tab's word with `meta set
//!   autopilot land`, as a chip click, the Autopilot menu or Settings' row does ([`word_step`]):
//!   - `switch-agent`: at rest in its first short turn's back-off, switched [`SWITCH_AFTER_MS`]
//!     in: land@v1 within [`LAND_WITHIN_MS`] of the switch and before the back-off's due
//!     ([`judge_at_rest`]);
//!   - `switch-seq`: the same under a draft the lab leaves in the composer over the switch:
//!     nothing typed while it stands, land@v1 within [`LAND_WITHIN_MS`] of its clearing
//!     ([`judge_draft`]);
//!   - `switch-flip`: Land then Off within [`FLIP_WITHIN_MS`]: nothing typed, past the
//!     back-off's due ([`judge_flip`]);
//!   - `switch-held`: held at the same reply under Cruise, switched to Land past its looks in
//!     front: nothing typed, the hold stands, and its state line still leads with `stopped
//!     typing:` ([`judge_held`], [`held_line`]);
//!   - `switch-busy`: switched [`BUSY_SWITCH_MS`] into a long busy turn: nothing typed mid-turn,
//!     land@v1 within [`BUSY_LAND_MS`] of the turn's point ([`judge_busy`]).
//!
//! A wait at rest the switch wakes is journaled by the loop as `NOTE seq=<n> autopilot=<w> the
//! session's Autopilot word moved: decided after a <ms> ms settle` ([`wake_after`]), within
//! [`WAKE_WITHIN_MS`] of a switch at rest; the session's tapes say which wait carried the word
//! ([`woke_witness`]: `await agent|seq|gone … autopilot=cruise`, answered `OK autopilot land`).

use std::path::Path;

use aterm_agent::harness::acts::{StopForm, stop_words};
use aterm_agent::supervise::approvals::RULE_POINT;
use aterm_agent::supervise::policy::turn_end;
use aterm_agent::supervise::tape::{show, store, text};

use super::surfaces::held::{arm_of, is_lift, stall_of};
use super::surfaces::{Ctx, compose, observed, page_field};
use super::{
    AT_DONE_MS, Chain, Harness, Line, Scenario, Seen, Tab, Verdict, act_rule, at, at_composer,
    backoff_due, backoff_named, backoff_streak, idle_words, on, word,
};
use crate::Fail;
use crate::door::Door;
use crate::run::Run;
use crate::util::{now_ms, one_line};

/// How long after the first point land@v1 may come on the measured pass's Land chains: one
/// decision and the instruction's typing, never a back-off.
pub const AT_ONCE_MS: i64 = 10_000;

/// The want [`land_at_once`] composes under ([`with_at_once`]).
pub const AT_ONCE: &str = "land@v1 at once: within 10 s of the first point (main: +124.3 s)";

/// How far into its back-off a tab at rest is switched.
pub const SWITCH_AFTER_MS: i64 = 10_000;

/// How far into its back-off `switch-seq`'s draft is typed.
pub const DRAFT_AFTER_MS: i64 = 5_000;

/// How long the draft stands before the switch.
pub const DRAFT_BEFORE_MS: i64 = 5_000;

/// How long the draft stands past the switch before the lab clears it.
pub const DRAFT_PAST_MS: i64 = 10_000;

/// The words ending the `WAITING` line the loop journals for the person who typed a draft: its
/// hands-off hold, until their grace from the draft's sighting runs out.
pub const PERSON_HOLD: &str = "a person is typing";

/// `switch-seq`'s draft: plain words with no key a composer reads as a shortcut of its own.
pub const DRAFT: &str = "a note for later";

/// How soon after the switch at rest (or the draft's clearing) land@v1 comes.
pub const LAND_WITHIN_MS: i64 = 5_000;

/// How soon after a switch at rest the loop journals its wake.
pub const WAKE_WITHIN_MS: i64 = 2_000;

/// How close `switch-flip`'s two writes go in.
pub const FLIP_WITHIN_MS: i64 = 300;

/// How far into its first, long turn `switch-busy` is switched.
pub const BUSY_SWITCH_MS: i64 = 5_000;

/// How soon after the busy turn's point land@v1 comes.
pub const BUSY_LAND_MS: i64 = 3_000;

/// How long past its hold `switch-held` is switched: past both of its looks in front
/// ([`super::surfaces::held::HELD_LOOKS_MS`]), which read the hold's words under Cruise.
pub const HELD_SWITCH_MS: i64 = 50_000;

/// How long past the switch a read may still show the words from before it: the loop's one-second
/// settle and a look.
pub const SWITCH_SETTLE_MS: i64 = 3_000;

/// How long a chain that types nothing after its switch is watched past it.
pub const WATCH_MS: i64 = 60_000;

/// How long a chain that lands after its switch is watched past it at most: past its back-off's
/// due, so a land@v1 that waited it out is seen.
pub const LAND_WATCH_MS: i64 = 180_000;

/// How long `switch-flip` is watched past its back-off's due.
const PAST_DUE_MS: i64 = 30_000;

/// The words of the loop's note at a switch's wake (`Session::word_moved`).
pub const WORD_MOVED: &str = "the session's Autopilot word moved";

/// One write of the tab's Autopilot word by the lab ([`word_step`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Switched {
    /// When its `meta set` went in.
    pub at: i64,
    /// The word set.
    pub word: &'static str,
    /// What `meta set` answered.
    pub reply: String,
    /// `status phase=` just before it.
    pub phase: String,
}

/// `switch-seq`'s draft ([`word_step`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    /// When its `send` went in.
    pub typed_ms: i64,
    /// Whether it stood in the composer as typed.
    pub stood: bool,
    /// When its last backspace went in.
    pub cleared_ms: Option<i64>,
    /// Whether the composer stood empty after them.
    pub emptied: bool,
}

/// A wait at rest a tape shows carrying the session's word and woken by another ([`woke_of`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Woke {
    /// When it was answered (unix ms, its exchange's stamp).
    pub at: i64,
    /// Its form: `agent`, `seq`, `gone` or `idle`.
    pub form: String,
    /// The word it carried (`autopilot=<word>`).
    pub carried: String,
    /// What it answered (`OK autopilot <word>`).
    pub answered: String,
}

/// Whether `chain` is one of the switch's own (the Off control is not).
#[must_use]
pub const fn switches(chain: Chain) -> bool {
    matches!(
        chain,
        Chain::SwitchAgent
            | Chain::SwitchSeq
            | Chain::SwitchFlip
            | Chain::SwitchHeld
            | Chain::SwitchBusy
    )
}

/// Whether `chain` is a Land chain of the measured pass, whose instruction is due at its first
/// point ([`land_at_once`]).
#[must_use]
pub fn at_once(chain: Chain) -> bool {
    Chain::ALL.contains(&chain) && chain.autopilot() == "land"
}

/// LAND@V1 AT ONCE: the first land@v1 within [`AT_ONCE_MS`] of the first point judged from `t0`
/// on (`point@v1`), the back-off that stood between them named where one did.
#[must_use]
pub fn land_at_once(seen: &Seen, t0: i64) -> Verdict {
    let Some(point) = seen
        .ledger
        .iter()
        .find(|r| r.rule == RULE_POINT && r.ts >= t0)
    else {
        return Verdict::Unobserved(format!("no point judged within {}", at(t0, seen.end_ms)));
    };
    let Some((tl, _)) = seen.acts(turn_end::RULE_LAND).first().copied() else {
        return Verdict::Fail(format!(
            "no land@v1 typed within {}, the first point at +{}",
            at(t0, seen.end_ms),
            at(t0, point.ts)
        ));
    };
    let gap = tl - point.ts;
    let said = format!(
        "land@v1 at +{}, {} {} the first point at +{}",
        at(t0, *tl),
        at(0, gap.abs()),
        if gap < 0 { "before" } else { "after" },
        at(t0, point.ts)
    );
    if gap.abs() <= AT_ONCE_MS {
        return Verdict::Pass(said);
    }
    let waited = seen
        .journal
        .iter()
        .find(|(t, l)| *t >= point.ts - AT_DONE_MS && t < tl && backoff_named(l))
        .map_or_else(String::new, |(t, l)| {
            format!(", waiting out +{} {}", at(t0, *t), one_line(l, 110))
        });
    Verdict::Fail(format!("{said}{waited}: past {} s", AT_ONCE_MS / 1000))
}

/// Folds [`land_at_once`] into the verdict `v` of the first scenario of a chain [`at_once`] names;
/// every other verdict as it was.
#[must_use]
pub fn with_at_once(s: Scenario, seen: &Seen, cx: &Ctx<'_>, v: Verdict) -> Verdict {
    if !at_once(cx.chain) || cx.chain.scenarios().first() != Some(&s) {
        return v;
    }
    compose(v, AT_ONCE, land_at_once(seen, cx.t0))
}

/// The first short turn's back-off from `t0` on, before any act: what a switch at rest is made in.
#[must_use]
pub fn rest_from(seen: &Seen, t0: i64) -> Option<&Line> {
    let first_act = seen.all_acts().first().map_or(i64::MAX, |(t, _)| *t);
    seen.journal.iter().find(|(t, l)| {
        *t >= t0 && *t < first_act && backoff_named(l) && backoff_streak(l).is_some()
    })
}

/// Whether journal `line` is the loop's wake at a switch ([`WORD_MOVED`]).
#[must_use]
pub fn is_wake(line: &str) -> bool {
    line.starts_with("NOTE ") && line.contains(WORD_MOVED)
}

/// The loop's first wake at a switch from `t` on.
#[must_use]
pub fn wake_after(seen: &Seen, t: i64) -> Option<&Line> {
    seen.journal.iter().find(|(at, l)| *at >= t && is_wake(l))
}

/// The acts journaled after `from` and before `until`.
fn acts_between(seen: &Seen, from: i64, until: i64) -> Vec<&Line> {
    seen.all_acts()
        .into_iter()
        .filter(|(t, _)| *t > from && *t < until)
        .collect()
}

/// The acts named for a verdict: `+<t> <rule>` each.
fn named(acts: &[&Line], t0: i64) -> String {
    acts.iter()
        .map(|(t, l)| format!("+{} {}", at(t0, *t), act_rule(l).unwrap_or("?")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The first wait at rest the tapes show woken from `t` on, named; what the tape showed else.
fn woke_said(seen: &Seen, t: i64, t0: i64) -> String {
    match seen.cover.woke.as_deref() {
        None => "no tape read".to_string(),
        Some(woke) => woke.iter().find(|w| w.at >= t - 1_000).map_or_else(
            || "the tape shows no wait at rest woken by it".to_string(),
            |w| {
                format!(
                    "the tape: +{} `await {} … {}` answered `{}`",
                    at(t0, w.at),
                    w.form,
                    w.carried,
                    w.answered
                )
            },
        ),
    }
}

/// The wake's part of a verdict: the note within [`WAKE_WITHIN_MS`] of the switch at `t`, else a
/// fault in `bad`.
fn wake_within(seen: &Seen, t: i64, t0: i64, bad: &mut Vec<String>) -> String {
    match wake_after(seen, t) {
        Some((tn, ln)) if tn - t <= WAKE_WITHIN_MS => {
            format!("woken +{} ({})", at(t0, *tn), one_line(ln, 120))
        }
        Some((tn, ln)) => {
            bad.push(format!(
                "its wake came {} after it, past {} s: +{} {}",
                at(0, tn - t),
                WAKE_WITHIN_MS / 1000,
                at(t0, *tn),
                one_line(ln, 110)
            ));
            "woken late".to_string()
        }
        None => {
            bad.push(format!(
                "no wake journaled for it (`NOTE … {WORD_MOVED}`) within {}",
                at(t0, seen.end_ms)
            ));
            "no wake".to_string()
        }
    }
}

/// The landing's part of a verdict: land@v1 typed once from `from` on and within `within` of it,
/// before `due` where a back-off named one, its ledger row typed, and nothing else typed between;
/// the faults in `bad`. The land act's time, where one came.
fn land_within(
    seen: &Seen,
    (from, within, due): (i64, i64, Option<i64>),
    t0: i64,
    (said, bad): (&mut Vec<String>, &mut Vec<String>),
) -> Option<i64> {
    let lands = seen.acts(turn_end::RULE_LAND);
    let Some((tl, _)) = lands.iter().copied().find(|(t, _)| *t >= from) else {
        bad.push(format!(
            "no land@v1 typed from +{} within {}",
            at(t0, from),
            at(t0, seen.end_ms)
        ));
        return None;
    };
    said.push(format!(
        "land@v1 at +{}, {} after +{}",
        at(t0, *tl),
        at(0, tl - from),
        at(t0, from)
    ));
    if tl - from > within {
        bad.push(format!(
            "land@v1 came {} after +{}, past {} s",
            at(0, tl - from),
            at(t0, from),
            within / 1000
        ));
    }
    if let Some(due) = due {
        if *tl >= due {
            bad.push(format!(
                "land@v1 at +{} waited out the back-off's due at +{}",
                at(t0, *tl),
                at(t0, due)
            ));
        } else {
            said.push(format!("{} before the back-off's due", at(0, due - tl)));
        }
    }
    let between = acts_between(seen, from, *tl);
    if !between.is_empty() {
        bad.push(format!("typed before land@v1: {}", named(&between, t0)));
    }
    let done = seen.first_after("DONE ", *tl).map_or(i64::MAX, |(d, _)| *d);
    let again: Vec<&Line> = lands
        .iter()
        .copied()
        .filter(|(t, _)| t > tl && *t < done)
        .collect();
    if !again.is_empty() {
        bad.push(format!("land@v1 typed again: {}", named(&again, t0)));
    }
    if seen.typed_row(turn_end::RULE_LAND, *tl).is_none() {
        bad.push(format!(
            "land@v1 journaled at +{}, but no ledger row typed",
            at(t0, *tl)
        ));
    }
    if done < i64::MAX {
        said.push(format!("landed +{}", at(t0, done)));
    }
    Some(*tl)
}

/// A verdict of `said` and `bad`: a fail where anything is bad, else a pass.
fn verdict(said: &[String], bad: &[String]) -> Verdict {
    let said = said.join("; ");
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// SWA: a Cruise tab at rest in its first short turn's back-off, switched to Land: nothing typed
/// before the switch, its wake within [`WAKE_WITHIN_MS`], land@v1 within [`LAND_WITHIN_MS`] of it
/// and before the back-off's due, once, nothing else typed between.
#[must_use]
pub fn judge_at_rest(seen: &Seen, t0: i64) -> Verdict {
    let Some(sw) = seen.cover.words.first() else {
        return Verdict::Unobserved(format!(
            "the switch was never made (no back-off at rest within {})",
            at(t0, seen.end_ms)
        ));
    };
    let Some((tw, wl)) = rest_from(seen, t0).filter(|(t, _)| *t < sw.at) else {
        return Verdict::Fail(format!(
            "switched at +{}, but no back-off at rest stood before it",
            at(t0, sw.at)
        ));
    };
    let due = backoff_due(wl);
    let mut said = vec![
        format!("+{} {}", at(t0, *tw), one_line(wl, 100)),
        format!(
            "switched to {} at +{} ({})",
            sw.word,
            at(t0, sw.at),
            one_line(&sw.reply, 60)
        ),
    ];
    let mut bad = Vec::new();
    let before = acts_between(seen, t0, sw.at);
    if !before.is_empty() {
        bad.push(format!("typed before the switch: {}", named(&before, t0)));
    }
    said.push(wake_within(seen, sw.at, t0, &mut bad));
    land_within(
        seen,
        (sw.at, LAND_WITHIN_MS, due),
        t0,
        (&mut said, &mut bad),
    );
    said.push(woke_said(seen, sw.at, t0));
    verdict(&said, &bad)
}

/// SWS: the same under a draft that stood over the switch: its wake within [`WAKE_WITHIN_MS`],
/// nothing typed from the draft's typing to its clearing, and land@v1 within [`LAND_WITHIN_MS`] of
/// the later of the clearing and the end of the hands-off hold the loop journaled for the person
/// who typed the draft ([`PERSON_HOLD`]): a draft the loop did not type is a person's typing, so it
/// keeps its hands off for the person's grace from its sighting. Where that hold ends before the
/// back-off's due, land@v1 comes before the due too.
#[must_use]
pub fn judge_draft(seen: &Seen, t0: i64) -> Verdict {
    let Some(d) = seen.cover.draft.as_ref() else {
        return Verdict::Unobserved(format!(
            "no draft was typed (no back-off at rest within {})",
            at(t0, seen.end_ms)
        ));
    };
    if !d.stood {
        return Verdict::Unobserved(format!(
            "the draft typed at +{} never stood in the composer",
            at(t0, d.typed_ms)
        ));
    }
    let Some(sw) = seen.cover.words.first() else {
        return Verdict::Unobserved(format!(
            "the draft stood from +{}, but the switch was never made",
            at(t0, d.typed_ms)
        ));
    };
    let Some(cleared) = d.cleared_ms.filter(|_| d.emptied) else {
        return Verdict::Unobserved(format!(
            "switched at +{} under the draft, but it was not cleared",
            at(t0, sw.at)
        ));
    };
    let Some((tw, wl)) = rest_from(seen, t0).filter(|(t, _)| *t < d.typed_ms) else {
        return Verdict::Fail(format!(
            "the draft at +{}, but no back-off at rest stood before it",
            at(t0, d.typed_ms)
        ));
    };
    let mut said = vec![
        format!("+{} {}", at(t0, *tw), one_line(wl, 100)),
        format!("the draft from +{}", at(t0, d.typed_ms)),
        format!("switched to {} at +{}", sw.word, at(t0, sw.at)),
    ];
    let mut bad = Vec::new();
    let before = acts_between(seen, t0, d.typed_ms);
    if !before.is_empty() {
        bad.push(format!("typed before the draft: {}", named(&before, t0)));
    }
    said.push(wake_within(seen, sw.at, t0, &mut bad));
    let over = acts_between(seen, d.typed_ms - 1, cleared);
    if !over.is_empty() {
        bad.push(format!(
            "typed while the draft stood (to +{}): {}",
            at(t0, cleared),
            named(&over, t0)
        ));
    }
    said.push(format!("cleared at +{}", at(t0, cleared)));
    let hold = seen
        .journal
        .iter()
        .find(|(t, l)| {
            *t >= d.typed_ms
                && *t <= cleared
                && l.starts_with("WAITING ")
                && l.ends_with(PERSON_HOLD)
        })
        .and_then(|(_, l)| {
            crate::workers::iso_ms(l.split_once(" until=")?.1.split_whitespace().next()?)
        });
    if let Some(h) = hold {
        said.push(format!("the hold for the person to +{}", at(t0, h)));
        let inside = acts_between(seen, cleared, h);
        if !inside.is_empty() {
            bad.push(format!(
                "typed inside the hold for the person (to +{}): {}",
                at(t0, h),
                named(&inside, t0)
            ));
        }
    }
    let due = backoff_due(wl).filter(|due| hold.is_none_or(|h| h < *due));
    land_within(
        seen,
        (
            hold.map_or(cleared, |h| h.max(cleared)),
            LAND_WITHIN_MS,
            due,
        ),
        t0,
        (&mut said, &mut bad),
    );
    said.push(woke_said(seen, sw.at, t0));
    verdict(&said, &bad)
}

/// SWF: Land then Off within [`FLIP_WITHIN_MS`] in its back-off at rest: its wake journaled,
/// nothing typed from the flip on, `autopilot=off` standing, watched past the back-off's due.
#[must_use]
pub fn judge_flip(seen: &Seen, t0: i64) -> Verdict {
    let [land, off] = seen.cover.words.as_slice() else {
        return Verdict::Unobserved(format!(
            "the flip was never made ({} write(s) of the word)",
            seen.cover.words.len()
        ));
    };
    if land.word != "land" || off.word != "off" {
        return Verdict::Unobserved(format!(
            "the flip wrote {} then {}, not land then off",
            land.word, off.word
        ));
    }
    if off.at - land.at > FLIP_WITHIN_MS {
        return Verdict::Unobserved(format!(
            "the flip's writes went in {} apart, past {} ms",
            at(0, off.at - land.at),
            FLIP_WITHIN_MS
        ));
    }
    let Some((tw, wl)) = rest_from(seen, t0).filter(|(t, _)| *t < land.at) else {
        return Verdict::Fail(format!(
            "flipped at +{}, but no back-off at rest stood before it",
            at(t0, land.at)
        ));
    };
    let mut said = vec![
        format!("+{} {}", at(t0, *tw), one_line(wl, 100)),
        format!(
            "land at +{} then off {} ms after it",
            at(t0, land.at),
            off.at - land.at
        ),
    ];
    let mut bad = Vec::new();
    let typed = acts_between(seen, t0, i64::MAX);
    if !typed.is_empty() {
        bad.push(format!("typed: {}", named(&typed, t0)));
    }
    said.push(wake_within(seen, land.at, t0, &mut bad));
    match seen.last_status("autopilot").as_deref() {
        Some("off") => said.push("status autopilot=off".into()),
        other => bad.push(format!(
            "the last status read autopilot={}, not off",
            other.unwrap_or("?")
        )),
    }
    if let Some((d, l)) = seen.first_after("DONE ", t0) {
        bad.push(format!("DONE at +{}: {}", at(t0, *d), one_line(l, 90)));
    }
    said.push(woke_said(seen, land.at, t0));
    let due = backoff_due(wl).unwrap_or(i64::MAX);
    if bad.is_empty() && seen.end_ms < due.saturating_add(PAST_DUE_MS) {
        return Verdict::Unobserved(format!(
            "{}: watched to +{}, not past the back-off's due at +{}",
            said.join("; "),
            at(t0, seen.end_ms),
            at(t0, due)
        ));
    }
    said.push(format!(
        "nothing typed to +{}, past the back-off's due at +{}",
        at(t0, seen.end_ms),
        at(t0, due)
    ));
    verdict(&said, &bad)
}

/// SWH: a tab held under Cruise (ruling F's same reply), switched to Land past its looks in front:
/// its wake journaled, nothing typed after the hold, no lift after the switch, and every `status`
/// read from [`SWITCH_SETTLE_MS`] past it says `autopilot=land` and the hold's arm (`held=`),
/// watched [`WATCH_MS`] past it.
#[must_use]
pub fn judge_held(seen: &Seen, t0: i64) -> Verdict {
    let Some((ts, sl)) = stall_of(seen, t0) else {
        return Verdict::Unobserved(format!("no hold within {}", at(t0, seen.end_ms)));
    };
    let Some(sw) = seen.cover.words.first() else {
        return Verdict::Unobserved(format!(
            "held at +{}, but the switch was never made",
            at(t0, *ts)
        ));
    };
    if sw.at <= *ts {
        return Verdict::Fail(format!(
            "switched at +{}, before the hold at +{}",
            at(t0, sw.at),
            at(t0, *ts)
        ));
    }
    let arm = arm_of(sl);
    let mut said = vec![
        format!("held at +{} ({})", at(t0, *ts), one_line(sl, 110)),
        format!("switched to {} at +{}", sw.word, at(t0, sw.at)),
    ];
    let mut bad = Vec::new();
    said.push(wake_within(seen, sw.at, t0, &mut bad));
    let typed = acts_between(seen, *ts, i64::MAX);
    if !typed.is_empty() {
        bad.push(format!("typed after the hold: {}", named(&typed, t0)));
    }
    if let Some((tl, ll)) = seen.journal.iter().find(|(t, l)| *t > sw.at && is_lift(l)) {
        bad.push(format!(
            "the hold lifted at +{}: {}",
            at(t0, *tl),
            one_line(ll, 110)
        ));
    }
    let reads = observed(
        &seen.status,
        &seen.status_reads,
        sw.at + SWITCH_SETTLE_MS,
        seen.end_ms + 1,
    );
    let off: Vec<String> = reads
        .iter()
        .filter(|(_, s)| {
            word(s, "autopilot").as_deref() != Some("land")
                || word(s, "held").as_deref() != Some(arm.word())
        })
        .map(|(t, s)| {
            format!(
                "+{} autopilot={} held={}",
                at(t0, *t),
                word(s, "autopilot").unwrap_or_else(|| "?".into()),
                word(s, "held").unwrap_or_else(|| "?".into())
            )
        })
        .collect();
    if reads.is_empty() {
        bad.push("no `status` read after the switch".into());
    } else if off.is_empty() {
        said.push(format!(
            "{} status read(s) after it: autopilot=land held={}",
            reads.len(),
            arm.word()
        ));
    } else {
        bad.push(format!("status after the switch: {}", off.join(", ")));
    }
    if bad.is_empty() && seen.end_ms - sw.at < WATCH_MS {
        return Verdict::Unobserved(format!(
            "{}: watched {} past the switch, under {} s",
            said.join("; "),
            at(0, seen.end_ms - sw.at),
            WATCH_MS / 1000
        ));
    }
    said.push(format!("nothing typed to +{}", at(t0, seen.end_ms)));
    said.push(woke_said(seen, sw.at, t0));
    verdict(&said, &bad)
}

/// SWH's window want: every state line the Harness page drew for the tab from
/// [`SWITCH_SETTLE_MS`] past the switch on leads with the hold's stop ([`stop_words`]' line).
#[must_use]
pub fn held_line(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some(sw) = seen.cover.words.first() else {
        return Verdict::Unobserved("the switch was never made".into());
    };
    let Some((_, sl)) = stall_of(seen, cx.t0) else {
        return Verdict::Unobserved("no hold before the switch".into());
    };
    let lead = stop_words(arm_of(sl), StopForm::Line);
    let w = &seen.window;
    let rows = observed(
        &w.page,
        &w.page_reads,
        sw.at + SWITCH_SETTLE_MS,
        seen.end_ms + 1,
    );
    let lines: Vec<(i64, &str)> = rows
        .iter()
        .filter(|(_, p)| *p != "-")
        .filter_map(|(t, p)| Some((*t, page_field(p, "state")?)))
        .collect();
    if rows.is_empty() {
        return Verdict::Unobserved("the Harness page was not read after the switch".into());
    }
    let Some((t1, l1)) = lines.first() else {
        return Verdict::Unobserved(format!(
            "its row drew no state line in the {} read(s) after the switch",
            rows.len()
        ));
    };
    match lines.iter().find(|(_, l)| !l.starts_with(&lead)) {
        Some((t, l)) => Verdict::Fail(format!(
            "+{} the state line \"{}\" leads with no stop ({lead:?})",
            at(cx.t0, *t),
            one_line(l, 200)
        )),
        None => Verdict::Pass(format!(
            "{} state line(s) from +{} lead with the stop: \"{}\"",
            lines.len(),
            at(cx.t0, *t1),
            one_line(l1, 160)
        )),
    }
}

/// The turn's point after `t`: its first `EVENT idle` with words.
fn point_after(seen: &Seen, t: i64) -> Option<&Line> {
    seen.journal
        .iter()
        .find(|(at, l)| *at > t && idle_words(l).is_some_and(|w| !w.is_empty() && w != "-"))
}

/// SWB: switched [`BUSY_SWITCH_MS`] into a long busy turn, before its point: nothing typed until
/// the point, the switch's wake after it, land@v1 within [`BUSY_LAND_MS`] of it.
#[must_use]
pub fn judge_busy(seen: &Seen, t0: i64) -> Verdict {
    let Some(sw) = seen.cover.words.first() else {
        return Verdict::Unobserved("the switch was never made".into());
    };
    if let Some((tp, _)) = point_after(seen, t0).filter(|(t, _)| *t <= sw.at) {
        return Verdict::Unobserved(format!(
            "the turn's point at +{} came before the switch at +{}",
            at(t0, *tp),
            at(t0, sw.at)
        ));
    }
    let mut said = vec![format!(
        "switched to {} at +{} (phase={})",
        sw.word,
        at(t0, sw.at),
        sw.phase
    )];
    let mut bad = Vec::new();
    let before = acts_between(seen, t0, sw.at);
    if !before.is_empty() {
        bad.push(format!("typed before the switch: {}", named(&before, t0)));
    }
    let Some((tp, pl)) = point_after(seen, sw.at) else {
        bad.push(format!("no point after it within {}", at(t0, seen.end_ms)));
        return verdict(&said, &bad);
    };
    said.push(format!(
        "the turn's point at +{} ({})",
        at(t0, *tp),
        one_line(pl, 90)
    ));
    let mid = acts_between(seen, sw.at, *tp);
    if !mid.is_empty() {
        bad.push(format!("typed mid-turn: {}", named(&mid, t0)));
    }
    let land = land_within(seen, (*tp, BUSY_LAND_MS, None), t0, (&mut said, &mut bad));
    match wake_after(seen, sw.at) {
        Some((tn, ln)) if land.is_none_or(|tl| *tn <= tl) => {
            said.push(format!("woken +{} ({})", at(t0, *tn), one_line(ln, 120)))
        }
        Some((tn, _)) => bad.push(format!("its wake at +{} came after land@v1", at(t0, *tn))),
        None => bad.push(format!(
            "no wake journaled for it (`NOTE … {WORD_MOVED}`) within {}",
            at(t0, seen.end_ms)
        )),
    }
    said.push(woke_said(seen, sw.at, t0));
    verdict(&said, &bad)
}

/// Whether a switch chain's watch is over: a landing one [`WATCH_MS`]'s third past its landing, or
/// [`LAND_WATCH_MS`] past its last step; `switch-flip` [`WATCH_MS`] past its flip and past its
/// back-off's due; `switch-held` [`WATCH_MS`] past its switch.
#[must_use]
pub fn watched(chain: Chain, seen: &Seen, now: i64) -> bool {
    let landed = seen
        .first_after("DONE ", i64::MIN)
        .is_some_and(|(d, _)| now - d >= WATCH_MS / 3);
    let since = |t: Option<i64>, ms: i64| t.is_some_and(|t| now - t >= ms);
    let first = seen.cover.words.first().map(|w| w.at);
    match chain {
        Chain::SwitchAgent | Chain::SwitchBusy => landed || since(first, LAND_WATCH_MS),
        Chain::SwitchSeq => {
            landed
                || since(
                    seen.cover.draft.as_ref().and_then(|d| d.cleared_ms),
                    LAND_WATCH_MS,
                )
        }
        Chain::SwitchFlip => {
            let due = rest_from(seen, i64::MIN)
                .and_then(|(_, l)| backoff_due(l))
                .unwrap_or(i64::MIN);
            since(seen.cover.words.last().map(|w| w.at), WATCH_MS) && now >= due + PAST_DUE_MS
        }
        Chain::SwitchHeld => since(first, WATCH_MS),
        _ => false,
    }
}

/// Writes `words` to tab `tab`'s Autopilot in turn, as fast as the socket takes them, each kept in
/// [`super::Cover::words`].
fn set(door: &Door<'_>, tab: &mut Tab, words: &[&'static str]) {
    let phase = word(&on(door, &tab.sid, &["status"]), "phase").unwrap_or_else(|| "?".into());
    for w in words {
        let t = now_ms();
        let reply = on(door, &tab.sid, &["meta", "set", "autopilot", w]);
        println!(
            "  [{}] +{} word    meta set autopilot {w}: {} (phase={phase})",
            tab.chain.name(),
            at(tab.t0, t),
            one_line(reply.trim(), 120)
        );
        tab.seen.cover.words.push(Switched {
            at: t,
            word: w,
            reply: reply.trim().to_string(),
            phase: phase.clone(),
        });
    }
}

/// THE WORD STEP: each switch chain's writes of its tab's word, the person's switch, at its time
/// (`switch-seq`'s draft typed and cleared around it): none for any other chain.
///
/// # Errors
/// The run was interrupted.
pub(in crate::forever) fn word_step(
    door: &Door<'_>,
    h: Harness,
    tab: &mut Tab,
    now: i64,
) -> Result<(), Fail> {
    if !switches(tab.chain) {
        return Ok(());
    }
    let s = &tab.seen;
    let rest = rest_from(s, tab.t0).map(|(t, _)| *t);
    let unset = s.cover.words.is_empty();
    match tab.chain {
        Chain::SwitchAgent if unset && rest.is_some_and(|t| now >= t + SWITCH_AFTER_MS) => {
            set(door, tab, &["land"]);
        }
        Chain::SwitchFlip if unset && rest.is_some_and(|t| now >= t + SWITCH_AFTER_MS) => {
            set(door, tab, &["land", "off"]);
        }
        Chain::SwitchHeld
            if unset && stall_of(s, tab.t0).is_some_and(|(t, _)| now >= t + HELD_SWITCH_MS) =>
        {
            set(door, tab, &["land"]);
        }
        Chain::SwitchBusy
            if unset && now >= tab.t0 + BUSY_SWITCH_MS && point_after(s, tab.t0).is_none() =>
        {
            set(door, tab, &["land"]);
        }
        Chain::SwitchSeq => draft_step(door, h, tab, now, rest)?,
        _ => {}
    }
    Ok(())
}

/// `switch-seq`'s steps: the draft typed [`DRAFT_AFTER_MS`] into its back-off at rest (through the
/// control socket; the loop takes a draft it did not type as a person's typing all the same, and
/// holds its hands off for their grace from its sighting, [`PERSON_HOLD`]), the switch
/// [`DRAFT_BEFORE_MS`] after it stood, and the draft cleared by backspaces [`DRAFT_PAST_MS`] past
/// the switch.
fn draft_step(
    door: &Door<'_>,
    h: Harness,
    tab: &mut Tab,
    now: i64,
    rest: Option<i64>,
) -> Result<(), Fail> {
    let agent = h.name();
    let standing = |text: &str| at_composer(door, &tab.sid, agent).as_deref() == Some(text);
    match tab.seen.cover.draft.clone() {
        None if rest.is_some_and(|t| now >= t + DRAFT_AFTER_MS) => {
            let typed = now_ms();
            let r = door.ctl(&[&format!("@{}", tab.sid), "send", DRAFT])?;
            let stood = r.code == 0
                && crate::wait_until(std::time::Duration::from_secs(10), || standing(DRAFT));
            println!(
                "  [{}] +{} draft   \"{DRAFT}\" sent ({}): {}",
                tab.chain.name(),
                at(tab.t0, typed),
                one_line(r.stdout.trim(), 60),
                if stood {
                    "it stands in the composer"
                } else {
                    "it did NOT stand in the composer"
                }
            );
            tab.seen.cover.draft = Some(Draft {
                typed_ms: typed,
                stood,
                ..Draft::default()
            });
        }
        Some(d) if d.stood && tab.seen.cover.words.is_empty() => {
            if now >= d.typed_ms + DRAFT_BEFORE_MS {
                set(door, tab, &["land"]);
            }
        }
        Some(d)
            if d.cleared_ms.is_none()
                && tab
                    .seen
                    .cover
                    .words
                    .first()
                    .is_some_and(|w| now >= w.at + DRAFT_PAST_MS) =>
        {
            let who = format!("@{}", tab.sid);
            let before = at_composer(door, &tab.sid, agent);
            for _ in 0..DRAFT.chars().count() {
                door.ctl(&[&who, "key", "backspace"])?;
            }
            let cleared = now_ms();
            let emptied = crate::wait_until(std::time::Duration::from_secs(10), || standing(""));
            println!(
                "  [{}] +{} draft   cleared by {} backspace(s) (it read {:?} before): {}",
                tab.chain.name(),
                at(tab.t0, cleared),
                DRAFT.chars().count(),
                before.unwrap_or_default(),
                if emptied {
                    "the composer stands empty"
                } else {
                    "the composer did NOT stand empty"
                }
            );
            if let Some(d) = tab.seen.cover.draft.as_mut() {
                d.cleared_ms = Some(cleared);
                d.emptied = emptied;
            }
        }
        _ => {}
    }
    Ok(())
}

/// THE TAPE'S WITNESS: the waits at rest a tape's text ([`text::render`]) shows carrying
/// `autopilot=<word>` and answered `OK autopilot <word>`, in its order.
#[must_use]
pub fn woke_of(text: &str) -> Vec<Woke> {
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(head) = lines.next() {
        let Some(head) = head.strip_prefix("exchange ctl") else {
            continue;
        };
        let at = head
            .split_whitespace()
            .find_map(|w| w.strip_prefix("stamp="))
            .and_then(|s| s.split(',').nth(1))
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(0);
        let mut args: Vec<&str> = Vec::new();
        let mut reply: Option<&str> = None;
        while let Some(l) = lines.next_if(|l| l.starts_with(['>', '<', '!', '|', '='])) {
            if let Some(a) = l.strip_prefix("> ") {
                args.push(a);
            } else if let Some(r) = l.strip_prefix("| ") {
                reply.get_or_insert(r);
            }
        }
        let Some(i) = args.iter().position(|a| *a == "await") else {
            continue;
        };
        let carried = args.iter().find(|a| a.starts_with("autopilot="));
        match (carried, reply) {
            (Some(c), Some(r)) if r.starts_with("OK autopilot") => out.push(Woke {
                at,
                form: args.get(i + 1).copied().unwrap_or("?").to_string(),
                carried: (*c).to_string(),
                answered: r.trim().to_string(),
            }),
            _ => {}
        }
    }
    out
}

/// The waits at rest the tapes of session `sid` show woken by a switch ([`woke_of`]), every tape
/// of the session read as far as it is whole; `None` where no tape could be read.
#[must_use]
pub(in crate::forever) fn woke_witness(run: &Run, sid: &str) -> Option<Vec<Woke>> {
    let dir = store::session_dir(&store::store_under(&run.state()), Some(sid));
    woke_in(&dir)
}

/// [`woke_witness`] over the tapes in `dir`.
#[must_use]
pub fn woke_in(dir: &Path) -> Option<Vec<Woke>> {
    let mut out = Vec::new();
    let mut read = false;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "atape") {
            continue;
        }
        let Some(tape) = std::fs::read(&path)
            .ok()
            .and_then(|raw| show::open(&raw, true).ok())
        else {
            continue;
        };
        read = true;
        out.extend(woke_of(&text::render(None, &tape.items)));
    }
    out.sort_by_key(|w| w.at);
    read.then_some(out)
}

/// [`Woke`]s for the run's report, from `t0`.
#[must_use]
pub fn woke_line(woke: Option<&[Woke]>, t0: i64) -> String {
    match woke {
        None => "no tape of the session read".to_string(),
        Some([]) => "no wait at rest carried autopilot= and woke on another word".to_string(),
        Some(w) => w
            .iter()
            .map(|w| {
                format!(
                    "+{} await {} … {} answered {}",
                    at(t0, w.at),
                    w.form,
                    w.carried,
                    w.answered
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}

#[cfg(test)]
#[path = "forever_switch_tests.rs"]
mod tests;
