// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHY LAND STILL HOLDS.

use aterm_agent::supervise::policy::{land_check, turn_end};

use super::super::{at, word};
use super::held::unstopped;
use super::{
    Ctx, Seen, Verdict, observed, page_field, quoted as field_quoted, split_title, standing, told,
    worker_field,
};
use crate::util::one_line;
use aterm_agent::harness::acts::{NOT_LANDED_YET, StopForm};
use aterm_agent::harness::hold_said::{OPEN_IN_REPORT, WAITS_ON_YOU, quoted};

/// How far before the land act's journal line a read may already show its
/// hold (the hold is told as the act is typed, the line written after).
const TOLD_SLACK_MS: i64 = 2_000;

/// How long after a nudge's journal line a read may still count the
/// nudges before it (a read between the tell and the line).
const NUDGE_SLACK_MS: i64 = 5_000;

/// How long after `status` first reads a hold the strip may still show the
/// hover from before it: the hold is on the timeline as the loop's call
/// returns, the strip stamped again when the window's wake for it is
/// applied, a look or two later at most.
const HOVER_SLACK_MS: i64 = 10_000;

/// One `status` read's hold, `[land_held, land_nudges, land_since]`;
/// `None` where it prints no such field (`?`).
fn fields(s: &str) -> Option<[String; 3]> {
    let f = |k: &str| word(s, k).filter(|v| v != "?");
    Some([f("land_held")?, f("land_nudges")?, f("land_since")?])
}

/// The first land act (`land@v1`): its journal time and its ledger row's
/// second.
fn began(seen: &Seen) -> Option<(i64, Option<i64>)> {
    let t = seen.acts(turn_end::RULE_LAND).first()?.0;
    let row = seen
        .typed_row(turn_end::RULE_LAND, t)
        .map(|r| r.ts.div_euclid(1000));
    Some((t, row))
}

/// When the landing after `t` was journaled (`DONE `); past the watch where
/// none was.
fn landed_after(seen: &Seen, t: i64) -> i64 {
    seen.first_after("DONE ", t)
        .map_or(seen.end_ms + 1, |(d, _)| *d)
}

/// The nudges (`land-nudge@v1`) journaled after `from` and before `until`.
fn nudges_between(seen: &Seen, from: i64, until: i64) -> usize {
    seen.acts(turn_end::RULE_LAND_NUDGE)
        .iter()
        .filter(|(t, _)| *t > from && *t < until)
        .count()
}

/// Whether a read at `t` printing `n` nudges counts those typed since
/// `from`: the nudges journaled before it, or before it less the slack.
fn counts_nudges(seen: &Seen, from: i64, t: i64, n: &str) -> bool {
    let n = n.parse::<usize>().ok();
    n == Some(nudges_between(seen, from, t + NUDGE_SLACK_MS))
        || n == Some(nudges_between(seen, from, t - NUDGE_SLACK_MS))
}

/// The actual `status` reads in `(from, until)`, each with its hold
/// fields; the first read with none, where one is.
fn reads(seen: &Seen, from: i64, until: i64) -> Result<Vec<(i64, [String; 3])>, String> {
    reads_of(observed(&seen.status, &seen.status_reads, from, until))
}

/// The `status` reads of `list`, each with its hold fields; the first read
/// with none, where one is.
fn reads_of(list: Vec<(i64, &str)>) -> Result<Vec<(i64, [String; 3])>, String> {
    let mut out = Vec::new();
    for (t, s) in list {
        match fields(s) {
            Some(f) => out.push((t, f)),
            None => {
                return Err(format!(
                    "status prints no land_held= land_nudges= land_since= (a build before 3c): {}",
                    one_line(s, 160)
                ));
            }
        }
    }
    Ok(out)
}

