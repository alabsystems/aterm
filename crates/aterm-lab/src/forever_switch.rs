// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LAND ACTS AT ONCE (v6 §14.10's 10a): what `lab forever` wants of Land's instruction once only
//! a back-off stood before it.
//!
//! * On the measured pass's Land chains ([`at_once`]) aterm's own share of land@v1, from the first
//!   point to the Enter it pressed, is within [`AT_ONCE_MS`] ([`land_at_once`]), timed on the
//!   session's tapes ([`typed_witness`]): everything but how long the program demonstrably held
//!   the text unshown, which is said beside it as its own number. Main typed it at +124.3 s: the
//!   first short turn's 2-minute back-off was waited out before the instruction.
//! * `--mode switch` ([`Chain::SWITCH`]) switches a running Cruise tab's word with `meta set
//!   autopilot land`, as a chip click, the Autopilot menu or Settings' row does ([`word_step`]):
//!   - `switch-agent`: at rest in its first short turn's back-off, switched [`SWITCH_AFTER_MS`]
//!     in: land@v1 within [`LAND_WITHIN_MS`] of the switch and before the back-off's due
//!     ([`judge_at_rest`]);
//!   - `switch-seq`: the same under a draft a person leaves in the composer over the switch:
//!     nothing typed while it stands nor in their grace from its clearing (their last key),
//!     land@v1 within [`LAND_WITHIN_MS`] of that grace's end, and no new point journaled from
//!     its typing to that act, the turn above the composer unchanged ([`judge_draft`]);
//!   - `switch-flip`: Land then Off within [`FLIP_WITHIN_MS`]: nothing typed, past the
//!     back-off's due ([`judge_flip`]);
//!   - `switch-over-held`: its replies repeated under Cruise ([`repeat_of`]: Cruise's wait before
//!     its request for a different approach since the continuous-forward design of 2026-11-01,
//!     or an older build's hold), switched to Land by `meta set`, no person's (limit 1: nothing
//!     typed; Land holds there), back to Cruise, then by the person's press of Land on Settings ▸
//!     Harness's SESSIONS row ([`press`], meta `autopilot_by=human:settings`), their turn (v6
//!     §14.17 item 3), all before Cruise's request falls due: land@v1 once, opened by the why
//!     quoting the repeated reply, `status land_asked=switch` and the state line's
//!     [`acts::SWITCH_ASKED`] while its answer is out, held by Land at the same answer and
//!     nothing typed past it ([`judge_over_held`], [`asked_line`]);
//!   - `switch-busy`: switched [`BUSY_SWITCH_MS`] into a long busy turn: nothing typed mid-turn,
//!     land@v1 within [`BUSY_LAND_MS`] of the turn's point ([`judge_busy`]);
//!   - `command-draft` (no switch, v6 §14.17.5.8): a person's command ([`command_draft`]: a typed
//!     [`COMMAND_DRAFT`], agy's pasted [`AGY_COMMAND_DRAFT`]) put in at rest and left standing
//!     [`COMMAND_PAST_MS`] past their grace: nothing typed or sent while it stands and its hold
//!     journalled once ([`turn_end::COMMAND_STANDS`]); cleared, the loop's next act comes past the
//!     grace from the clearing, never the command ([`judge_command`]).
//!
//! A wait at rest the switch wakes is journaled by the loop as `NOTE seq=<n> autopilot=<w> the
//! session's Autopilot word moved: decided after a <ms> ms settle` ([`wake_after`]), within
//! [`WAKE_WITHIN_MS`] of a switch at rest; the session's tapes say which wait carried the word
//! ([`woke_witness`]: `await agent|seq|gone … autopilot=cruise`, answered `OK autopilot land`).
//!
//! A write of the word `meta set` did not take (refused, or its client failed) is no switch: the
//! chain fails on it, its reply named ([`untaken`]).

use std::path::Path;

use aterm_agent::harness::acts::{self, StopForm, stop_words};
use aterm_agent::supervise::approvals::RULE_POINT;
use aterm_agent::supervise::policy::turn_end;
use aterm_agent::supervise::tape::show::Typed;
use aterm_agent::supervise::tape::{show, store, text};

use super::surfaces::held::{arm_of, is_stall};
use super::surfaces::{Ctx, compose, observed, page_field};
use super::{
    AT_DONE_MS, Chain, Harness, Line, Scenario, Seen, Tab, Verdict, act_rule, at, at_composer,
    backoff_due, backoff_named, backoff_streak, idle_words, on, recovery_wait, word,
};
use crate::Fail;
use crate::door::{Door, Reply};
use crate::run::Run;
use crate::util::{now_ms, one_line};

/// How long after the first point land@v1 may come on the measured pass's Land chains: one
/// decision and the instruction's typing, never a back-off.
pub const AT_ONCE_MS: i64 = 10_000;

/// The want [`land_at_once`] composes under ([`with_at_once`]).
pub const AT_ONCE: &str =
    "land@v1 at once: aterm's own share of it within 10 s of the first point (main: +124.3 s)";

/// How far into its back-off a tab at rest is switched.
pub const SWITCH_AFTER_MS: i64 = 10_000;

/// How far into its back-off `switch-seq`'s draft is typed.
pub const DRAFT_AFTER_MS: i64 = 5_000;

/// How long after the draft is typed the switch comes, once it stands.
pub const DRAFT_BEFORE_MS: i64 = 5_000;

/// How long the draft stands past the switch before the lab clears it.
pub const DRAFT_PAST_MS: i64 = 10_000;

/// `command-draft`'s draft under Claude Code and Muse Code ([`command_draft`]): a command a person
/// might leave in the composer, harmless where a build sends it (each shows its help and runs no
/// turn), as `/clear` or `/logout` would not be.
pub const COMMAND_DRAFT: &str = "/help";

/// `command-draft`'s draft under agy ([`command_draft`]): a `!` line pasted, which agy 1.4.12's `>`
/// composer keeps as text (measured: `agy::fixtures::lab::PASTED_BANG_DRAFT`), harmless where a
/// build sends it (`true` does nothing). A typed `/help` is no negative control there: agy's `/`
/// menu leaves main's read of it unauthoritative, so main decides nothing on it and sends nothing;
/// a typed `!` opens agy's bash mode, which draws no composer (v6 §14.17.5.8).
pub const AGY_COMMAND_DRAFT: &str = "!true";

/// How `command-draft` puts its command in under `h`: the text, and whether it goes in as one
/// bracketed paste (agy's, which no person's key stamps) rather than typed `as=person`.
#[must_use]
pub const fn command_draft(h: Harness) -> (&'static str, bool) {
    match h {
        Harness::Claude | Harness::Muse => (COMMAND_DRAFT, false),
        Harness::Agy => (AGY_COMMAND_DRAFT, true),
    }
}

