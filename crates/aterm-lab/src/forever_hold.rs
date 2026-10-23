// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHY LAND STILL HOLDS.

use aterm_agent::supervise::policy::turn_end;

use super::super::{at, word};
use super::{Ctx, Seen, Verdict, observed, page_field, told};
use crate::util::one_line;

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
    /// Words the state line must carry after `not landed yet: `.
    pub line: &'static [&'static str],
    /// Words the hover must carry after `not landed yet, `.
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

/// L2: the open words its reading found.
pub const L2: Legs = Legs {
    legs: "open",
    line: &["in its report"],
    hover: &["in its report"],
    not: &[
        "needs you",
        "changed file",
        "untracked file",
        "stash",
        "unpushed commit",
        "not on the remote main",
    ],
};
/// L3: its need.
pub const L3: Legs = Legs {
    legs: "needs",
    line: &["needs you"],
    hover: &["needs you"],
    not: &[
        "in its report",
        "changed file",
        "untracked file",
        "stash",
        "unpushed commit",
        "not on the remote main",
        "too tall",
    ],
};
/// L4: one untracked file.
pub const L4: Legs = Legs {
    legs: "repo",
    line: &["1 untracked file"],
    hover: &["1 untracked file"],
    not: &others(1),
};
/// L6: its need.
pub const L6: Legs = Legs {
    legs: "needs",
    line: &["needs you"],
    hover: &["needs you"],
    not: &[
        "in its report",
        "changed file",
        "untracked file",
        "stash",
        "unpushed commit",
        "not on the remote main",
        "too tall",
    ],
};
/// L8: one changed file.
pub const L8: Legs = Legs {
    legs: "repo",
    line: &["1 changed file"],
    hover: &["1 changed file"],
    not: &others(0),
};
/// L9: one stash.
pub const L9: Legs = Legs {
    legs: "repo",
    line: &["1 stash"],
    hover: &["1 stash"],
    not: &others(2),
};
/// L10: one unpushed commit, so one not on the remote main.
pub const L10: Legs = Legs {
    legs: "repo",
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
    line: &["1 commit not on the remote main"],
    hover: &["1 commit not on the remote main"],
    not: &others(4),
};

/// The words the open leg in `line` names (`open work in its report:
/// follow-up, still open; …`: `follow-up`, `still open`), up to the next
/// leg, the line's next part or the hover's end; split as the nudge's suffix
/// is ([`nudge_named`]), so a word with a comma in it splits alike on both.
fn named_words(line: &str) -> Vec<&str> {
    let Some((_, rest)) = line.split_once("open work in its report: ") else {
        return Vec::new();
    };
    let end = [";", " \u{b7} ", ")"]
        .iter()
        .filter_map(|stop| rest.find(stop))
        .min()
        .unwrap_or(rest.len());
    rest[..end].split(", ").collect()
}

/// L2: the words the nudge after `t1` named open (`Your report still names
/// open work (<words>): …`); `None` before a nudge.
fn nudge_named(seen: &Seen, t1: i64) -> Option<Vec<String>> {
    let nudge = seen
        .acts(turn_end::RULE_LAND_NUDGE)
        .into_iter()
        .find(|(t, _)| *t > t1)?;
    let text = seen.act_text(turn_end::RULE_LAND_NUDGE, nudge);
    let named = text
        .split_once("still names open work (")?
        .1
        .split_once(')')?
        .0;
    Some(
        named
            .split(", ")
            .filter(|w| *w != "\u{2026}")
            .map(str::to_string)
            .collect(),
    )
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
    let says = |line: &str, words: &[&str]| {
        words.iter().all(|x| line.contains(x)) && !want.not.iter().any(|x| line.contains(x))
    };
    let hover_reads = observed(&w.hover, &w.hover_reads, t1, until);
    let hovers: Vec<_> = hover_reads
        .iter()
        .copied()
        .filter(|(_, h)| h.starts_with("driven by aterm (land: not landed yet, "))
        .collect();
    let hover = hovers.iter().find(|(_, h)| says(h, want.hover));
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
            unread.push("the hover never said `not landed yet`".into());
        } else {
            bad.push(format!(
                "the hover never said {:?} without {:?}: {:?}",
                want.hover,
                want.not,
                hovers.iter().map(|(_, h)| *h).collect::<Vec<_>>()
            ));
        }
    }
    let lines: Vec<(i64, &str)> = observed(&w.page, &w.page_reads, t1, until)
        .into_iter()
        .filter_map(|(t, p)| Some((t, page_field(p, "state")?)))
        .filter(|(_, l)| l.starts_with("not landed yet: "))
        .collect();
    let line = lines.iter().find(|(_, l)| says(l, want.line));
    let page_from = held.first().map_or(until, |(t, _)| t.saturating_sub(1));
    let page_reads = observed(&w.page, &w.page_reads, page_from, until);
    let on_page = page_reads.iter().any(|(_, p)| *p != "-");
    if line.is_none() && !lines.is_empty() {
        bad.push(format!(
            "the state line never said {:?} without {:?}: {:?}",
            want.line,
            want.not,
            lines.iter().map(|(_, l)| *l).collect::<Vec<_>>()
        ));
    } else if line.is_none() && on_page {
        bad.push("its row on the page never said `not landed yet: `".into());
    } else if page_reads.is_empty() {
        unread.push("no Harness page read after Land's instruction".into());
    }
    if want.legs == "open" {
        match nudge_named(seen, t1) {
            Some(named) => {
                for said in hover
                    .map(|(_, h)| *h)
                    .into_iter()
                    .chain(line.map(|(_, l)| *l))
                {
                    let words = named_words(said);
                    let quoted =
                        said.contains('\u{201c}') && !said.contains("\u{201c}Nothing left:");
                    if words.is_empty()
                        || words.iter().any(|w| !named.iter().any(|n| n == w))
                        || quoted
                    {
                        bad.push(format!(
                            "names {words:?}, but the nudge named {named:?}: {}",
                            one_line(said, 200)
                        ));
                    }
                }
            }
            None => unread.push("no land-nudge@v1 named the open words".into()),
        }
    }
    let said = format!(
        "status {:?}; hover {}; state line {}",
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
        )
    );
    told(said, &bad, &unread)
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
    if let Some((t, h)) = hovers.iter().find(|(_, h)| h.contains("not landed yet")) {
        bad.push(format!("+{} the hover: {h}", at(cx.t0, *t)));
    }
    let pages = observed(&w.page, &w.page_reads, after, seen.end_ms + 1);
    if let Some((t, p)) = pages.iter().find(|(_, p)| {
        page_field(p, "state")
            .is_some_and(|s| s.contains("landing since") || s.contains("not landed yet"))
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