/// L1 (and L16's and L17's start).
pub fn begins(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((t1, row_s)) = began(seen) else {
        return Verdict::Unobserved("no land act (its records)".into());
    };
    let until = landed_after(seen, t1);
    let (before, during) = match (
        reads(seen, cx.t0, t1 - TOLD_SLACK_MS),
        reads(seen, t1 - TOLD_SLACK_MS, until),
    ) {
        (Ok(b), Ok(d)) => (b, d),
        (Err(e), _) | (_, Err(e)) => return Verdict::Fail(e),
    };
    let mut bad = Vec::new();
    if let Some((t, f)) = before.iter().find(|(_, f)| f.iter().any(|v| v != "-")) {
        bad.push(format!(
            "a hold before Land's instruction: +{} land_held={} land_nudges={} land_since={}",
            at(cx.t0, *t),
            f[0],
            f[1],
            f[2]
        ));
    }
    let since_ok = |s: &str| {
        s.parse::<i64>()
            .is_ok_and(|s| row_s.is_none_or(|r| (s - r).abs() <= 1))
    };
    let held: Vec<&(i64, [String; 3])> = during.iter().filter(|(_, f)| f[2] != "-").collect();
    if let Some((t, f)) = held.iter().find(|(_, f)| !since_ok(&f[2])) {
        bad.push(format!(
            "+{} land_since={}, not land@v1's second ({})",
            at(cx.t0, *t),
            f[2],
            row_s.map_or_else(|| "-".to_string(), |r| r.to_string())
        ));
    }
    if let Some((t, f)) = held
        .iter()
        .find(|(t, f)| !counts_nudges(seen, t1, *t, &f[1]))
    {
        bad.push(format!(
            "+{} land_nudges={}, but {} land-nudge@v1 journaled since land@v1",
            at(cx.t0, *t),
            f[1],
            nudges_between(seen, t1, *t)
        ));
    }
    let said = format!(
        "land@v1 at +{} (ledgered {}); held: {:?}",
        at(cx.t0, t1),
        row_s.map_or_else(|| "-".to_string(), |r| r.to_string()),
        held.iter()
            .map(|(t, f)| format!("+{} {}/{}/{}", at(cx.t0, *t), f[0], f[1], f[2]))
            .collect::<Vec<_>>()
    );
    let unread = if held.is_empty() && bad.is_empty() {
        vec!["no read showed the hold after land@v1".to_string()]
    } else {
        Vec::new()
    };
    told(said, &bad, &unread)
}

/// What a land act's yield that did not land must say (§12.3.7).
#[derive(Clone, Copy, Debug)]
pub struct Legs {
    /// `land_held=`'s legs, in `status`'s order.
    pub legs: &'static str,
    /// What the state line and the hover lead with: `not landed yet`, or `waits only on you`
    /// for a hold its need alone holds (said once, §14.4, slice 4b).
    pub head: &'static str,
    /// Words the state line must carry after its head.
    pub line: &'static [&'static str],
    /// Words the hover must carry after its head.
    pub hover: &'static [&'static str],
    /// Words neither may carry: another leg's.
    pub not: &'static [&'static str],
}

/// Every count a repository leg may say, by its words.
const COUNTS: [&str; 5] = [
    "changed file",
    "untracked file",
    "stash",
    "unpushed commit",
    "not on the remote main",
];

/// The counts of `COUNTS` other than `own`'s, with the other legs' words.
const fn others(own: usize) -> [&'static str; 6] {
    let mut out = ["in its report"; 6];
    let mut i = 0;
    let mut j = 0;
    while i < COUNTS.len() {
        if i != own {
            out[j] = COUNTS[i];
            j += 1;
        }
        i += 1;
    }
    out[4] = "needs you";
    out[5] = "in its report";
    out
}