/// How long past the person's grace from the command's typing the lab leaves it standing before it
/// clears it: past the decision the grace's end owes, and the hold that decision journals.
pub const COMMAND_PAST_MS: i64 = 30_000;

/// The words ending the `WAITING` line the loop journals for the person who typed a draft: its
/// hands-off hold, until their grace runs out.
pub const PERSON_HOLD: &str = turn_end::PERSON_TYPING;

/// How long past the person's grace from the clearing the loop's hold may run: the loop reads
/// their last key at its next look, every few seconds at rest, later under load.
pub const CLEAR_READ_MS: i64 = 15_000;

/// How much sooner than the lab's clock the loop's hold may end: the lab stamps the clearing once
/// its last backspace's call returned, after the window stamped that key, and the journal's
/// `until` is to the second.
pub const HOLD_SKEW_MS: i64 = 2_000;

/// The person's grace the forever lab's loop holds its hands off for (`[harness] human_grace_s`,
/// the policy's default: the lab sets none), in ms.
#[must_use]
pub fn person_grace_ms() -> i64 {
    i64::from(aterm_agent::supervise::SupervisorConfig::default().human_grace_s) * 1000
}

/// `switch-seq`'s draft: plain words with no key a composer reads as a shortcut of its own.
pub const DRAFT: &str = "a note for later";

/// The verdict's words for a point the loop journaled (`EVENT idle`) between the draft and the
/// next act: the draft read as the agent's turn ending again.
pub const DRAFT_REPOINTED: &str = "the draft read as a new point (ATERM-KI-1101-2)";

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

/// How long past its repeat ([`repeat_of`]) `switch-over-held` is switched: well inside Cruise's
/// wait before its request (the ladder's step for three short turns, 8 min), with time for the
/// state line to say the repeat.
pub const HELD_SWITCH_MS: i64 = 50_000;

/// How long past the switch a read may still show the words from before it: the loop's one-second
/// settle and a look.
pub const SWITCH_SETTLE_MS: i64 = 3_000;

/// How long a chain that types nothing after its switch is watched past it.
pub const WATCH_MS: i64 = 60_000;

/// How long `switch-over-held` watches its tab past `meta set`'s switch to Land, no person's,
/// before switching it back to Cruise: six times [`LAND_WITHIN_MS`], the bound its ask comes in
/// at the person's switch.
pub const CTL_WATCH_MS: i64 = 30_000;

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
    /// What `meta set` answered; where it did not take the word, how it failed (its exit status
    /// and both streams, or why its client could not run).
    pub reply: String,
    /// Whether `meta set` took the word ([`taken`]): a write it did not is no switch
    /// ([`untaken`]).
    pub took: bool,
    /// `status phase=` just before it.
    pub phase: String,
    /// Whether it was the person's press on Settings ▸ Harness ([`press`]), not `meta set`.
    pub person: bool,
}

/// `switch-seq`'s draft ([`word_step`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    /// What went in: `command-draft`'s command ([`command_draft`]); empty for `switch-seq`'s.
    pub text: String,
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
            | Chain::CommandDraft
            | Chain::SwitchFlip
            | Chain::SwitchOverHeld
            | Chain::SwitchBusy
    )
}

/// Whether `chain` is a Land chain of the measured pass, whose instruction is due at its first
/// point ([`land_at_once`]).
#[must_use]
pub fn at_once(chain: Chain) -> bool {
    Chain::ALL.contains(&chain) && chain.autopilot() == "land"
}

/// LAND@V1 AT ONCE, BY ATERM'S OWN SHARE: the first land@v1 the session's tapes show the loop
/// writing ([`Cover::typed`](super::Cover::typed)), judged from the first point judged from `t0`
/// on (`point@v1`). Aterm's share is the point to the Enter it pressed, less the time the program
/// demonstrably held the text unshown ([`Typed::aterm_ms`]): the decision, the write, the looks
/// and settles after the draw and the Enter are all aterm's, and must come within [`AT_ONCE_MS`];
/// the program's hold is said beside it ([`held_said`]) and judges nothing. A text the program
/// drew that aterm never sent fails; one it never drew by the watch's end has the write judged.
/// The back-off that stood before a late act is named. Where no tape shows a land@v1 written (no
/// tape read, or tapes with no such write), the journal's `CONTINUED` line is the clock, the
/// program's hold within it, so that fallback can only fail a program's stall, never pass aterm's.
#[must_use]
pub fn land_at_once(seen: &Seen, t0: i64) -> Verdict {
    let Some(point) = seen
        .ledger
        .iter()
        .find(|r| r.rule == RULE_POINT && r.ts >= t0)
    else {
        return Verdict::Unobserved(format!("no point judged within {}", at(t0, seen.end_ms)));
    };
    let line = seen.acts(turn_end::RULE_LAND).first().map(|(t, _)| *t);
    let wrote = seen.cover.typed.as_deref().and_then(|typed| {
        typed
            .iter()
            .find(|a| a.rule.as_deref() == Some(turn_end::RULE_LAND) && a.sent_ms >= t0)
    });
    let after = |t: i64| {
        let gap = t - point.ts;
        format!(
            "{} {} the first point at +{}",
            at(0, gap.abs()),
            if gap < 0 { "before" } else { "after" },
            at(t0, point.ts)
        )
    };
    let waited = |until: i64| {
        seen.journal
            .iter()
            .find(|(t, l)| *t >= point.ts - AT_DONE_MS && *t < until && backoff_named(l))
            .map_or_else(String::new, |(t, l)| {
                format!(", waiting out +{} {}", at(t0, *t), one_line(l, 110))
            })
    };
    let past = format!(": past {} s", AT_ONCE_MS / 1000);
    let Some(a) = wrote else {
        let Some(tl) = line else {
            return Verdict::Fail(format!(
                "no land@v1 written within {}, the first point at +{}",
                at(t0, seen.end_ms),
                at(t0, point.ts)
            ));
        };
        let said = format!(
            "land@v1 at +{} ({}: the program's hold of the text is within it), {}",
            at(t0, tl),
            if seen.cover.typed.is_some() {
                "the journal's line, no write of it on the tapes"
            } else {
                "the journal's line, no tape read"
            },
            after(tl)
        );
        if (tl - point.ts).abs() <= AT_ONCE_MS {
            return Verdict::Pass(said);
        }
        return Verdict::Fail(format!("{said}{}{past}", waited(tl)));
    };
    let mut said = format!(
        "aterm wrote land@v1 at +{}{}, {}",
        at(t0, a.sent_ms),
        a.decided_ms
            .map_or_else(String::new, |d| format!(" (decided at +{})", at(t0, d))),
        after(a.sent_ms)
    );
    if (a.sent_ms - point.ts).abs() > AT_ONCE_MS {
        said.push_str(&format!("; {}", held_said(a, seen.end_ms, t0)));
        return Verdict::Fail(format!("{said}{}{past}", waited(a.sent_ms)));
    }
    match (a.aterm_ms(point.ts), a.enter_ms, a.drawn_ms) {
        (Some(share), Some(enter), _) => {
            // The point is the journal's line and the rest the tape's stamps, a few ms apart: a
            // share just under zero is said as 0.0 s.
            said.push_str(&format!(
                ", and sent it at +{}: aterm's own share {}; {}",
                at(t0, enter),
                at(0, share.max(0)),
                held_said(a, seen.end_ms, t0)
            ));
            if share <= AT_ONCE_MS {
                Verdict::Pass(said)
            } else {
                Verdict::Fail(format!("{said}{}{past}", waited(enter)))
            }
        }
        (_, _, Some(drawn)) => Verdict::Fail(format!(
            "{said}; the program drew it by +{}, and aterm had not sent it by +{}; {}",
            at(t0, drawn),
            at(t0, seen.end_ms),
            held_said(a, seen.end_ms, t0)
        )),
        _ => {
            said.push_str(&format!("; {}", held_said(a, seen.end_ms, t0)));
            Verdict::Pass(said)
        }
    }
}