/// L2: the agent's own list, quoted (slice 4b: the quote is checked against the list Land's
/// nudge quoted, [`listed_quote`]).
pub const L2: Legs = Legs {
    legs: "open",
    head: NOT_LANDED_YET,
    line: &[OPEN_IN_REPORT],
    hover: &[OPEN_IN_REPORT],
    not: &[
        "needs you",
        "changed file",
        "untracked file",
        "stash",
        "unpushed commit",
        "not on the remote main",
    ],
};
/// L3: its need alone: it waits only on the person, its need named once (slice 4b).
pub const L3: Legs = Legs {
    legs: "needs",
    head: WAITS_ON_YOU,
    line: &[super::super::NEED],
    hover: &[],
    not: &[
        NOT_LANDED_YET,
        "needs you",
        "in its report",
        "changed file",
        "untracked file",
        "stash",
        "unpushed commit",
        "not on the remote main",
        "too tall",
    ],
};
/// L4: one untracked file, named (slice 4b).
pub const L4: Legs = Legs {
    legs: "repo",
    head: NOT_LANDED_YET,
    line: &["1 untracked file: wip-notes.txt"],
    hover: &["1 untracked file: wip-notes.txt"],
    not: &others(1),
};
/// L6: its need alone, as L3's.
pub const L6: Legs = Legs {
    legs: "needs",
    head: WAITS_ON_YOU,
    line: &[super::super::NEED],
    hover: &[],
    not: &[
        NOT_LANDED_YET,
        "needs you",
        "in its report",
        "changed file",
        "untracked file",
        "stash",
        "unpushed commit",
        "not on the remote main",
        "too tall",
    ],
};
/// L8: one changed file, named (slice 4b).
pub const L8: Legs = Legs {
    legs: "repo",
    head: NOT_LANDED_YET,
    line: &["1 changed file: README.md"],
    hover: &["1 changed file: README.md"],
    not: &others(0),
};
/// L9: one stash.
pub const L9: Legs = Legs {
    legs: "repo",
    head: NOT_LANDED_YET,
    line: &["1 stash"],
    hover: &["1 stash"],
    not: &others(2),
};
/// L10: one unpushed commit, so one not on the remote main.
pub const L10: Legs = Legs {
    legs: "repo",
    head: NOT_LANDED_YET,
    line: &["1 unpushed commit", "1 commit not on the remote main"],
    hover: &["1 unpushed commit", "1 commit not on the remote main"],
    not: &[
        "changed file",
        "untracked file",
        "stash",
        "needs you",
        "in its report",
    ],
};
/// L11: one commit pushed elsewhere, not on the remote main.
pub const L11: Legs = Legs {
    legs: "repo",
    head: NOT_LANDED_YET,
    line: &["1 commit not on the remote main"],
    hover: &["1 commit not on the remote main"],
    not: &others(4),
};

/// The defense leg's words on the hover, the state line and the tip, read from the app's own
/// ([`HoldLeg::fixed_words`](aterm_agent::harness::acts::HoldLeg::fixed_words)).
pub const DEFENSE_WORDS: &str = match aterm_agent::harness::acts::HoldLeg::Defense.fixed_words() {
    Some(w) => w,
    None => "",
};

/// P5 and L12 (cover's `land-t` and `land-q`): a work reply, or a question, under Land, and no
/// `Nothing left:` defense in it.
pub const DEFENSE: Legs = Legs {
    legs: "defense",
    head: NOT_LANDED_YET,
    line: &[DEFENSE_WORDS],
    hover: &[DEFENSE_WORDS],
    not: &[
        "needs you",
        "in its report",
        "changed file",
        "untracked file",
        "stash",
        "unpushed commit",
        "not on the remote main",
    ],
};

/// L2: the agent's list as the first Land nudge after `t1` that quotes it quoted it
/// (`You said this is left: “<list>”.`, `land_check::LISTED_OPEN`, or the contradiction's
/// `you said this is left: “<list>”`, [`listed_in_nudge`]), each item as the hold's
/// quote spells it: the list's items ([`land_check::listed_items`]) in the curly quotes the glass
/// quotes in; `None` before such a nudge.
fn listed_quote(seen: &Seen, t1: i64) -> Option<Vec<String>> {
    seen.acts(turn_end::RULE_LAND_NUDGE)
        .into_iter()
        .filter(|(t, _)| *t > t1)
        .find_map(|nudge| {
            let text = seen.act_text(turn_end::RULE_LAND_NUDGE, nudge);
            let list = listed_in_nudge(text)?;
            Some(list.split("; ").map(quoted).collect())
        })
}

/// The agent's list as a land nudge quotes it: after Land's nudge's opening
/// (`land_check::LISTED_OPEN`) or its contradiction's (`turn_end::CONTRADICTED_LISTED`, slice 4c),
/// up to the closing quote mark, which the contradiction follows with its own words, not `.`.
pub(crate) fn listed_in_nudge(text: &str) -> Option<&str> {
    let after = [land_check::LISTED_OPEN, turn_end::CONTRADICTED_LISTED]
        .into_iter()
        .filter_map(|open| text.find(open).map(|i| &text[i + open.len()..]))
        .next()?;
    let close = land_check::LISTED_CLOSE.trim_end_matches('.');
    Some(after.split_once(close)?.0)
}

/// L2, L3, L4, L6, L8–L11.
pub fn legs(seen: &Seen, cx: &Ctx<'_>, want: Legs) -> Verdict {
    let Some((t1, _)) = began(seen) else {
        return Verdict::Unobserved("no land act (its records)".into());
    };
    let until = landed_after(seen, t1);
    let during = match reads(seen, t1, until) {
        Ok(d) => d,
        Err(e) => return Verdict::Fail(e),
    };
    let mut bad = Vec::new();
    let mut unread = Vec::new();
    let held: Vec<&(i64, [String; 3])> = during.iter().filter(|(_, f)| f[0] != "-").collect();
    if let Some((t, f)) = held.iter().find(|(_, f)| f[0] != want.legs) {
        bad.push(format!(
            "+{} land_held={}, not {}",
            at(cx.t0, *t),
            f[0],
            want.legs
        ));
    }
    if !held.iter().any(|(_, f)| f[0] == want.legs) && bad.is_empty() {
        unread.push(format!("no read showed land_held={}", want.legs));
    }
    if let Some((t, f)) = held
        .iter()
        .find(|(t, f)| !counts_nudges(seen, t1, *t, &f[1]))
    {
        bad.push(format!(
            "+{} land_nudges={}, but {} land-nudge@v1 journaled since land@v1",
            at(cx.t0, *t),
            f[1],
            nudges_between(seen, t1, *t)
        ));
    }
    let w = &seen.window;
    let carries = |line: &str, words: &[&str]| {
        words.iter().all(|x| line.contains(x)) && !want.not.iter().any(|x| line.contains(x))
    };
    let hover_reads = observed(&w.hover, &w.hover_reads, t1, until);
    // A stopped tab's hover and state line lead with the stop (3a), the hold's words behind it.
    let hover_head = format!("driven by aterm (land: {}", want.head);
    let hovers: Vec<(i64, std::borrow::Cow<'_, str>)> = hover_reads
        .iter()
        .map(|(t, h)| (*t, unstopped(h, StopForm::Hover)))
        .filter(|(_, h)| {
            h.starts_with(&format!("driven by aterm (land: {NOT_LANDED_YET}"))
                || h.starts_with(&format!("driven by aterm (land: {WAITS_ON_YOU}"))
        })
        .collect();
    let says =
        |line: &str, words: &[&str], head: &str| line.starts_with(head) && carries(line, words);
    let hover = hovers
        .iter()
        .find(|(_, h)| says(h, want.hover, &hover_head));
    if hover.is_none() {
        let late = held
            .first()
            .map(|(t, _)| t + HOVER_SLACK_MS)
            .filter(|late| *late < until)
            .filter(|late| {
                observed(&seen.status, &seen.status_reads, *late, until)
                    .first()
                    .and_then(|(_, s)| fields(s))
                    .is_some_and(|f| f[0] != "-")
            });
        let stale = late.and_then(|late| {
            hover_reads
                .iter()
                .find(|(t, h)| *t >= late && *h == "driven by aterm (land)")
        });
        if hovers.is_empty()
            && let (Some(late), Some(_)) = (late, stale)
        {
            bad.push(format!(
                "+{} the hover still said `driven by aterm (land)` {} s after `status` read the \
                 hold: a stale push",
                at(cx.t0, late),
                HOVER_SLACK_MS / 1000
            ));
        } else if hovers.is_empty() {
            unread.push("the hover never said the hold".into());
        } else {
            bad.push(format!(
                "the hover never said {:?} {:?} without {:?}: {:?}",
                want.head,
                want.hover,
                want.not,
                hovers.iter().map(|(_, h)| h.as_ref()).collect::<Vec<_>>()
            ));
        }
    }
    let lines: Vec<(i64, std::borrow::Cow<'_, str>)> = observed(&w.page, &w.page_reads, t1, until)
        .into_iter()
        .filter_map(|(t, p)| Some((t, unstopped(page_field(p, "state")?, StopForm::Line))))
        .filter(|(_, l)| {
            l.starts_with(&format!("{NOT_LANDED_YET}: ")) || l.starts_with(WAITS_ON_YOU)
        })
        .collect();
    let line = lines.iter().find(|(_, l)| says(l, want.line, want.head));
    let page_from = held.first().map_or(until, |(t, _)| t.saturating_sub(1));
    let page_reads = observed(&w.page, &w.page_reads, page_from, until);
    let on_page = page_reads.iter().any(|(_, p)| *p != "-");
    let mut described: Vec<(i64, String)> = Vec::new();
    if line.is_none() && !lines.is_empty() {
        bad.push(format!(
            "the state line never said {:?} {:?} without {:?}: {:?}",
            want.head,
            want.line,
            want.not,
            lines.iter().map(|(_, l)| l.as_ref()).collect::<Vec<_>>()
        ));
    } else if line.is_none() && on_page {
        bad.push(format!("its row on the page never said `{}`", want.head));
    } else if page_reads.is_empty() {
        unread.push("no Harness page read after Land's instruction".into());
    }
    if want.legs == "open" {
        match listed_quote(seen, t1) {
            Some(items) => {
                let shown = format!("{OPEN_IN_REPORT}: {}", items.join("; "));
                // The hover quotes the first item cut to the room it has, so it is held to the
                // first item's opening words; the state line to the list whole.
                let first = items.first().map_or("", String::as_str);
                let opening = first.chars().take(24).collect::<String>();
                if let Some((_, l)) = line
                    && !l.contains(&shown)
                {
                    bad.push(format!(
                        "the state line does not quote the list its nudge quoted ({shown:?}): {}",
                        one_line(l, 200)
                    ));
                }
                if let Some((_, h)) = hover
                    && !h.contains(&format!("{OPEN_IN_REPORT}: {opening}"))
                {
                    bad.push(format!(
                        "the hover does not quote the list its nudge quoted ({first:?}): {}",
                        one_line(h, 200)
                    ));
                }
                // The description is spoken: the first item whole, with no quote marks a screen
                // reader would read out.
                let heard = format!(
                    "{}: {}",
                    spoken_open(),
                    opening.trim_start_matches('\u{201c}')
                );
                described = described_open(seen, (page_from, until));
                if let Some((t, d)) = described.iter().find(|(_, d)| !d.contains(&heard)) {
                    bad.push(format!(
                        "+{} the chip's description does not say the list its nudge quoted \
                         ({heard:?}): {}",
                        at(cx.t0, *t),
                        one_line(d, 200)
                    ));
                }
            }
            None => unread.push("no land-nudge@v1 quoted the agent's list".into()),
        }
    }
    let said = format!(
        "status {:?}; hover {}; state line {}{}",
        held.iter()
            .map(|(t, f)| format!("+{} {}/{}/{}", at(cx.t0, *t), f[0], f[1], f[2]))
            .collect::<Vec<_>>(),
        hover.map_or_else(
            || "-".to_string(),
            |(t, h)| format!("+{} {}", at(cx.t0, *t), one_line(h, 200))
        ),
        line.map_or_else(
            || if on_page {
                "-".to_string()
            } else {
                "(its row off the page's screen)".to_string()
            },
            |(t, l)| format!("+{} {}", at(cx.t0, *t), one_line(l, 200))
        ),
        match described.first() {
            Some((t, d)) => format!(
                "; the chip's description +{} {}",
                at(cx.t0, *t),
                one_line(d, 200)
            ),
            None if want.legs == "open" =>
                "; the chip's description: not read in front".to_string(),
            None => String::new(),
        }
    );
    told(said, &bad, &unread)
}

/// The chip's accessibility description at each `chrome` read from `from` to `until` (the hold
/// standing) that drew the tab's chip under Land's `⤓` (`face=land`, a description printed), as
/// `chrome` prints it beside the node the screen reader is handed (§14.3's windowed read): a tab
/// behind says none. Every one is held to the list, whatever its words.
fn described_open(seen: &Seen, (from, until): (i64, i64)) -> Vec<(i64, String)> {
    let w = &seen.window;
    let mut out: Vec<(i64, String)> = w
        .chip
        .iter()
        .chain(&w.held)
        .filter(|(t, _)| *t >= from && *t < until)
        .filter_map(|(t, l)| {
            let (rest, _) = split_title(l);
            (word(&rest, "face").as_deref() == Some("land"))
                .then(|| field_quoted(&rest, "description"))
                .flatten()
                .filter(|d| !d.is_empty() && d != "-")
                .map(|d| (*t, d))
        })
        .collect();
    out.sort_by_key(|(t, _)| *t);
    out
}

/// [`OPEN_IN_REPORT`] as the chip's description says it, a spoken sentence's head (`Open in its
/// report`).
fn spoken_open() -> String {
    let mut cs = OPEN_IN_REPORT.chars();
    cs.next()
        .map(|first| first.to_uppercase().collect::<String>() + cs.as_str())
        .unwrap_or_default()
}

/// LR (land-reworded): A REWORDED ITEM REFRESHES THE QUOTE (slice 4b). Every read of the state
/// line or the hover that says the hold (`not landed yet`) while `status` holds the Open leg
/// (`land_held` names `open` at the reads either side of it) quotes the check's first list
/// ([`super::super::CHECK_LIST`]) until the second check is typed, and the reworded one
/// ([`super::super::REWORDED_LIST`]) after it: a hold whose word family and legs stayed the same,
/// so only its own words moved. A read of the Open leg in any other words (the reader's family
/// words a build without 4b says) fails; a surface never read while the Open leg stood goes
/// unobserved.
pub fn reworded(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((t1, _)) = began(seen) else {
        return Verdict::Unobserved("no land act (its records)".into());
    };
    let until = landed_after(seen, t1);
    let status = match reads(seen, t1, until) {
        Ok(r) => r,
        Err(e) => return Verdict::Fail(e),
    };
    let open = |f: &[String; 3]| {
        f[0].split(',')
            .any(|l| l == aterm_agent::harness::acts::HoldLeg::Open.word())
    };
    let open_at = |t: i64| {
        status
            .iter()
            .rev()
            .find(|(ts, _)| *ts <= t)
            .is_some_and(|(_, f)| open(f))
            && status
                .iter()
                .find(|(ts, _)| *ts >= t)
                .is_none_or(|(_, f)| open(f))
    };
    let second = seen
        .acts(turn_end::RULE_LAND_CHECK)
        .iter()
        .filter(|(t, _)| *t > t1)
        .nth(1)
        .map(|(t, _)| *t);
    let w = &seen.window;
    let (old, new) = (
        format!("{OPEN_IN_REPORT}: {}", quoted(super::super::CHECK_LIST)),
        format!("{OPEN_IN_REPORT}: {}", quoted(super::super::REWORDED_LIST)),
    );
    let lines: Vec<(i64, String)> = observed(&w.page, &w.page_reads, t1, until)
        .into_iter()
        .filter_map(|(t, p)| {
            Some((
                t,
                unstopped(page_field(p, "state")?, StopForm::Line).into_owned(),
            ))
        })
        .collect();
    let hovers: Vec<(i64, String)> = observed(&w.hover, &w.hover_reads, t1, until)
        .into_iter()
        .map(|(t, h)| (t, unstopped(h, StopForm::Hover).into_owned()))
        .collect();
    let mut bad = Vec::new();
    let mut unread = Vec::new();
    let mut said = Vec::new();
    for (surface, reads) in [("state line", &lines), ("hover", &hovers)] {
        let shown: Vec<&(i64, String)> = reads
            .iter()
            .filter(|(t, l)| l.contains(NOT_LANDED_YET) && open_at(*t))
            .collect();
        let (early, late): (Vec<_>, Vec<_>) = shown
            .into_iter()
            .partition(|(t, _)| second.is_none_or(|c| *t <= c));
        if let Some((t, l)) = early.iter().find(|(_, l)| !l.contains(&old)) {
            bad.push(format!(
                "+{} the {surface} said the Open leg without quoting the list: {}",
                at(cx.t0, *t),
                one_line(l, 200)
            ));
        }
        if let Some((t, l)) = late.iter().find(|(_, l)| !l.contains(&new)) {
            bad.push(format!(
                "+{} the {surface} said the Open leg after the second check without its new \
                 words: {}",
                at(cx.t0, *t),
                one_line(l, 200)
            ));
        }
        match (early.first(), late.first()) {
            (Some((tf, _)), Some((tn, _))) => said.push(format!(
                "the {surface} quoted the list at +{}, the new words at +{}",
                at(cx.t0, *tf),
                at(cx.t0, *tn)
            )),
            (None, Some((tn, _))) => {
                said.push(format!(
                    "the {surface} quoted the new words at +{}",
                    at(cx.t0, *tn)
                ));
                unread.push(format!("the {surface} never read the first list's hold"));
            }
            (Some((tf, _)), None) => unread.push(format!(
                "the {surface} quoted the list at +{}, and {}",
                at(cx.t0, *tf),
                if second.is_some() {
                    "never said the Open leg after the second check"
                } else {
                    "no second check was typed"
                }
            )),
            (None, None) => unread.push(format!(
                "the {surface} never said the Open leg while `status` held it"
            )),
        }
    }
    told(said.join("; "), &bad, &unread)
}

/// L5 (and L16's and L17's end).
pub fn gone(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((d, _)) = seen.first_after("DONE ", cx.t0) else {
        return Verdict::Unobserved("no landing (its records)".into());
    };
    let after = *d + TOLD_SLACK_MS;
    let read = match reads(seen, after, seen.end_ms + 1) {
        Ok(r) => r,
        Err(e) => return Verdict::Fail(e),
    };
    let w = &seen.window;
    let mut bad = Vec::new();
    if let Some((t, f)) = read.iter().find(|(_, f)| f.iter().any(|v| v != "-")) {
        bad.push(format!(
            "+{} land_held={} land_nudges={} land_since={} after the landing",
            at(cx.t0, *t),
            f[0],
            f[1],
            f[2]
        ));
    }
    let hovers = observed(&w.hover, &w.hover_reads, after, seen.end_ms + 1);
    if let Some((t, h)) = hovers
        .iter()
        .find(|(_, h)| h.contains(NOT_LANDED_YET) || h.contains(WAITS_ON_YOU))
    {
        bad.push(format!("+{} the hover: {h}", at(cx.t0, *t)));
    }
    let pages = observed(&w.page, &w.page_reads, after, seen.end_ms + 1);
    if let Some((t, p)) = pages.iter().find(|(_, p)| {
        page_field(p, "state").is_some_and(|s| {
            s.contains("landing since") || s.contains(NOT_LANDED_YET) || s.contains(WAITS_ON_YOU)
        })
    }) {
        bad.push(format!(
            "+{} the state line: {}",
            at(cx.t0, *t),
            page_field(p, "state").unwrap_or("-")
        ));
    }
    let mut unread = Vec::new();
    for (empty, field) in [
        (read.is_empty(), "status"),
        (hovers.is_empty(), "hover"),
        (pages.is_empty(), "Harness page"),
    ] {
        if empty {
            unread.push(format!("no {field} read after the landing's slack"));
        }
    }
    told(
        format!(
            "landed at +{}; {} reads after it",
            at(cx.t0, *d),
            read.len()
        ),
        &bad,
        &unread,
    )
}

/// The hold's tip form (`hold_words`' `Tip`, `SessionRow::landing_tip`) for a read defense hold
/// after `nudges` nudges, as the app composes it
/// ([`held_tip`](aterm_agent::harness::acts::held_tip)).
#[must_use]
pub fn defense_tip(nudges: usize) -> String {
    aterm_agent::harness::acts::held_tip(DEFENSE_WORDS, u32::try_from(nudges).unwrap_or(u32::MAX))
}

/// L15 (cover's `worker`, spawned by `mgr` under Land, whose first land reply holds): its row
/// one level into its manager's submenu says `● <label> — working` before Land's instruction
/// (§12.4.1: a worker on its first task works) and `● <label> — landing` once the instruction
/// was typed and its hold stands, its manager's row counting it `1 landing`; once `status`
/// reads the hold, its tip is the hold's tip form ([`defense_tip`]), or its nudges alone once
/// a defense got the land check; never `landing` once landed.
pub fn worker_row(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some((t1, _)) = began(seen) else {
        return Verdict::Unobserved("no land act (its records)".into());
    };
    let until = landed_after(seen, t1);
    let w = &seen.window;
    let label = &w.label;
    let row_of = |e: &str| worker_field(e, "row").map(str::to_string);
    let mut bad = Vec::new();
    let mut unread = Vec::new();
    let working = format!("\u{25cf} {label} \u{2014} working ");
    let landing = format!("\u{25cf} {label} \u{2014} landing ");
    let before = standing(&w.worker, cx.t0, t1 - TOLD_SLACK_MS);
    let mut worked = None;
    for (t, e) in &before {
        let Some(row) = row_of(e) else {
            continue;
        };
        if !row.starts_with(&working)
            || worker_field(e, "tip").is_some()
            || !worker_field(e, "mgr").is_some_and(|m| m.ends_with(": 1 working"))
        {
            bad.push(format!(
                "+{} before Land's instruction: {}",
                at(cx.t0, *t),
                one_line(e, 200)
            ));
        } else if worked.is_none() {
            worked = Some((*t, row));
        }
    }
    if worked.is_none() && bad.is_empty() {
        unread.push("no read of its row before Land's instruction".to_string());
    }
    let held = match reads(seen, t1, until) {
        Ok(d) => d.into_iter().find(|(_, f)| f[0] != "-").map(|(t, _)| t),
        Err(e) => return Verdict::Fail(e),
    };
    let during = if until > t1 + HOVER_SLACK_MS {
        standing(&w.worker, t1 + HOVER_SLACK_MS, until)
    } else {
        Vec::new()
    };
    let mut landed_row = None;
    let mut tipped = None;
    for (t, e) in &during {
        let row = row_of(e);
        let ok = row.as_deref().is_some_and(|r| r.starts_with(&landing))
            && worker_field(e, "mgr").is_some_and(|m| m.ends_with(": 1 landing"));
        if !ok {
            bad.push(format!(
                "+{} while it holds (land@v1 at +{}): {}",
                at(cx.t0, *t),
                at(cx.t0, t1),
                one_line(e, 200)
            ));
            continue;
        }
        landed_row.get_or_insert((*t, row.unwrap_or_default()));
        let tip = worker_field(e, "tip");
        let read = held.is_some_and(|h| *t >= h + HOVER_SLACK_MS);
        let n = nudges_between(seen, t1, *t + NUDGE_SLACK_MS);
        let wants = [
            defense_tip(n),
            defense_tip(nudges_between(seen, t1, *t - NUDGE_SLACK_MS)),
        ];
        // Its defense answered the hold: while the land check it got is out, the tip is its
        // nudges alone (slice 13a: a defense is checked before it lands).
        let checked = seen
            .acts(turn_end::RULE_LAND_CHECK)
            .iter()
            .any(|(c, _)| *c > t1 && *c < *t + NUDGE_SLACK_MS);
        let answered = || {
            [n, nudges_between(seen, t1, *t - NUDGE_SLACK_MS)].map(|n| {
                aterm_agent::harness::acts::nudge_words(u32::try_from(n).unwrap_or(u32::MAX))
            })
        };
        match tip {
            Some(tip) if wants.iter().any(|w| w == tip) => {
                tipped.get_or_insert((*t, tip.to_string()));
            }
            Some(tip) if checked && answered().iter().any(|w| w.as_deref() == Some(tip)) => {}
            None if checked => {}
            Some(tip) if read || tip.starts_with("not landed yet") => bad.push(format!(
                "+{} its tip {tip:?}, not the hold's {:?}",
                at(cx.t0, *t),
                wants[0]
            )),
            None if read => bad.push(format!(
                "+{} no tip, though status read the hold from +{}",
                at(cx.t0, *t),
                held.map_or_else(|| "-".to_string(), |h| at(cx.t0, h))
            )),
            _ => {}
        }
    }
    if landed_row.is_none() && bad.is_empty() {
        unread.push(format!(
            "no read of its row while it held (land@v1 at +{}, landed at +{})",
            at(cx.t0, t1),
            at(cx.t0, until)
        ));
    } else if tipped.is_none() && bad.is_empty() {
        unread.push("no read of its row with the hold's tip".to_string());
    }
    if let Some((t, e)) = standing(&w.worker, until + HOVER_SLACK_MS, i64::MAX)
        .into_iter()
        .find(|(_, e)| row_of(e).is_some_and(|r| r.starts_with(&landing)))
    {
        bad.push(format!(
            "+{} still landing after the landing at +{}: {}",
            at(cx.t0, *t),
            at(cx.t0, until),
            one_line(e, 200)
        ));
    }
    let said = format!(
        "working {}; landing {}; tip {}",
        worked.map_or_else(
            || "-".to_string(),
            |(t, r)| format!("+{} {r}", at(cx.t0, t))
        ),
        landed_row.map_or_else(
            || "-".to_string(),
            |(t, r)| format!("+{} {r}", at(cx.t0, t))
        ),
        tipped.map_or_else(
            || "-".to_string(),
            |(t, r)| format!("+{} {r}", at(cx.t0, t))
        )
    );
    told(said, &bad, &unread)
}

/// L16, L17: a hold from Land's instruction ([`begins`]), gone at the landing ([`gone`]).
pub fn begins_then_gone(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let (a, b) = (begins(seen, cx), gone(seen, cx));
    let why = format!(
        "from the instruction: {} ({}); at the landing: {} ({})",
        a.word(),
        a.why(),
        b.word(),
        b.why()
    );
    match (&a, &b) {
        (Verdict::Fail(_), _) | (_, Verdict::Fail(_)) => Verdict::Fail(why),
        (Verdict::Unobserved(_), _) | (_, Verdict::Unobserved(_)) => Verdict::Unobserved(why),
        _ => Verdict::Pass(why),
    }
}

#[cfg(test)]
#[path = "forever_hold_tests.rs"]
mod tests;