/// THE PROGRAM'S HOLD OF A TEXT ATERM WROTE, as its own number: how long it demonstrably held the
/// text unshown and the read that first showed it, or the last read that still did not where none
/// did by the watch's `end`; and the longest `status` read that found the text unread.
#[must_use]
pub fn held_said(a: &Typed, end: i64, t0: i64) -> String {
    let mut out = match (a.drawn_ms, a.held_ms) {
        (Some(d), Some(h)) => format!(
            "the program held the text {} (first shown in the read at +{})",
            at(0, h),
            at(t0, d)
        ),
        _ => match a.unshown_ms {
            Some(u) => format!(
                "the program had drawn none of the text by +{} (its last read +{})",
                at(t0, end),
                at(t0, u)
            ),
            None => format!(
                "no read after the write showed the text by +{}",
                at(t0, end)
            ),
        },
    };
    if let Some(u) = &a.unread {
        out.push_str(&format!("; status +{} {}", at(t0, u.at_ms), u.said()));
    }
    out
}

/// [`Typed`]s for the run's report, from `t0`: each write, its decision, the program's draw and
/// hold, and its Enter.
#[must_use]
pub fn typed_line(typed: Option<&[Typed]>, t0: i64) -> String {
    match typed {
        None => "no tape of the session read".to_string(),
        Some([]) => "no text written on the tapes".to_string(),
        Some(t) => t
            .iter()
            .map(|a| {
                let mut s = format!(
                    "+{} {}",
                    at(t0, a.sent_ms),
                    a.rule.as_deref().unwrap_or("?")
                );
                if let Some(d) = a.decided_ms {
                    s.push_str(&format!(" decided +{}", at(t0, d)));
                }
                match (a.drawn_ms, a.held_ms, a.unshown_ms) {
                    (Some(d), Some(h), _) => {
                        s.push_str(&format!(", drawn +{} (held {})", at(t0, d), at(0, h)))
                    }
                    (_, _, Some(u)) => s.push_str(&format!(", undrawn at +{}", at(t0, u))),
                    _ => s.push_str(", no read after it"),
                }
                if let Some(u) = &a.unread {
                    s.push_str(&format!(", status +{} {}", at(t0, u.at_ms), u.said()));
                }
                if let Some(e) = a.enter_ms {
                    s.push_str(&format!(", Enter +{}", at(t0, e)));
                }
                s
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// The texts the tapes of session `sid` show the loop writing ([`show::typed`]), every tape of the
/// session read as far as it is whole, in the order written; `None` where no tape could be read.
#[must_use]
pub(in crate::forever) fn typed_witness(run: &Run, sid: &str) -> Option<Vec<Typed>> {
    let dir = store::session_dir(&store::store_under(&run.state()), Some(sid));
    typed_in(&dir)
}

/// [`typed_witness`] over the tapes in `dir`.
#[must_use]
pub fn typed_in(dir: &Path) -> Option<Vec<Typed>> {
    let mut out = Vec::new();
    let mut read = false;
    for tape in tapes_in(dir)? {
        read = true;
        out.extend(show::typed(&tape));
    }
    out.sort_by_key(|a| a.sent_ms);
    read.then_some(out)
}

/// The tapes in `dir`, each read as far as it is whole; `None` where the folder cannot be read.
fn tapes_in(dir: &Path) -> Option<Vec<show::Tape>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "atape") {
            continue;
        }
        if let Some(tape) = crate::util::read_regular_file(&path)
            .ok()
            .and_then(|raw| show::open(&raw, true).ok())
        {
            out.push(tape);
        }
    }
    Some(out)
}

/// Folds [`land_at_once`] into the verdict `v` of the first scenario of a chain [`at_once`] names;
/// every other verdict as it was.
#[must_use]
pub fn with_at_once(s: Scenario, seen: &Seen, cx: &Ctx<'_>, v: Verdict) -> Verdict {
    if !at_once(cx.chain) || cx.chain.scenarios().first() != Some(&s) {
        return v;
    }
    let v = compose(v, AT_ONCE, land_at_once(seen, cx.t0));
    let v = compose(v, LAND_ONCE, land_once(seen, cx.t0));
    // The note is judged only where the tapes show the write and the look after it; elsewhere
    // the want is named as skipped, and the chain's word stands on the rest.
    match held_noted(seen, cx.t0) {
        Verdict::Unobserved(why) => {
            let skip = format!("; {HELD_NOTED}: SKIPPED ({why})");
            match v {
                Verdict::Pass(w) => Verdict::Pass(w + &skip),
                Verdict::Fail(w) => Verdict::Fail(w + &skip),
                Verdict::Unobserved(w) => Verdict::Unobserved(w + &skip),
            }
        }
        judged => compose(v, HELD_NOTED, judged),
    }
}

/// The want [`land_once`] composes under ([`with_at_once`]).
pub const LAND_ONCE: &str = "land@v1 never typed twice in a row";

/// LAND'S INSTRUCTION IS GIVEN ONCE (the at1 review, 2026-11-01): no land@v1 act from `t0` on is
/// followed by another with no act between, as one drawn late and sent by a later decision was,
/// typed again at the agent's `Nothing left:` in place of the land check.
#[must_use]
pub fn land_once(seen: &Seen, t0: i64) -> Verdict {
    let acts: Vec<&Line> = seen
        .all_acts()
        .into_iter()
        .filter(|(t, _)| *t >= t0)
        .collect();
    let land = |l: &Line| act_rule(&l.1) == Some(turn_end::RULE_LAND);
    if let Some(w) = acts.windows(2).find(|w| land(w[0]) && land(w[1])) {
        return Verdict::Fail(format!(
            "land@v1 typed at +{} and again at +{}, no act between",
            at(t0, w[0].0),
            at(t0, w[1].0)
        ));
    }
    let n = acts.iter().filter(|&&l| land(l)).count();
    Verdict::Pass(format!("land@v1 typed {n} time(s), never twice in a row"))
}

/// The want [`held_noted`] composes under ([`with_at_once`]).
pub const HELD_NOTED: &str =
    "a write its program left unshown past the first look is noted once, under its rule";

/// THE HELD TEXT'S NOTE, ONE PER WRITE (slice at1's item 3): the first land@v1 the tapes show
/// written from `t0` on gets exactly one `NOTE … rule=land@v1` naming the program as holding the
/// text where the first `await seq` past it ran out, and none where it saw the screen move,
/// however long the program then held the text.
#[must_use]
pub fn held_noted(seen: &Seen, t0: i64) -> Verdict {
    let Some(typed) = seen.cover.typed.as_deref() else {
        return Verdict::Unobserved("no tape of the session read".to_string());
    };
    let Some(a) = typed
        .iter()
        .find(|a| a.rule.as_deref() == Some(turn_end::RULE_LAND) && a.sent_ms >= t0)
    else {
        return Verdict::Unobserved("no land@v1 written on the tapes".to_string());
    };
    let Some(moved) = a.reaction else {
        return Verdict::Unobserved(format!(
            "no look past land@v1's write at +{} on the tapes",
            at(t0, a.sent_ms)
        ));
    };
    let until = typed
        .iter()
        .find(|b| b.sent_ms > a.sent_ms)
        .map_or(seen.end_ms, |b| b.sent_ms);
    let notes = seen
        .journal
        .iter()
        .filter(|(t, l)| {
            *t >= a.sent_ms
                && *t <= until
                && l.starts_with("NOTE ")
                && l.contains(aterm_agent::supervise::journal::TEXT_HELD)
                && l.split_whitespace().any(|w| w == "rule=land@v1")
        })
        .collect::<Vec<_>>();
    let want = usize::from(!moved);
    let said = format!(
        "the first look past the write at +{} {}; {} note(s){}",
        at(t0, a.sent_ms),
        if moved {
            "saw the screen move"
        } else {
            "ran out"
        },
        notes.len(),
        notes.first().map_or_else(String::new, |(t, l)| format!(
            ", +{} {}",
            at(t0, *t),
            one_line(l, 140)
        ))
    );
    if notes.len() == want {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: want {want}"))
    }
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

/// The points journaled (`EVENT <phase> seq=…`) from `from` to before `until`: a draft typed and
/// cleared in the composer is none, the turn above it unchanged (ATERM-KI-1101-2).
fn points_between(seen: &Seen, from: i64, until: i64) -> Vec<&Line> {
    seen.journal
        .iter()
        .filter(|(t, l)| *t >= from && *t < until && l.starts_with("EVENT "))
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

/// The first of the lab's writes of the word that `meta set` did not take, as a fail naming its
/// reply: a refused or failed write is no switch, so what followed it is never judged as one's.
fn untaken(seen: &Seen, t0: i64) -> Option<Verdict> {
    let w = seen.cover.words.iter().find(|w| !w.took)?;
    Some(Verdict::Fail(format!(
        "the write of {} at +{} did not take: {}",
        w.word,
        at(t0, w.at),
        one_line(&w.reply, 160)
    )))
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
    if let Some(v) = untaken(seen, t0) {
        return v;
    }
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

/// SWS: the same under a draft a person typed and cleared over the switch: its wake within
/// [`WAKE_WITHIN_MS`], nothing typed from the draft's typing to its clearing, nor inside the
/// person's grace from the clearing as the lab timed it ([`person_grace_ms`] from their last key),
/// a hands-off hold for them journaled ([`PERSON_HOLD`]) whose last `until` ends no sooner than
/// that grace and within [`CLEAR_READ_MS`] past it, and land@v1 within [`LAND_WITHIN_MS`] of its
/// end, past the back-off's due (the clearing comes after the back-off began, the grace as long
/// as it). The lab times the grace itself, so a run that typed in it fails whatever the loop
/// journaled.
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
    if let Some(v) = untaken(seen, t0) {
        return v;
    }
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
    let held_to = cleared + person_grace_ms();
    said.push(format!(
        "the person's grace from it to +{}",
        at(t0, held_to)
    ));
    let inside = acts_between(seen, cleared, held_to - HOLD_SKEW_MS);
    if !inside.is_empty() {
        bad.push(format!(
            "typed inside the person's grace from the clearing (to +{}): {}",
            at(t0, held_to),
            named(&inside, t0)
        ));
    }
    let next = acts_between(seen, cleared, i64::MAX)
        .first()
        .map_or(seen.end_ms, |(t, _)| *t);
    let points = points_between(seen, d.typed_ms, next);
    if !points.is_empty() {
        bad.push(format!(
            "{DRAFT_REPOINTED}: {}",
            points
                .iter()
                .map(|(t, l)| format!("+{} {}", at(t0, *t), one_line(l, 60)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let hold = seen
        .journal
        .iter()
        .filter(|(t, l)| {
            *t >= d.typed_ms && *t <= next && l.starts_with("WAITING ") && l.ends_with(PERSON_HOLD)
        })
        .filter_map(|(_, l)| {
            crate::workers::iso_ms(l.split_once(" until=")?.1.split_whitespace().next()?)
        })
        .next_back();
    let until = match hold {
        None => {
            bad.push(format!(
                "no hold for the person journaled (`WAITING … {PERSON_HOLD}`) from the draft's \
                 typing to +{}",
                at(t0, next)
            ));
            held_to
        }
        Some(h) => {
            said.push(format!("the hold for the person to +{}", at(t0, h)));
            if h < held_to - HOLD_SKEW_MS {
                bad.push(format!(
                    "the hold for the person ends at +{}, inside their grace from the clearing",
                    at(t0, h)
                ));
            } else if h > held_to + CLEAR_READ_MS {
                bad.push(format!(
                    "the hold for the person runs to +{}, {} past their grace from the clearing, \
                     past {} s",
                    at(t0, h),
                    at(0, h - held_to),
                    CLEAR_READ_MS / 1000
                ));
            }
            h.max(held_to)
        }
    };
    land_within(
        seen,
        (until, LAND_WITHIN_MS, None),
        t0,
        (&mut said, &mut bad),
    );
    said.push(woke_said(seen, sw.at, t0));
    verdict(&said, &bad)
}

/// SWC: A PERSON'S COMMAND STANDS (v6 §14.17.5.8). The harness's command ([`command_draft`]), put
/// in at rest in Cruise's first back-off and left standing past the person's grace from then:
/// nothing typed from its typing to its clearing (main's draft step sent it at the grace's end,
/// `CONTINUED … continue@v1 /help`), and its hold journalled once in that span, by the draft
/// step's command arm (`HOLD … why=command …`, [`turn_end::COMMAND_STANDS`]). Cleared by the
/// person, the loop goes on: its next act comes past the person's grace from the clearing, within
/// [`CLEAR_READ_MS`] of it, and is never the command.
#[must_use]
pub fn judge_command(seen: &Seen, t0: i64) -> Verdict {
    let Some(d) = seen.cover.draft.as_ref() else {
        return Verdict::Unobserved(format!(
            "no command was typed (no back-off at rest within {})",
            at(t0, seen.end_ms)
        ));
    };
    if !d.stood {
        return Verdict::Unobserved(format!(
            "the command typed at +{} never stood in the composer",
            at(t0, d.typed_ms)
        ));
    }
    let grace_end = d.typed_ms + person_grace_ms();
    let mut said = vec![
        format!("{} from +{}", d.text, at(t0, d.typed_ms)),
        format!("the person's grace to +{}", at(t0, grace_end)),
    ];
    let mut bad = Vec::new();
    let until = d.cleared_ms.unwrap_or(seen.end_ms);
    let over = acts_between(seen, d.typed_ms - 1, until);
    if !over.is_empty() {
        bad.push(format!(
            "typed while the command stood (to +{}): {}",
            at(t0, until),
            named(&over, t0)
        ));
    }
    let holds: Vec<&Line> = seen
        .journal
        .iter()
        .filter(|(t, l)| {
            *t >= d.typed_ms
                && *t <= until
                && l.starts_with("HOLD ")
                && l.ends_with(turn_end::COMMAND_STANDS)
        })
        .collect();
    match holds.as_slice() {
        [(th, lh)] => said.push(format!("held +{} ({})", at(t0, *th), one_line(lh, 100))),
        [] => bad.push(format!(
            "no hold journaled for the person's command (`HOLD … {}`) from +{} to +{}",
            one_line(turn_end::COMMAND_STANDS, 40),
            at(t0, d.typed_ms),
            at(t0, until)
        )),
        more => bad.push(format!("the hold journaled {} times", more.len())),
    }
    let Some(cleared) = d.cleared_ms.filter(|_| d.emptied) else {
        if bad.is_empty() {
            return Verdict::Unobserved(format!(
                "the command stood from +{}, but it was not cleared",
                at(t0, d.typed_ms)
            ));
        }
        return verdict(&said, &bad);
    };
    said.push(format!("cleared at +{}", at(t0, cleared)));
    let held_to = cleared + person_grace_ms();
    let inside = acts_between(seen, cleared, held_to - HOLD_SKEW_MS);
    if !inside.is_empty() {
        bad.push(format!(
            "typed inside the person's grace from the clearing (to +{}): {}",
            at(t0, held_to),
            named(&inside, t0)
        ));
    }
    match acts_between(seen, cleared, i64::MAX).first() {
        None => bad.push(format!(
            "nothing typed past the clearing within {}: the loop did not go on",
            at(t0, seen.end_ms)
        )),
        Some((ta, la)) => {
            said.push(format!("then +{} {}", at(t0, *ta), one_line(la, 80)));
            if *ta > held_to + CLEAR_READ_MS {
                bad.push(format!(
                    "the next act came {} past the grace from the clearing, past {} s",
                    at(0, ta - held_to),
                    CLEAR_READ_MS / 1000
                ));
            }
            if !d.text.is_empty() && la.trim_end().ends_with(d.text.as_str()) {
                bad.push(format!("the command was sent: +{}", at(t0, *ta)));
            }
        }
    }
    verdict(&said, &bad)
}

/// SWF: Land then Off within [`FLIP_WITHIN_MS`] in its back-off at rest, both writes taken: its
/// wake journaled, nothing typed from the flip on, `autopilot=off` standing, watched past the
/// back-off's due. A Land write that did not take would leave Off's wake, word and quiet to pass
/// for a flip that never raced, so it fails ([`untaken`]).
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
    if let Some(v) = untaken(seen, t0) {
        return v;
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

/// THE REPEAT `switch-over-held` SWITCHES OVER: the first point from `t0` its replies repeated at
/// under Cruise, the loop's wait before its request for a different approach
/// ([`recovery_wait`]), or on a build that held Cruise there (ruling F, before the
/// continuous-forward design of 2026-11-01) its hold. The replies hold the same reply at it
/// (`TurnEndState::held_by`), so the person's switch to Land there is their switch over a held
/// tab (v6 §14.17 item 3), though Cruise goes on.
#[must_use]
pub fn repeat_of(seen: &Seen, t0: i64) -> Option<&Line> {
    seen.journal
        .iter()
        .find(|(t, l)| *t >= t0 && (recovery_wait(l) || is_stall(l)))
}

/// Whether `journal`, a session's `drive/<sid>.journal.jsonl` text, holds `switch-over-held`'s
/// repeat point ([`repeat_of`]: Cruise's recovery wait, or an older build's hold): what
/// `autopilot-captures`' switch-asked scene waits for before the person's switch to Land.
#[must_use]
pub fn repeat_journaled(journal: &str) -> bool {
    super::parse_journal_lines(journal).any(|(_, l)| recovery_wait(&l) || is_stall(&l))
}

/// The person's switch to Land after the hold at `ts`: the first of the lab's presses from it on.
fn press_after(seen: &Seen, ts: i64) -> Option<&Switched> {
    seen.cover.words.iter().find(|w| w.at > ts && w.person)
}

/// The hold again after Land's one ask at `ta`: the first `kind=stalled` journaled past it.
#[must_use]
pub fn held_again(seen: &Seen, ta: i64) -> Option<&Line> {
    seen.journal.iter().find(|(t, l)| *t > ta && is_stall(l))
}

/// The reply the hold at `ts` stopped at: the last point's words journaled before it.
fn held_reply(seen: &Seen, ts: i64) -> Option<&str> {
    seen.journal
        .iter()
        .filter(|(t, _)| *t <= ts)
        .filter_map(|(_, l)| idle_words(l).filter(|w| !w.is_empty() && *w != "-"))
        .next_back()
}

/// SWO: a tab whose replies repeated under Cruise ([`repeat_of`]), switched to Land by `meta set`
/// and back to Cruise, then by the person's press, its stand-in giving Land's one ask the same
/// reply (v6 §14.17 item 3): nothing typed between the repeat and the person's press, the switch
/// no person made included (limit 1); its wake journaled within [`WAKE_WITHIN_MS`]; one land@v1
/// within [`LAND_WITHIN_MS`] of it, opened by the switch's why (`You stopped at: “…”`) quoting
/// the repeated reply; `status land_asked=switch` read while its answer was out; held by Land at
/// that answer (`kind=stalled`); and nothing typed past it, watched [`WATCH_MS`] past that hold,
/// every `status` from [`SWITCH_SETTLE_MS`] past it saying the hold's arm (`held=`) and no ask
/// (`land_asked=-`).
#[must_use]
pub fn judge_over_held(seen: &Seen, t0: i64) -> Verdict {
    let Some((ts, sl)) = repeat_of(seen, t0) else {
        return Verdict::Unobserved(format!("no repeated reply within {}", at(t0, seen.end_ms)));
    };
    if let Some(v) = untaken(seen, t0) {
        return v;
    }
    let ctl: Vec<&Switched> = seen
        .cover
        .words
        .iter()
        .filter(|w| w.at > *ts && !w.person)
        .collect();
    let Some(cl) = ctl.iter().find(|w| w.word == "land") else {
        return Verdict::Unobserved(format!(
            "repeated at +{}, but no switch to Land by `meta set` was made",
            at(t0, *ts)
        ));
    };
    let Some(sw) = press_after(seen, cl.at) else {
        return Verdict::Unobserved(format!(
            "repeated at +{}, switched to Land by `meta set` at +{}, but the person's switch was \
             never made",
            at(t0, *ts),
            at(t0, cl.at)
        ));
    };
    let mut said = vec![
        format!("repeated at +{} ({})", at(t0, *ts), one_line(sl, 110)),
        format!(
            "`meta set` {} (no person's)",
            ctl.iter()
                .filter(|w| w.at < sw.at)
                .map(|w| format!("{} at +{}", w.word, at(t0, w.at)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        format!("the person switched to {} at +{}", sw.word, at(t0, sw.at)),
    ];
    let mut bad = Vec::new();
    said.push(wake_within(seen, sw.at, t0, &mut bad));
    let at_ctl = acts_between(seen, cl.at, sw.at);
    if !at_ctl.is_empty() {
        bad.push(format!(
            "typed at the switch no person made (+{}): {}",
            at(t0, cl.at),
            named(&at_ctl, t0)
        ));
    }
    let before = acts_between(seen, *ts, cl.at);
    if !before.is_empty() {
        bad.push(format!("typed after the repeat: {}", named(&before, t0)));
    }
    let after = acts_between(seen, sw.at, i64::MAX);
    let Some(&ask) = after.first() else {
        bad.push(format!(
            "NO ASK: nothing typed after the switch within {}",
            at(t0, seen.end_ms)
        ));
        return verdict(&said, &bad);
    };
    let ta = ask.0;
    let rule = act_rule(&ask.1).unwrap_or("?");
    said.push(format!(
        "{rule} at +{}, {} after the switch",
        at(t0, ta),
        at(0, ta - sw.at)
    ));
    if rule != turn_end::RULE_LAND {
        bad.push(format!("the act after the switch was {rule}, not land@v1"));
    }
    if ta - sw.at > LAND_WITHIN_MS {
        bad.push(format!("past {} s of the switch", LAND_WITHIN_MS / 1000));
    }
    let text = seen.act_text(turn_end::RULE_LAND, ask);
    if turn_end::is_switch_ask(text) {
        said.push(format!("opened by the why: \"{}\"", one_line(text, 150)));
    } else {
        bad.push(format!(
            "land@v1 opens with no why: \"{}\"",
            one_line(text, 150)
        ));
    }
    if let Some(reply) = held_reply(seen, *ts) {
        // The journal's point keeps the reply's bullet (`⏺ `), which the loop's reply has not.
        let quote = turn_end::held_quote(reply);
        let head: String = quote
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .chars()
            .take(40)
            .collect();
        if !text.contains(&head) {
            bad.push(format!("its why quotes no \"{head}\" (the held reply)"));
        }
    }
    if after.len() > 1 {
        bad.push(format!(
            "typed again after the ask: {}",
            named(&after[1..], t0)
        ));
    }
    let Some((th, hl)) = held_again(seen, ta) else {
        bad.push(format!(
            "not held again after the ask within {}",
            at(t0, seen.end_ms)
        ));
        return verdict(&said, &bad);
    };
    said.push(format!(
        "held again at +{} ({})",
        at(t0, *th),
        one_line(hl, 110)
    ));
    let arm = arm_of(hl);
    let asking = observed(&seen.status, &seen.status_reads, ta, *th);
    if asking.is_empty() {
        bad.push("no `status` read while the ask stood".into());
    } else if let Some((t, _)) = asking
        .iter()
        .find(|(_, s)| word(s, "land_asked").as_deref() == Some("switch"))
    {
        said.push(format!("status land_asked=switch at +{}", at(t0, *t)));
    } else {
        bad.push(format!(
            "status never said land_asked=switch in {} read(s) while the ask stood: {}",
            asking.len(),
            asking
                .iter()
                .map(|(t, s)| format!(
                    "+{} land_asked={}",
                    at(t0, *t),
                    word(s, "land_asked").unwrap_or_else(|| "?".into())
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let reads = observed(
        &seen.status,
        &seen.status_reads,
        th + SWITCH_SETTLE_MS,
        seen.end_ms + 1,
    );
    let off: Vec<String> = reads
        .iter()
        .filter(|(_, s)| {
            word(s, "held").as_deref() != Some(arm.word())
                || word(s, "land_asked").as_deref() != Some("-")
        })
        .map(|(t, s)| {
            format!(
                "+{} held={} land_asked={}",
                at(t0, *t),
                word(s, "held").unwrap_or_else(|| "?".into()),
                word(s, "land_asked").unwrap_or_else(|| "?".into())
            )
        })
        .collect();
    if reads.is_empty() {
        bad.push("no `status` read after the second hold".into());
    } else if off.is_empty() {
        said.push(format!(
            "{} status read(s) after it: held={} land_asked=-",
            reads.len(),
            arm.word()
        ));
    } else {
        bad.push(format!("status after the second hold: {}", off.join(", ")));
    }
    if bad.is_empty() && seen.end_ms - th < WATCH_MS {
        return Verdict::Unobserved(format!(
            "{}: watched {} past the second hold, under {} s",
            said.join("; "),
            at(0, seen.end_ms - th),
            WATCH_MS / 1000
        ));
    }
    said.push(format!("nothing more typed to +{}", at(t0, seen.end_ms)));
    said.push(woke_said(seen, sw.at, t0));
    verdict(&said, &bad)
}

/// SWO's window want: a state line the Harness page drew for the tab while Land's one ask stood
/// leads with its words ([`acts::SWITCH_ASKED`]), and every one it drew from [`SWITCH_SETTLE_MS`]
/// past the second hold on leads with the hold's stop again ([`stop_words`]' line).
#[must_use]
pub fn asked_line(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((ts, _)) = repeat_of(seen, cx.t0) else {
        return Verdict::Unobserved("no repeated reply before the switch".into());
    };
    let Some(sw) = press_after(seen, *ts) else {
        return Verdict::Unobserved("the person's switch was never made".into());
    };
    if let Some(v) = untaken(seen, cx.t0) {
        return v;
    }
    let Some(ta) = acts_between(seen, sw.at, i64::MAX).first().map(|(t, _)| *t) else {
        return Verdict::Fail("NO ASK: nothing typed after the switch".into());
    };
    let Some((th, hl)) = held_again(seen, ta) else {
        return Verdict::Unobserved("not held again after the ask".into());
    };
    let w = &seen.window;
    let lines = |from: i64, until: i64| -> Vec<(i64, String)> {
        observed(&w.page, &w.page_reads, from, until)
            .into_iter()
            .filter(|(_, p)| *p != "-")
            .filter_map(|(t, p)| Some((t, page_field(p, "state")?.to_string())))
            .collect()
    };
    let asking = lines(ta, *th);
    let Some((t1, l1)) = asking
        .iter()
        .find(|(_, l)| l.starts_with(acts::SWITCH_ASKED))
    else {
        return if asking.is_empty() {
            Verdict::Unobserved("its row drew no state line while the ask stood".into())
        } else {
            Verdict::Fail(format!(
                "no state line in the {} drawn while the ask stood leads with \"{}\": +{} \"{}\"",
                asking.len(),
                acts::SWITCH_ASKED,
                at(cx.t0, asking[0].0),
                one_line(&asking[0].1, 200)
            ))
        };
    };
    // Held again under Land: no switch to make, so the stop names none.
    let lead = stop_words(arm_of(hl), StopForm::Line, false);
    let again = lines(th + SWITCH_SETTLE_MS, seen.end_ms + 1);
    if let Some((t, l)) = again.iter().find(|(_, l)| !l.starts_with(&lead)) {
        return Verdict::Fail(format!(
            "+{} after the second hold the state line \"{}\" leads with no stop ({lead:?})",
            at(cx.t0, *t),
            one_line(l, 200)
        ));
    }
    if again.is_empty() {
        return Verdict::Unobserved(format!(
            "+{} \"{}\"; no state line drawn after the second hold",
            at(cx.t0, *t1),
            one_line(l1, 160)
        ));
    }
    Verdict::Pass(format!(
        "+{} \"{}\"; {} state line(s) after the second hold lead with the stop: \"{}\"",
        at(cx.t0, *t1),
        one_line(l1, 160),
        again.len(),
        one_line(&again[0].1, 160)
    ))
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
    if let Some(v) = untaken(seen, t0) {
        return v;
    }
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
/// back-off's due; `switch-over-held` [`WATCH_MS`] past its second hold, or [`LAND_WATCH_MS`]
/// past its switch where no ask came and held again.
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
        Chain::CommandDraft => since(
            seen.cover.draft.as_ref().and_then(|d| d.cleared_ms),
            person_grace_ms() + CLEAR_READ_MS + LAND_WITHIN_MS,
        ),
        Chain::SwitchFlip => {
            let due = rest_from(seen, i64::MIN)
                .and_then(|(_, l)| backoff_due(l))
                .unwrap_or(i64::MIN);
            since(seen.cover.words.last().map(|w| w.at), WATCH_MS) && now >= due + PAST_DUE_MS
        }
        Chain::SwitchOverHeld => {
            let pressed = press_after(seen, i64::MIN).map(|w| w.at);
            let again = seen
                .all_acts()
                .into_iter()
                .find(|(t, _)| pressed.is_some_and(|p| *t > p))
                .and_then(|(ta, _)| held_again(seen, *ta))
                .map(|(t, _)| *t);
            since(again, WATCH_MS) || since(pressed, LAND_WATCH_MS)
        }
        _ => false,
    }
}

/// Whether `meta set`'s reply `r` took the word: its client exited 0 with an answer that is no
/// `ERR` (one it prints on stderr, exiting 1).
fn taken(r: &Reply) -> bool {
    let head = r
        .stdout
        .lines()
        .next()
        .or_else(|| r.stderr.lines().next())
        .unwrap_or("")
        .trim();
    r.code == 0 && !head.is_empty() && !head.starts_with("ERR")
}

/// Writes `words` to tab `tab`'s Autopilot in turn, as fast as the socket takes them, each kept in
/// [`super::Cover::words`] with whether `meta set` took it ([`taken`]); one it did not take is kept
/// with how it failed, never as a switch made.
fn set(door: &Door<'_>, tab: &mut Tab, words: &[&'static str]) {
    let phase = word(&on(door, &tab.sid, &["status"]), "phase").unwrap_or_else(|| "?".into());
    let who = format!("@{}", tab.sid);
    for w in words {
        let t = now_ms();
        let (took, reply) = match door.ctl(&[&who, "meta", "set", "autopilot", w]) {
            Ok(r) if taken(&r) => (true, r.stdout.trim().to_string()),
            Ok(r) => (
                false,
                format!(
                    "exit {}: {}",
                    r.code,
                    [r.stdout.trim(), r.stderr.trim()].join(" ").trim()
                ),
            ),
            Err(e) => (false, e),
        };
        println!(
            "  [{}] +{} word    meta set autopilot {w}: {}{} (phase={phase})",
            tab.chain.name(),
            at(tab.t0, t),
            if took { "" } else { "DID NOT TAKE: " },
            one_line(&reply, 120)
        );
        tab.seen.cover.words.push(Switched {
            at: t,
            word: w,
            reply,
            took,
            phase: phase.clone(),
            person: false,
        });
    }
}

/// THE PERSON'S PRESS of `w` on Settings ▸ Harness's SESSIONS row for `tab`
/// ([`super::surfaces::press_word`], on `page`, the desk's view, where one stands): the window
/// writes the word as the person's (`autopilot_by=human:settings`), which no `meta set` does. Kept
/// as [`set`] keeps its writes, with `person` set; one not pressed is kept as not taken, saying
/// why ([`untaken`]).
fn press(door: &Door<'_>, tab: &mut Tab, page: Option<&str>, w: &'static str) {
    let phase = word(&on(door, &tab.sid, &["status"]), "phase").unwrap_or_else(|| "?".into());
    let t = now_ms();
    let (took, reply) = match super::surfaces::press_word(door, page, &tab.sid, w) {
        Ok(said) => (true, said),
        Err(why) => (false, why),
    };
    println!(
        "  [{}] +{} word    the person's press of {w} on Settings ▸ Harness: {}{} (phase={phase})",
        tab.chain.name(),
        at(tab.t0, t),
        if took { "" } else { "DID NOT TAKE: " },
        one_line(&reply, 120)
    );
    tab.seen.cover.words.push(Switched {
        at: t,
        word: w,
        reply,
        took,
        phase,
        person: true,
    });
}

/// THE WORD STEP: each switch chain's writes of its tab's word, the person's switch, at its time
/// (`switch-seq`'s draft typed and cleared around it; `switch-over-held`'s by `meta set` and then
/// by the person's press on `page`, Settings ▸ Harness's view): none for any other chain.
///
/// # Errors
/// The run was interrupted.
pub(in crate::forever) fn word_step(
    door: &Door<'_>,
    h: Harness,
    tab: &mut Tab,
    now: i64,
    page: Option<&str>,
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
        Chain::SwitchOverHeld => {
            // `meta set`'s Land HELD_SWITCH_MS past the repeat, its Cruise CTL_WATCH_MS later,
            // then the person's press of Land.
            let held = repeat_of(s, tab.t0).map(|(t, _)| *t);
            match s.cover.words.as_slice() {
                [] if held.is_some_and(|t| now >= t + HELD_SWITCH_MS) => {
                    set(door, tab, &["land"]);
                }
                [ctl] if now >= ctl.at + CTL_WATCH_MS => set(door, tab, &["cruise"]),
                [_, back] if now >= back.at + SWITCH_SETTLE_MS => press(door, tab, page, "land"),
                _ => {}
            }
        }
        Chain::SwitchBusy
            if unset && now >= tab.t0 + BUSY_SWITCH_MS && point_after(s, tab.t0).is_none() =>
        {
            set(door, tab, &["land"]);
        }
        Chain::SwitchSeq => draft_step(door, h, tab, now, rest)?,
        Chain::CommandDraft => command_step(door, h, tab, now, rest)?,
        _ => {}
    }
    Ok(())
}

/// `command-draft`'s steps: the harness's command ([`command_draft`]) put in [`DRAFT_AFTER_MS`]
/// into the back-off at rest, typed `as=person` as `switch-seq`'s draft is ([`draft_step`]) or
/// pasted, and cleared by backspaces `as=person` [`COMMAND_PAST_MS`] past the person's grace from
/// then.
fn command_step(
    door: &Door<'_>,
    h: Harness,
    tab: &mut Tab,
    now: i64,
    rest: Option<i64>,
) -> Result<(), Fail> {
    let agent = h.name();
    let (command, pasted) = command_draft(h);
    // The composer's row as the reader takes it, or its text told from a placeholder by its cell
    // (`stand_on`): agy's and Muse Code's empty composers draw a grey hint.
    let standing = |text: &str| {
        at_composer(door, &tab.sid, agent).as_deref() == Some(text)
            || super::stand_on(door, &tab.sid, agent, &on(door, &tab.sid, &["text"]))
                == super::Stand::Composer(text.to_string())
    };
    match tab.seen.cover.draft.clone() {
        None if rest.is_some_and(|t| now >= t + DRAFT_AFTER_MS) => {
            let typed = now_ms();
            let who = format!("@{}", tab.sid);
            let r = if pasted {
                door.ctl(&[&who, "paste", command])?
            } else {
                door.ctl(&[&who, "send", "as=person", command])?
            };
            // A hang detector, never a budget (`draft_step`'s).
            let stood = r.code == 0 && crate::wait_until(super::DRAFT_HANG, || standing(command));
            println!(
                "  [{}] +{} command \"{command}\" {} ({}): {}",
                tab.chain.name(),
                at(tab.t0, typed),
                if pasted { "pasted" } else { "sent" },
                one_line(r.stdout.trim(), 60),
                if stood {
                    "it stands in the composer"
                } else {
                    "it did NOT stand in the composer"
                }
            );
            tab.seen.cover.draft = Some(Draft {
                text: command.to_string(),
                typed_ms: typed,
                stood,
                ..Draft::default()
            });
        }
        Some(d)
            if d.stood
                && d.cleared_ms.is_none()
                && now >= d.typed_ms + person_grace_ms() + COMMAND_PAST_MS =>
        {
            let who = format!("@{}", tab.sid);
            let before = at_composer(door, &tab.sid, agent);
            for _ in 0..command.chars().count() {
                door.ctl(&[&who, "key", "as=person", "backspace"])?;
            }
            let cleared = now_ms();
            let emptied = crate::wait_until(super::DRAFT_HANG, || standing(""));
            println!(
                "  [{}] +{} command cleared by {} backspace(s) (it read {:?} before): {}",
                tab.chain.name(),
                at(tab.t0, cleared),
                command.chars().count(),
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

/// `switch-seq`'s steps: the draft typed [`DRAFT_AFTER_MS`] into its back-off at rest, the switch
/// [`DRAFT_BEFORE_MS`] after it was typed and no sooner than it stood, and the draft cleared by
/// backspaces [`DRAFT_PAST_MS`] past the switch, each key `as=person`: the loop holds its hands off
/// for a person's key ([`PERSON_HOLD`]), and a draft no key of theirs stamped holds it only while
/// it stands.
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
            let r = door.ctl(&[&format!("@{}", tab.sid), "send", "as=person", DRAFT])?;
            // A hang detector, never a budget: Claude Code 2.1.316 under load drew a draft typed
            // through the socket 15 s late (`kif3-cc`, 2026-11-01), as it reads a fresh tab's.
            let stood = r.code == 0 && crate::wait_until(super::DRAFT_HANG, || standing(DRAFT));
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
                door.ctl(&[&who, "key", "as=person", "backspace"])?;
            }
            let cleared = now_ms();
            let emptied = crate::wait_until(super::DRAFT_HANG, || standing(""));
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
    for tape in tapes_in(dir)? {
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
