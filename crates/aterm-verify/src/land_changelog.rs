// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CHANGELOG entries a landing adds stay under `## [Unreleased]`.
//!
//! A release cut writes its version heading directly under `## [Unreleased]`, above the entries
//! it ships, so a branch that merges main after the cut has each entry it wrote into the old
//! `[Unreleased]` body merged under that version: its change reads as shipped by a release cut
//! before it landed. git merges by line and cannot tell; the landing can ([`rehome`]).
//!
//! An entry is settled only when the landing adds it whole beside main's file ([`added_lines`]:
//! a change that rewrites none of main's lines), and by where its words came from
//! ([`Written`]: the entries the branch's own commits wrote, each with the release headings the
//! changelog held when they wrote or moved it under `[Unreleased]`, if they did):
//! - a copy the file holds more often than main's does goes (a branch's copy of main's unreleased
//!   entry, stranded beside main's own);
//! - one the branch wrote under `[Unreleased]` that sits under a release main cut since (a heading
//!   main holds and the changelog did not at that write, so a release that cannot have shipped
//!   it) moves to the top of its `### <kind>` under `## [Unreleased]`, which gains that
//!   subsection, in Keep a Changelog's order, where it has none. A move down writes nothing, so a
//!   cut replayed on the branch (a squash or rebase of main) strands no entry for good;
//! - one no commit of the branch wrote, whose words main does not hold, goes: main rewrote or
//!   removed them and a merge brought them back. `CHANGELOG.md` merges with git's union driver,
//!   which keeps both sides of a hunk, so a branch's entry inserted beside one main rewrote lands
//!   with the old words and the new (measured 2026-11-08, git 2.56).
//!
//! What stays where it is: an entry main holds (moved or reordered among the releases), one the
//! branch wrote only under released versions (a restored or backfilled entry), one it moved
//! under a release cut before it wrote it (until main re-dates or renames that heading), one
//! under a release heading main does not hold (a cut of the branch's own), one a rebase of the
//! branch's own commits replayed onto the cut (the replayed commit writes it under the release,
//! as a backfill does, and the commit that wrote it under `[Unreleased]` is gone), one the
//! branch rewords under the release after a merge stranded it (its new words are first written
//! there), a rewritten line of a released entry, an entry added in the same change as a rewrite
//! of main's line next to it, an entry whose next non-blank line starts at the margin but opens
//! nothing (a code fence), which leaves its end unclear, and two rewrites of one entry, main's
//! and the branch's, which no rule here can choose between.
//!
//! A `### <kind>` subsection under `## [Unreleased]` that the landing adds whole beside main's file
//! (its heading and every non-blank line of it), where the section holds another of that kind and
//! more `### <kind>` headings than main's does, joins it ([`joinable`]): the first of the kind the
//! landing does not add whole (main's), else the first. The lines it adds go to the top of that one
//! where it sat above it, after its last line where it sat below, and its heading goes; a blank
//! line main holds stays. Git's diff may give an added run any of the equal places where the lines
//! around it repeat (one directly above a subsection of its kind takes the lower of two identical
//! headings), so the run is read at the earliest ([`slid_up`]). `CHANGELOG.md` merges with git's
//! union driver, which keeps both sides of a conflicting hunk, so a branch that reordered
//! `[Unreleased]` while main wrote into it had its merge open a kind main's side already held,
//! above or below main's (measured 2026-11-10 on 7d1464cf8bb, which landed `### Changed` twice),
//! and a release ships the section as it stands. Only lines the landing adds move, so no line main
//! holds is rewritten, and a release cut that lands mid-run strands the moved lines as any other
//! added ones, for the rules above. A kind said twice by main's own lines, or by a second
//! subsection the landing adds only in part, stays, as does a section holding an HTML comment or a
//! code fence at the margin, where a `### ` line is not a heading.

use std::collections::{BTreeMap, BTreeSet};

/// The changelog, at the checkout's root.
pub(super) const PATH: &str = "CHANGELOG.md";
/// The heading of the section a change is written under until a release ships it.
const UNRELEASED: &str = "## [Unreleased]";
/// Keep a Changelog's subsection order, which a subsection [`rehome`] adds follows.
const KINDS: [&str; 6] = [
    "Added",
    "Changed",
    "Deprecated",
    "Removed",
    "Fixed",
    "Security",
];

/// The 0-based new-side lines of `diff` (one file's `git diff`) that a change adds without
/// replacing a non-blank line of the old side. A change is a run of `-` and `+` lines between
/// context lines and hunk edges, so a hunk git joined across unchanged lines
/// (`diff.interHunkContext`) reads as the changes it holds, and its context is never added.
pub(super) fn added_lines(diff: &str) -> BTreeSet<usize> {
    let mut added = BTreeSet::new();
    // The open change's added lines, and whether it removes a non-blank old line.
    let mut change: (Vec<usize>, bool) = (Vec::new(), false);
    let mut close = |change: &mut (Vec<usize>, bool)| {
        let (lines, rewrites) = std::mem::take(change);
        if !rewrites {
            added.extend(lines);
        }
    };
    // The next new-side line, inside a hunk.
    let mut next: Option<usize> = None;
    for line in diff.lines() {
        if let Some(header) = line.strip_prefix("@@ ") {
            close(&mut change);
            next = new_start(header);
            continue;
        }
        let Some(at) = next.as_mut() else {
            continue;
        };
        match line.as_bytes().first() {
            Some(b'+') => {
                change.0.push(*at);
                *at += 1;
            }
            Some(b'-') => change.1 |= !line[1..].trim().is_empty(),
            Some(b'\\') => {}
            // A context line (`diff.suppressBlankEmpty` writes a blank one empty).
            Some(b' ') | None => {
                close(&mut change);
                *at += 1;
            }
            Some(_) => {
                close(&mut change);
                next = None;
            }
        }
    }
    close(&mut change);
    added
}

/// The 0-based first new-side line a hunk header (`-a[,b] +c[,d] @@ …`, after its `@@ `) names.
fn new_start(header: &str) -> Option<usize> {
    let new = header.split_whitespace().nth(1)?.strip_prefix('+')?;
    let start = new.split_once(',').map_or(new, |(start, _)| start);
    Some(start.parse::<usize>().ok()?.saturating_sub(1))
}

/// A changelog whose added entries left its released versions.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Rehomed {
    /// The changelog, the entries moved and dropped.
    pub(super) text: String,
    /// Each entry moved under `## [Unreleased]`, by its title ([`title`]), in file order.
    pub(super) moved: Vec<String>,
    /// Each entry dropped as a surplus copy, by its title.
    pub(super) dropped: Vec<String>,
    /// Each entry dropped as words no commit of the branch wrote and main no longer holds, which
    /// a merge brought back, by its title.
    pub(super) revived: Vec<String>,
    /// Each `### <kind>` the landing added under `## [Unreleased]` beside another of its kind,
    /// with how many of its subsections joined that one ([`joinable`]), in file order.
    pub(super) joined: Vec<(String, usize)>,
}

/// One entry of a changelog: a top-level `- ` line and its indented lines.
struct Entry<'a> {
    /// The release heading it sits under, trimmed, or `None` under `## [Unreleased]`.
    release: Option<&'a str>,
    /// The `### ` subsection it sits in.
    kind: Option<&'a str>,
    /// Its lines, 0-based.
    lines: std::ops::Range<usize>,
    /// Whether the next non-blank line after it is another entry, a heading or the file's end.
    ends_clearly: bool,
}

/// `lines`' entries, and the `### ` headings under released versions.
fn scan<'a>(lines: &[&'a str]) -> (Vec<Entry<'a>>, BTreeSet<usize>) {
    let (mut entries, mut released_kinds) = (Vec::new(), BTreeSet::new());
    let mut release = None;
    let mut kind = None;
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        if line.starts_with("## ") {
            release = (!is_unreleased(line)).then(|| line.trim());
            kind = None;
        } else if let Some(heading) = line.strip_prefix("### ") {
            kind = Some(heading.trim());
            if release.is_some() {
                released_kinds.insert(at);
            }
        } else if is_entry(line) {
            let end = entry_end(lines, at);
            let ends_clearly = lines[end..]
                .iter()
                .find(|next| !next.trim().is_empty())
                .is_none_or(|next| is_entry(next) || is_heading(next));
            entries.push(Entry {
                release,
                kind,
                lines: at..end,
                ends_clearly,
            });
            at = end;
            continue;
        }
        at += 1;
    }
    (entries, released_kinds)
}

/// An entry's words, as census's `changelog-repeat` compares two.
fn words(lines: &[&str]) -> String {
    lines
        .iter()
        .flat_map(|line| line.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A changelog's release headings, each trimmed: a release is its whole heading, its date
/// included, so one pulled and cut again under its number (a re-spin) is another release.
fn releases(changelog: &str) -> BTreeSet<String> {
    changelog
        .split('\n')
        .filter(|line| line.starts_with("## ") && !is_unreleased(line))
        .map(|line| line.trim().to_owned())
        .collect()
}

/// What one changelog holds of the entries a walk wants ([`placed`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Placed {
    /// Each wanted entry it holds, by its words: whether a copy sits under `## [Unreleased]`.
    entries: BTreeMap<String, bool>,
    /// Its release headings ([`releases`]).
    releases: BTreeSet<String>,
}

/// What `changelog` holds of the entries whose words are `wanted` ([`Placed`]). An entry's first
/// words are read before all of them, so a changelog of thousands of entries costs little more
/// than its scan.
pub(super) fn placed(changelog: &str, wanted: &BTreeSet<String>) -> Placed {
    let leads: BTreeSet<String> = wanted.iter().map(|said| lead(said.split(' '))).collect();
    let lines: Vec<&str> = changelog.split('\n').collect();
    let mut entries = BTreeMap::new();
    for entry in scan(&lines).0 {
        let entry_lines = &lines[entry.lines];
        if !leads.contains(&lead(entry_lines.iter().flat_map(|l| l.split_whitespace()))) {
            continue;
        }
        let said = words(entry_lines);
        if wanted.contains(&said) {
            *entries.entry(said).or_insert(false) |= entry.release.is_none();
        }
    }
    Placed {
        entries,
        releases: releases(changelog),
    }
}

/// An entry's first three words, joined as [`words`] joins them.
fn lead<'a>(words: impl Iterator<Item = &'a str>) -> String {
    words.take(3).collect::<Vec<_>>().join(" ")
}

/// Where a branch's commits wrote an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Wrote {
    /// Only under released versions.
    Released,
    /// Under `## [Unreleased]`, with the release headings the changelog held at every such write
    /// ([`releases`]): no release cut since can have shipped it.
    Unreleased(BTreeSet<String>),
}

/// The entries a branch's commits wrote, by their words ([`Wrote`]).
pub(super) type Written = BTreeMap<String, Wrote>;

/// Add to `written` what one commit wrote, from what its changelog holds (`after`) and its
/// parents' do (`before`): each entry none of its parents holds, a merge's own resolution
/// included, and each it holds under `## [Unreleased]` that none of its parents does (one it moved
/// up from a release). A move down writes nothing: a release cut moves every entry below its new
/// heading, in main's own commit, in a merge of it, or in a replay of it on the branch (a squash,
/// a rebase), and which release a stranded entry sits under is read against the releases it was
/// written beside instead. What `written` ends with does not depend on the order commits come in.
pub(super) fn wrote(written: &mut Written, after: &Placed, before: &[&Placed]) {
    for (said, &unreleased) in &after.entries {
        let mut held = before.iter().filter_map(|parent| parent.entries.get(said));
        let known = if unreleased {
            held.any(|&up| up)
        } else {
            held.next().is_some()
        };
        if known {
            continue;
        }
        let wrote = written.entry(said.clone()).or_insert(Wrote::Released);
        if unreleased {
            *wrote = Wrote::Unreleased(match wrote {
                Wrote::Unreleased(beside) => {
                    beside.intersection(&after.releases).cloned().collect()
                }
                Wrote::Released => after.releases.clone(),
            });
        }
    }
}

/// How many times `changelog` holds each entry, by its words.
fn copies(changelog: &str) -> BTreeMap<String, usize> {
    let lines: Vec<&str> = changelog.split('\n').collect();
    let mut copies = BTreeMap::new();
    for entry in scan(&lines).0 {
        *copies.entry(words(&lines[entry.lines])).or_insert(0) += 1;
    }
    copies
}

/// The words of each entry `text` adds beside `main` that [`rehome`] settles by who wrote it: one
/// wholly in `added` that ends clearly, whose words `main` does not hold. A walk of the branch's
/// commits needs to place only these ([`placed`]).
pub(super) fn in_question(text: &str, added: &BTreeSet<usize>, main: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    if text.contains('\r') || !lines.iter().any(|line| is_unreleased(line)) {
        return BTreeSet::new();
    }
    let in_main = copies(main);
    settled(&scan(&lines).0, added)
        .map(|entry| words(&lines[entry.lines.clone()]))
        .filter(|said| !in_main.contains_key(said))
        .collect()
}

/// The entries a landing settles: each wholly in `added` that ends clearly.
fn settled<'e, 'a>(
    entries: &'e [Entry<'a>],
    added: &'e BTreeSet<usize>,
) -> impl Iterator<Item = &'e Entry<'a>> {
    entries
        .iter()
        .filter(|entry| entry.ends_clearly && entry.lines.clone().all(|n| added.contains(&n)))
}

/// `text` (the landing's changelog) with the entries it adds beside `main`'s changelog (wholly in
/// `added`, 0-based lines) settled by where their words came from (`written`, the branch's own
/// commits): a surplus copy (one `text` holds more often than `main` does, another copy staying)
/// and words neither `main` nor the branch wrote are dropped, and one the branch wrote under
/// `## [Unreleased]` that sits under a release heading `main` holds and the changelog did not when
/// the branch wrote it ([`Wrote::Unreleased`]) moves to the top of its `### <kind>` there; and a
/// `### <kind>` subsection the landing adds whole under `## [Unreleased]` beside another of its
/// kind joins that one ([`joinable`]). `None` where none is, or where `text` has no
/// `## [Unreleased]` or holds a carriage return. A `### <kind>` heading under a released version
/// that the same lines added, left with no entry, goes with them.
pub(super) fn rehome(
    text: &str,
    added: &BTreeSet<usize>,
    main: &str,
    written: &Written,
) -> Option<Rehomed> {
    if text.contains('\r') {
        return None;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    if !lines.iter().any(|line| is_unreleased(line)) {
        return None;
    }
    let (entries, released_kinds) = scan(&lines);
    let in_main = copies(main);
    let main_releases = releases(main);
    let mut left = BTreeMap::new();
    for entry in &entries {
        *left
            .entry(words(&lines[entry.lines.clone()]))
            .or_insert(0_usize) += 1;
    }
    let joins = joinable(&lines, added, main);
    let mut moved: Vec<(Option<&str>, std::ops::Range<usize>)> = Vec::new();
    let (mut dropped, mut revived) = (Vec::new(), Vec::new());
    for entry in settled(&entries, added) {
        let said = words(&lines[entry.lines.clone()]);
        let theirs = in_main.get(&said).copied().unwrap_or(0);
        let ours = left.get_mut(&said).expect("every entry is counted");
        if *ours > theirs.max(1) {
            *ours -= 1;
            dropped.push(entry.lines.clone());
        } else if theirs > 0 {
            // Main holds these words: an entry moved or reordered, not one the cut stranded.
        } else {
            match (written.get(&said), entry.release) {
                // The branch wrote it under `[Unreleased]`, and it sits under a release main cut
                // after that: a release that cannot have shipped it.
                (Some(Wrote::Unreleased(beside)), Some(release))
                    if main_releases.contains(release) && !beside.contains(release) =>
                {
                    moved.push((entry.kind, entry.lines.clone()));
                }
                (Some(_), _) => {}
                // No commit of the branch wrote these words and main no longer holds them: main
                // rewrote or removed them, and a merge (git's union driver keeps both sides of a
                // hunk) brought them back.
                (None, _) => revived.push(entry.lines.clone()),
            }
        }
    }
    if moved.is_empty() && dropped.is_empty() && revived.is_empty() && joins.is_empty() {
        return None;
    }
    let mut gone: BTreeSet<usize> = moved
        .iter()
        .map(|(_, entry)| entry)
        .chain(&dropped)
        .chain(&revived)
        .flat_map(Clone::clone)
        .collect();
    for &heading in &released_kinds {
        if added.contains(&heading) && emptied(&lines, heading, &gone) {
            gone.insert(heading);
        }
    }
    // A joined subsection gives up its heading and the lines the landing adds; the ones no other
    // rule took join the subsection of their kind, a removal inside them leaving one blank line
    // as below.
    let mut join_blocks: Vec<(&str, bool, Vec<String>)> = Vec::new();
    for join in &joins {
        let mut block: Vec<String> = Vec::new();
        let mut after_gone = false;
        for n in join.body.clone() {
            if gone.contains(&n) {
                after_gone = true;
                continue;
            }
            if after_gone
                && lines[n].trim().is_empty()
                && block.last().is_none_or(|l| l.trim().is_empty())
            {
                continue;
            }
            after_gone = false;
            block.push(lines[n].to_owned());
        }
        gone.extend(join.remove.iter().copied());
        join_blocks.push((join.kind, join.above, block));
    }
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    let mut after_gone = false;
    for (n, line) in lines.iter().enumerate() {
        if gone.contains(&n) {
            after_gone = true;
            continue;
        }
        // A removal leaves one blank line where the entry had one on each side.
        if after_gone && line.trim().is_empty() && kept.last().is_none_or(|l| l.trim().is_empty()) {
            continue;
        }
        after_gone = false;
        kept.push((*line).to_owned());
    }
    let mut kinds: Vec<Option<&str>> = Vec::new();
    for (kind, _) in &moved {
        if !kinds.contains(kind) {
            kinds.push(*kind);
        }
    }
    for kind in kinds {
        let mut block: Vec<String> = Vec::new();
        for (_, entry) in moved.iter().filter(|(k, _)| *k == kind) {
            block.extend(lines[entry.clone()].iter().map(|l| (*l).to_owned()));
            block.push(String::new());
        }
        place(&mut kept, kind, block);
    }
    let mut joined: Vec<(String, usize)> = Vec::new();
    for (kind, _, _) in &join_blocks {
        match joined.iter_mut().find(|(k, _)| k == kind) {
            Some((_, count)) => *count += 1,
            None => joined.push(((*kind).to_owned(), 1)),
        }
    }
    for (kind, _) in &joined {
        let run = |above: bool| {
            let mut run: Vec<String> = Vec::new();
            for (_, _, block) in join_blocks
                .iter()
                .filter(|(k, a, _)| k == kind && *a == above)
            {
                let block = trimmed(block);
                if !block.is_empty() {
                    if !run.is_empty() {
                        run.push(String::new());
                    }
                    run.extend_from_slice(block);
                }
            }
            run
        };
        join_into(&mut kept, kind, run(true), run(false));
    }
    let titled = |entry: &std::ops::Range<usize>| title(&lines[entry.clone()]);
    Some(Rehomed {
        text: kept.join("\n"),
        moved: moved.iter().map(|(_, entry)| titled(entry)).collect(),
        dropped: dropped.iter().map(titled).collect(),
        revived: revived.iter().map(titled).collect(),
        joined,
    })
}

/// A `### <kind>` subsection the landing adds whole, which joins another of its kind.
struct Join<'a> {
    /// Its kind, trimmed.
    kind: &'a str,
    /// The lines it gives up, 0-based: its heading and the lines the landing adds after it.
    remove: BTreeSet<usize>,
    /// The lines after its heading, 0-based.
    body: std::ops::Range<usize>,
    /// Whether it sits above the subsection it joins.
    above: bool,
}

/// The `### <kind>` subsections under `## [Unreleased]` the landing adds whole (the heading and
/// every non-blank line up to the next heading in `added`, read as [`slid_up`] places it) where
/// the section holds another of the kind and more `### <kind>` headings than `main`'s
/// `## [Unreleased]` holds, at least one for each that joins, in file order: each joins the first
/// subsection of its kind the landing does not add whole (main's), else the first. A kind main's
/// own lines repeat stays as main holds it. None where the section holds an HTML comment or a
/// code fence at the margin: a `### ` line in one is not a heading.
fn joinable<'a>(lines: &[&'a str], added: &BTreeSet<usize>, main: &str) -> Vec<Join<'a>> {
    let Some((unreleased, end)) = unreleased_span(lines) else {
        return Vec::new();
    };
    if lines[unreleased + 1..end].iter().any(|line| {
        line.trim_start().starts_with("<!--") || line.starts_with("```") || line.starts_with("~~~")
    }) {
        return Vec::new();
    }
    let main_lines: Vec<&str> = main.split('\n').collect();
    let main_kinds: Vec<&str> = unreleased_span(&main_lines).map_or_else(Vec::new, |(u, e)| {
        main_lines[u + 1..e]
            .iter()
            .filter_map(|line| line.strip_prefix("### ").map(str::trim))
            .collect()
    });
    let added = slid_up(lines, added, unreleased + 1, end);
    let headings: Vec<usize> = (unreleased + 1..end)
        .filter(|&n| lines[n].starts_with("### "))
        .collect();
    // Each subsection: its kind, its lines, and whether the landing adds it whole.
    let subsections: Vec<(&str, std::ops::Range<usize>, bool)> = headings
        .iter()
        .enumerate()
        .map(|(i, &heading)| {
            let body_end = headings.get(i + 1).copied().unwrap_or(end);
            let whole = (heading..body_end)
                .all(|n| added.contains(&n) || lines[n].trim().is_empty())
                && added.contains(&heading);
            (
                lines[heading]["### ".len()..].trim(),
                heading..body_end,
                whole,
            )
        })
        .collect();
    let mut joins = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (kind, _, _) in &subsections {
        if seen.contains(kind) {
            continue;
        }
        seen.push(kind);
        let of_kind: Vec<usize> = (0..subsections.len())
            .filter(|&j| subsections[j].0 == *kind)
            .collect();
        let Some(target) = of_kind
            .iter()
            .copied()
            .find(|&j| !subsections[j].2)
            .or_else(|| of_kind.first().copied())
        else {
            continue;
        };
        let joining: Vec<usize> = of_kind
            .iter()
            .copied()
            .filter(|&j| j != target && subsections[j].2)
            .collect();
        let theirs = main_kinds.iter().filter(|k| *k == kind).count();
        if joining.is_empty() || of_kind.len() < theirs + joining.len() {
            continue;
        }
        for j in joining {
            let range = subsections[j].1.clone();
            joins.push(Join {
                kind,
                remove: range.clone().filter(|n| added.contains(n)).collect(),
                body: range.start + 1..range.end,
                above: j < target,
            });
        }
    }
    joins.sort_by_key(|join| join.body.start);
    joins
}

/// The span of `lines`' `## [Unreleased]` section: its heading's line and the line after its
/// last (the next `## ` heading, or the end).
fn unreleased_span(lines: &[&str]) -> Option<(usize, usize)> {
    let unreleased = lines.iter().position(|line| is_unreleased(line))?;
    let end = (unreleased + 1..lines.len())
        .find(|&n| lines[n].starts_with("## "))
        .unwrap_or(lines.len());
    Some((unreleased, end))
}

/// `added` with each run of its consecutive lines inside `from..to` moved up while the line above
/// the run is the same as its last line. Git's diff places an added run at one of the equal
/// places where the lines around it repeat, which leave the same text; this takes the earliest,
/// so a subsection added directly above one of its kind reads as added whole whichever of the
/// two identical headings the diff gave it.
fn slid_up(lines: &[&str], added: &BTreeSet<usize>, from: usize, to: usize) -> BTreeSet<usize> {
    let mut runs: Vec<std::ops::Range<usize>> = Vec::new();
    for &n in added {
        match runs.last_mut() {
            Some(run) if run.end == n => run.end += 1,
            _ => runs.push(n..n + 1),
        }
    }
    let mut slid = BTreeSet::new();
    for run in runs {
        let (mut start, mut end) = (run.start, run.end);
        if start >= from && end <= to {
            while start > from && !slid.contains(&(start - 1)) && lines[start - 1] == lines[end - 1]
            {
                start -= 1;
                end -= 1;
            }
        }
        slid.extend(start..end);
    }
    slid
}

/// `lines` without its leading and trailing blank lines.
fn trimmed(lines: &[String]) -> &[String] {
    let mut lines = lines;
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines = &lines[1..];
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines = &lines[..lines.len() - 1];
    }
    lines
}

/// Join `top` (the lines of subsections that sat above it) and `bottom` (of ones below) into the
/// first `### <kind>` subsection under `kept`'s `## [Unreleased]`: `top` at the top of its lines,
/// `bottom` after its last, each with one blank line around it. Its own lines stay as they are.
fn join_into(kept: &mut Vec<String>, kind: &str, top: Vec<String>, bottom: Vec<String>) {
    // The subsection's heading and the line after its last (the next heading, or the end).
    let bounds = |kept: &[String]| {
        let unreleased = kept.iter().position(|line| is_unreleased(line))?;
        let end = (unreleased + 1..kept.len())
            .find(|&n| kept[n].starts_with("## "))
            .unwrap_or(kept.len());
        let heading = (unreleased + 1..end).find(|&n| {
            kept[n]
                .strip_prefix("### ")
                .is_some_and(|h| h.trim() == kind)
        })?;
        let next = (heading + 1..end)
            .find(|&n| kept[n].starts_with("### "))
            .unwrap_or(end);
        Some((heading, next))
    };
    if !top.is_empty() {
        let Some((heading, next)) = bounds(kept) else {
            return;
        };
        let mut at = heading + 1;
        while at < next && kept[at].trim().is_empty() {
            at += 1;
        }
        let mut insert = Vec::new();
        if at == heading + 1 {
            insert.push(String::new());
        }
        insert.extend(top);
        insert.push(String::new());
        kept.splice(at..at, insert);
    }
    if !bottom.is_empty() {
        let Some((heading, next)) = bounds(kept) else {
            return;
        };
        let mut last = next;
        while last > heading + 1 && kept[last - 1].trim().is_empty() {
            last -= 1;
        }
        let mut insert = vec![String::new()];
        insert.extend(bottom);
        if !(last < kept.len() && kept[last].trim().is_empty()) {
            insert.push(String::new());
        }
        kept.splice(last..last, insert);
    }
}

fn is_unreleased(line: &str) -> bool {
    line.trim_end() == UNRELEASED
}

fn is_entry(line: &str) -> bool {
    line.starts_with("- ") || line.starts_with("* ")
}

fn is_heading(line: &str) -> bool {
    line.starts_with("## ") || line.starts_with("### ")
}

/// Where the entry starting at `start` ends: past its last non-blank line before the first later
/// line that is neither blank nor indented (another entry, a heading), so an entry's indented
/// paragraphs and the blank lines between them are its own, as census's `changelog-repeat` reads
/// one.
fn entry_end(lines: &[&str], start: usize) -> usize {
    let mut end = start + 1;
    let mut at = start + 1;
    while at < lines.len()
        && (lines[at].trim().is_empty() || lines[at].starts_with(char::is_whitespace))
    {
        at += 1;
        if !lines[at - 1].trim().is_empty() {
            end = at;
        }
    }
    end
}

/// Whether the subsection headed at `heading` loses its last entry to `gone`: it holds a line in
/// `gone`, and nothing but blank lines once `gone` goes.
fn emptied(lines: &[&str], heading: usize, gone: &BTreeSet<usize>) -> bool {
    let (mut lost, mut left) = (false, false);
    for (n, line) in lines
        .iter()
        .enumerate()
        .skip(heading + 1)
        .take_while(|(_, line)| !is_heading(line))
    {
        if gone.contains(&n) {
            lost = true;
        } else if !line.trim().is_empty() {
            left = true;
        }
    }
    lost && !left
}

/// Put `block` (entries, each followed by a blank line) at the top of `kind` under
/// `## [Unreleased]`, adding that subsection where the section has none.
fn place(kept: &mut Vec<String>, kind: Option<&str>, mut block: Vec<String>) {
    let Some(unreleased) = kept.iter().position(|line| is_unreleased(line)) else {
        return;
    };
    let end = kept
        .iter()
        .enumerate()
        .skip(unreleased + 1)
        .find(|(_, line)| line.starts_with("## "))
        .map_or(kept.len(), |(n, _)| n);
    let below_blanks = |from: usize| {
        let mut at = from;
        while at < end && kept[at].trim().is_empty() {
            at += 1;
        }
        at
    };
    let at = match kind {
        None => below_blanks(unreleased + 1),
        Some(kind) => {
            let own = (unreleased + 1..end).find(|&n| {
                kept[n]
                    .strip_prefix("### ")
                    .is_some_and(|h| h.trim() == kind)
            });
            match own {
                Some(heading) => below_blanks(heading + 1),
                None => {
                    let rank = |name: &str| KINDS.iter().position(|k| *k == name);
                    let later = rank(kind).and_then(|mine| {
                        (unreleased + 1..end).find(|&n| {
                            kept[n]
                                .strip_prefix("### ")
                                .and_then(|h| rank(h.trim()))
                                .is_some_and(|theirs| theirs > mine)
                        })
                    });
                    let mut heading = vec![format!("### {kind}"), String::new()];
                    heading.append(&mut block);
                    block = heading;
                    match later {
                        Some(n) => n,
                        None => {
                            if end > 0 && !kept[end - 1].trim().is_empty() {
                                block.insert(0, String::new());
                            }
                            end
                        }
                    }
                }
            }
        }
    };
    kept.splice(at..at, block);
}

/// An entry's title: its bold lead (`- **…**`, which may wrap), else its first words.
fn title(entry: &[&str]) -> String {
    let words = entry
        .iter()
        .flat_map(|line| line.split_whitespace())
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(lead) = words.strip_prefix("**")
        && let Some((bold, _)) = lead.split_once("**")
    {
        return bold.to_owned();
    }
    words.chars().take(72).collect()
}

/// What the landing did, in a clause: the entries it moved and the ones it dropped, each by its
/// title, and the subsections it joined, by their kind.
pub(super) fn said(rehomed: &Rehomed) -> String {
    let quoted = |titles: &[String]| {
        titles
            .iter()
            .map(|title| format!("\"{title}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let one = |titles: &[String]| titles.len() == 1;
    let mut parts = Vec::new();
    if !rehomed.moved.is_empty() {
        let moved = &rehomed.moved;
        parts.push(format!(
            "moved {} {} this branch adds from under a release cut after {} written to \
             [Unreleased]: {}",
            moved.len(),
            if one(moved) {
                "CHANGELOG entry"
            } else {
                "CHANGELOG entries"
            },
            if one(moved) { "it was" } else { "they were" },
            quoted(moved)
        ));
    }
    if !rehomed.dropped.is_empty() {
        let dropped = &rehomed.dropped;
        parts.push(format!(
            "dropped {} of {} the changelog holds elsewhere from under a release cut: {}",
            if one(dropped) {
                "a copy".to_owned()
            } else {
                format!("{} copies", dropped.len())
            },
            if one(dropped) { "an entry" } else { "entries" },
            quoted(dropped)
        ));
    }
    if !rehomed.revived.is_empty() {
        let revived = &rehomed.revived;
        parts.push(format!(
            "dropped {} no commit of this branch wrote and main no longer holds, which a merge \
             brought back: {}",
            if one(revived) {
                "an entry's words".to_owned()
            } else {
                format!("{} entries' words", revived.len())
            },
            quoted(revived)
        ));
    }
    if !rehomed.joined.is_empty() {
        let joined = rehomed
            .joined
            .iter()
            .map(|(kind, count)| match count {
                1 => format!("a `### {kind}` subsection"),
                _ => format!("{count} `### {kind}` subsections"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "joined {joined} this branch adds under [Unreleased] beside another of its kind into \
             that one"
        ));
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: &str = "# Changelog

Write each change under `## [Unreleased]`.

## [Unreleased]

## [0.2.0] - 2026-11-08

### Changed

- **Main's change.** Shipped in 0.2.0.

### Fixed

- **Main's fix.** Shipped in 0.2.0,
  over two lines.

## [0.1.0] - 2026-11-01

### Fixed

- **The first fix.** Shipped in 0.1.0.
";

    /// What a branch wrote under `[Unreleased]`: `entries`.
    fn wrote_unreleased(entries: &[&str]) -> Written {
        entries
            .iter()
            .map(|entry| {
                let said = words(&entry.split('\n').collect::<Vec<_>>());
                (said, Wrote::Unreleased(BTreeSet::new()))
            })
            .collect()
    }

    /// `text` with `entry` inserted before the line `before`: the text, and the inserted lines.
    fn insert(text: &str, before: &str, entry: &str) -> (String, BTreeSet<usize>) {
        let at = text.find(before).expect("the anchor");
        let first = text[..at].matches('\n').count();
        let count = entry.matches('\n').count();
        (
            format!("{}{entry}{}", &text[..at], &text[at..]),
            (first..first + count).collect(),
        )
    }

    /// THE RULE: an entry a branch wrote into `[Unreleased]` that the merge put under a release
    /// cut meanwhile goes back to the top of its kind there, and the released section is left as
    /// the release shipped it. CONTROLS: nothing moves where the entry is under `[Unreleased]`
    /// already, where every line is added but main holds each entry (main's own changelog), where
    /// main holds the entry's words (an entry moved or reordered among the releases), where the
    /// branch wrote it under the release (a restored or backfilled entry), where it wrote it
    /// under `[Unreleased]` after that release was cut (and moved it there), or where main names
    /// no such release (a cut of the branch's own); and words no commit of the branch wrote,
    /// which main does not hold, go (a merge brought them back).
    #[test]
    fn an_added_entry_under_a_released_version_moves_to_the_top_of_its_kind_unreleased() {
        let entry = "- **The branch's fix.** Written before the cut,\n  landed after it.\n\n";
        let branch = wrote_unreleased(&[entry]);
        let (merged, added) = insert(MAIN, "- **Main's fix.**", entry);
        let rehomed = rehome(&merged, &added, MAIN, &branch).expect("the stranded entry moves");
        assert_eq!(
            (rehomed.moved, rehomed.dropped),
            (vec!["The branch's fix.".to_owned()], Vec::new())
        );
        assert_eq!(
            rehomed.text,
            MAIN.replace(
                "## [Unreleased]\n\n## [0.2.0]",
                "## [Unreleased]\n\n### Fixed\n\n- **The branch's fix.** Written before the \
                 cut,\n  landed after it.\n\n## [0.2.0]"
            )
        );
        let in_place = rehomed.text.clone();
        let placed: BTreeSet<usize> = (6..10).collect();
        assert_eq!(
            rehome(&in_place, &placed, MAIN, &branch),
            None,
            "already under [Unreleased]"
        );
        let every: BTreeSet<usize> = (0..MAIN.lines().count()).collect();
        assert_eq!(
            rehome(MAIN, &every, MAIN, &Written::new()),
            None,
            "main's own entries"
        );
        assert_eq!(
            rehome(&merged, &added, &merged, &branch),
            None,
            "main holds it"
        );
        let as_written = |wrote: Wrote| -> Written {
            branch
                .keys()
                .map(|said| (said.clone(), wrote.clone()))
                .collect()
        };
        assert_eq!(
            rehome(&merged, &added, MAIN, &as_written(Wrote::Released)),
            None,
            "the branch wrote it under the release"
        );
        let beside = Wrote::Unreleased(BTreeSet::from(["## [0.2.0] - 2026-11-08".to_owned()]));
        assert_eq!(
            rehome(&merged, &added, MAIN, &as_written(beside)),
            None,
            "the branch wrote it after 0.2.0 was cut and moved it there"
        );
        let uncut = MAIN.replace("## [0.2.0] - 2026-11-08\n\n", "");
        assert_eq!(
            rehome(&merged, &added, &uncut, &branch),
            None,
            "a release main does not name"
        );
        let revived = rehome(&merged, &added, MAIN, &Written::new()).expect("words main removed");
        assert_eq!(
            (revived.moved, revived.revived, revived.text),
            (
                Vec::new(),
                vec!["The branch's fix.".to_owned()],
                MAIN.to_owned()
            ),
            "no commit of the branch wrote it and main does not hold it: a merge brought it back"
        );
    }

    /// An entry joins an `[Unreleased]` subsection of its kind at its top; a kind the section
    /// lacks is added in Keep a Changelog's order (Changed before Fixed); several entries keep
    /// their order; and a `### <kind>` heading the branch added with them, left empty, goes too.
    #[test]
    fn moved_entries_join_their_kind_in_order_and_take_an_emptied_added_heading_with_them() {
        let main = MAIN.replace(
            "## [Unreleased]\n\n",
            "## [Unreleased]\n\n### Fixed\n\n- **A fix after the cut.** Unreleased.\n\n",
        );
        let (merged, mut added) = insert(
            &main,
            "- **Main's change.**",
            "- **First change.** One.\n\n- **Second change.** Two.\n\n",
        );
        let (merged, more) = insert(
            &merged,
            "## [0.1.0]",
            "### Removed\n\n- **A removal.** Gone.\n\n",
        );
        // The second insertion shifts nothing before it.
        added.extend(more);
        let branch = wrote_unreleased(&[
            "- **First change.** One.",
            "- **Second change.** Two.",
            "- **A removal.** Gone.",
        ]);
        let rehomed = rehome(&merged, &added, &main, &branch).expect("three entries move");
        assert_eq!(
            rehomed.moved,
            ["First change.", "Second change.", "A removal."]
        );
        let unreleased = rehomed
            .text
            .split("## [0.2.0]")
            .next()
            .expect("the head")
            .split("\n## [Unreleased]")
            .nth(1)
            .expect("the section")
            .to_owned();
        assert_eq!(
            unreleased,
            "\n\n### Changed\n\n- **First change.** One.\n\n- **Second change.** Two.\n\n\
             ### Removed\n\n- **A removal.** Gone.\n\n### Fixed\n\n- **A fix after the cut.** \
             Unreleased.\n\n"
        );
        assert!(
            rehomed
                .text
                .contains("over two lines.\n\n## [0.1.0] - 2026-11-01\n\n### Fixed\n"),
            "the emptied `### Removed` the branch added goes:\n{}",
            rehomed.text
        );
        assert_eq!(
            rehomed.text.matches("\n\n\n").count(),
            0,
            "no doubled blank line:\n{}",
            rehomed.text
        );
    }

    /// A HUNK THAT REWRITES ONE OF MAIN'S LINES IS AN EDIT, NOT AN ADDITION: a fix to a released
    /// entry's words stays in its release. CONTROL: the same lines from a pure insertion, and one
    /// whose only removed line is blank (git's union merge can drop the blank line between two
    /// entries), are added.
    #[test]
    fn only_lines_a_hunk_adds_without_rewriting_a_line_of_mains_are_added() {
        let edit = "diff --git a/CHANGELOG.md b/CHANGELOG.md\n--- a/CHANGELOG.md\n+++ \
                    b/CHANGELOG.md\n@@ -11 +11 @@ ### Changed\n-- **Main's chnage.** Shipped.\n+- \
                    **Main's change.** Shipped.\n";
        assert_eq!(added_lines(edit), BTreeSet::new());
        let insertion = "--- a/CHANGELOG.md\n+++ b/CHANGELOG.md\n@@ -14,0 +15,3 @@ ### \
                         Fixed\n+- **New.** One\n+  two.\n+\n";
        assert_eq!(added_lines(insertion), (14..17).collect());
        let blank_dropped = "@@ -14 +14,3 @@\n-\n+\n+- **New.** One.\n+\n@@ -30,0 +31 @@\n+x\n";
        assert_eq!(added_lines(blank_dropped), BTreeSet::from([13, 14, 15, 30]));
        let deleted_only = "@@ -3,2 +2,0 @@\n-a\n-b\n";
        assert_eq!(added_lines(deleted_only), BTreeSet::new());
    }

    /// A HUNK GIT JOINED ACROSS UNCHANGED LINES (`diff.interHunkContext`) READS AS THE CHANGES IT
    /// HOLDS: its context lines (blank ones written empty under `diff.suppressBlankEmpty`
    /// included) are never added, and each change is judged alone, so a rewrite beside an
    /// insertion keeps only the insertion's lines.
    #[test]
    fn a_joined_hunk_adds_only_its_own_insertions() {
        let joined = "@@ -10,3 +10,7 @@\n+- **N1.** One.\n+\n - **A.** Main's.\n \n+- **N2.** \
                      Two.\n+\n";
        assert_eq!(added_lines(joined), BTreeSet::from([9, 10, 13, 14]));
        assert_eq!(
            added_lines(&joined.replace("\n \n", "\n\n")),
            BTreeSet::from([9, 10, 13, 14])
        );
        let beside = "@@ -5,2 +5,3 @@\n-old\n+new\n ctx\n+add\n";
        assert_eq!(added_lines(beside), BTreeSet::from([6]));
    }

    /// A STRANDED COPY OF AN ENTRY THE FILE HOLDS ELSEWHERE IS DROPPED, NOT MOVED: a branch
    /// that once merged main's unreleased entry, merged across the cut that left main's copy
    /// under `[Unreleased]`, holds a second copy under the release (2026-11-08, measured on a
    /// real merge), and moving it would only make `[Unreleased]` say it twice. CONTROL: a new
    /// entry beside it moves, and a title whose bold lead wraps onto its next line is whole.
    #[test]
    fn a_stranded_copy_of_an_entry_held_elsewhere_is_dropped_and_a_wrapped_title_is_whole() {
        let main = MAIN.replace(
            "## [Unreleased]\n\n",
            "## [Unreleased]\n\n### Fixed\n\n- **Main's late fix.** Not in 0.2.0.\n\n",
        );
        let (merged, added) = insert(
            &main,
            "- **Main's fix.**",
            "- **Main's late fix.** Not in 0.2.0.\n\n- **A new fix whose title\n  wraps.** \
             Its words.\n\n",
        );
        let branch = wrote_unreleased(&["- **A new fix whose title
  wraps.** Its words."]);
        let rehomed = rehome(&merged, &added, &main, &branch).expect("one moves, one drops");
        assert_eq!(rehomed.moved, ["A new fix whose title wraps."]);
        assert_eq!(rehomed.dropped, ["Main's late fix."]);
        assert_eq!(
            rehomed.text,
            main.replace(
                "### Fixed\n\n- **Main's late fix.**",
                "### Fixed\n\n- **A new fix whose title\n  wraps.** Its words.\n\n- **Main's \
                 late fix.**"
            )
        );
        assert_eq!(
            said(&rehomed),
            "moved 1 CHANGELOG entry this branch adds from under a release cut after it was \
             written to [Unreleased]: \"A new fix whose title wraps.\"; dropped a copy of an \
             entry the changelog holds elsewhere from under a release cut: \"Main's late fix.\""
        );
    }

    /// An entry of two paragraphs moves whole: its indented second paragraph is its own.
    /// CONTROL: the entry after it, main's, stays where it is.
    #[test]
    fn an_entry_of_two_paragraphs_moves_whole() {
        let entry = "- **Two paragraphs.** The first.\n\n  The second.\n\n";
        let (merged, added) = insert(MAIN, "- **Main's fix.**", entry);
        let rehomed =
            rehome(&merged, &added, MAIN, &wrote_unreleased(&[entry])).expect("the entry moves");
        assert_eq!(rehomed.moved, ["Two paragraphs."]);
        assert_eq!(
            rehomed.text,
            MAIN.replace(
                "## [Unreleased]\n\n## [0.2.0]",
                &format!("## [Unreleased]\n\n### Fixed\n\n{entry}## [0.2.0]")
            )
        );
    }

    /// An entry whose next non-blank line starts at the margin but opens nothing (a code fence,
    /// straight after it or after a blank line) has no clear end, so it stays; and an empty
    /// `### <kind>` the branch added under `[Unreleased]` stays while a stranded entry moves.
    /// CONTROL: the same entry with an indented fence moves.
    #[test]
    fn an_entry_without_a_clear_end_stays_and_an_unreleased_heading_is_never_swept() {
        let branch = wrote_unreleased(&["- **Code.** Run:"]);
        for fenced in [
            "- **Code.** Run:\n```\nx\n```\n\n",
            "- **Code.** Run:\n\n```\nx\n```\n\n",
        ] {
            let (merged, added) = insert(MAIN, "- **Main's fix.**", fenced);
            assert_eq!(rehome(&merged, &added, MAIN, &branch), None, "{fenced:?}");
        }
        let indented = "- **Code.** Run:\n  ```\n  x\n  ```\n\n";
        let (merged, added) = insert(MAIN, "- **Main's fix.**", indented);
        let (merged, more) = insert(&merged, "## [0.2.0]", "### Security\n\n");
        let added: BTreeSet<usize> = added.iter().map(|n| n + 2).chain(more).collect();
        let indented_branch = wrote_unreleased(&[indented]);
        let rehomed =
            rehome(&merged, &added, MAIN, &indented_branch).expect("the indented one moves");
        assert_eq!(rehomed.moved, ["Code."]);
        assert!(
            rehomed.text.contains("### Security\n\n## [0.2.0]"),
            "{}",
            rehomed.text
        );
    }

    #[test]
    fn a_changelog_with_no_unreleased_section_is_left_alone() {
        let text = "# Changelog\n\n## [0.1.0]\n\n- **Fix.** One.\n";
        assert_eq!(rehome(text, &(0..6).collect(), "", &Written::new()), None);
    }

    /// A changelog whose `## [Unreleased]` body is `unreleased`, above one release.
    fn changelog(unreleased: &str) -> String {
        format!(
            "# Changelog\n\n## [Unreleased]\n\n{unreleased}## [0.1.0] - 2026-11-01\n\n### \
             Fixed\n\n- **The first fix.** Shipped.\n"
        )
    }

    /// `text`'s 0-based lines that `main` lacks, as a diff of the two would add them: here,
    /// one run of lines inserted after `main`'s own.
    fn appended(text: &str, main: &str) -> BTreeSet<usize> {
        let (ours, theirs): (Vec<&str>, Vec<&str>) =
            (text.split('\n').collect(), main.split('\n').collect());
        let lead = ours.iter().zip(&theirs).take_while(|(a, b)| a == b).count();
        let tail = ours[lead..]
            .iter()
            .rev()
            .zip(theirs[lead..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        (lead..ours.len() - tail).collect()
    }

    /// A `### <kind>` SUBSECTION THE LANDING ADDS BESIDE MAIN'S OF ITS KIND JOINS IT, over
    /// 7d1464cf8bb's shape as a real `git merge --no-ff` under the union driver writes it
    /// (2026-11-10): the branch put `### Fixed` first and reworded the entry under
    /// `### Changed`; main wrote a new entry above that one; the merge kept main's side whole
    /// and added the branch's `### Changed` below it. The added subsection's entry follows main's
    /// last, its heading goes, and main's lines stay as they are; running it again changes
    /// nothing. The mirror (the added one above main's) puts its entries at the top of main's,
    /// and a surplus copy inside an added subsection goes with one blank line left. CONTROLS: a
    /// kind main's own lines say twice stays; a subsection the landing adds only in part (main's
    /// entry inside it) stays; a section holding an HTML comment is left alone; an added bare
    /// `### <kind>` under a release, with nothing to join or settle, is `None`.
    /// NEGATIVE CONTROL (2026-11-10): without the join, `rehome` returns `None` here; joining
    /// into the first of the kind fails the mirror, a join block kept without the blank-line
    /// collapse doubles the blank where the copy sat, and no early `None` fails the bare heading.
    #[test]
    fn a_kind_the_landing_adds_a_second_time_joins_mains() {
        let old = "- **Old words.** Said once.\n\n";
        let mains = "- **Main's change.** Written meanwhile.\n\n";
        let fixed = "### Fixed\n\n- **First fix.** One.\n\n";
        let new = "- **New words.** Said better.\n\n";
        let main = changelog(&format!("### Changed\n\n{mains}{old}{fixed}"));
        let union = changelog(&format!(
            "### Changed\n\n{mains}{old}{fixed}### Changed\n\n{new}"
        ));
        let added = appended(&union, &main);
        let branch = wrote_unreleased(&[new]);
        let rehomed = rehome(&union, &added, &main, &branch).expect("the kinds join");
        assert_eq!(rehomed.joined, [("Changed".to_owned(), 1)]);
        assert_eq!(
            rehomed.text,
            changelog(&format!("### Changed\n\n{mains}{old}{new}{fixed}"))
        );
        assert_eq!(
            said(&rehomed),
            "joined a `### Changed` subsection this branch adds under [Unreleased] beside another \
             of its kind into that one"
        );
        let mirror_main = changelog(&format!("{fixed}### Changed\n\n{mains}{old}"));
        let mirror = changelog(&format!(
            "### Changed\n\n{new}{fixed}### Changed\n\n{mains}{old}"
        ));
        let mirrored = rehome(
            &mirror,
            &appended(&mirror, &mirror_main),
            &mirror_main,
            &branch,
        )
        .expect("the mirror joins");
        assert_eq!(mirrored.joined, [("Changed".to_owned(), 1)]);
        assert_eq!(
            mirrored.text,
            changelog(&format!("{fixed}### Changed\n\n{new}{mains}{old}"))
        );
        let next = "- **Next words.** Said last.\n\n";
        let copied = changelog(&format!(
            "### Changed\n\n{mains}{old}{fixed}### Changed\n\n{new}{mains}{next}"
        ));
        let both = wrote_unreleased(&[new, next]);
        let rehomed_copy = rehome(&copied, &appended(&copied, &main), &main, &both)
            .expect("the copy goes and the kinds join");
        assert_eq!(rehomed_copy.dropped, ["Main's change."]);
        assert_eq!(
            rehomed_copy.text,
            changelog(&format!("### Changed\n\n{mains}{old}{new}{next}{fixed}"))
        );
        let (bare, bare_added) = insert(
            &main,
            "### Fixed\n\n- **The first fix.**",
            "### Security\n\n",
        );
        assert_eq!(
            rehome(&bare, &bare_added, &main, &branch),
            None,
            "an added bare heading under a release"
        );
        let settled = appended(&rehomed.text, &main);
        assert_eq!(
            rehome(&rehomed.text, &settled, &main, &branch),
            None,
            "settled"
        );
        assert_eq!(
            rehome(&union, &BTreeSet::new(), &union, &branch),
            None,
            "main's own lines say `### Changed` twice"
        );
        // The branch adds a `### Changed` heading above main's entry and its own entry below it.
        let partly_main = changelog(&format!("### Changed\n\n{old}{fixed}{mains}"));
        let (step, mut partly_added) =
            insert(&partly_main, "- **Main's change.**", "### Changed\n\n");
        let (partly, more) = insert(&step, "## [0.1.0]", new);
        partly_added.extend(more);
        assert_eq!(
            rehome(&partly, &partly_added, &partly_main, &branch),
            None,
            "the second `### Changed` holds main's entry"
        );
        let commented = union.replace(
            "## [Unreleased]\n\n",
            "## [Unreleased]\n\n<!-- note -->\n\n",
        );
        let commented_main = main.replace(
            "## [Unreleased]\n\n",
            "## [Unreleased]\n\n<!-- note -->\n\n",
        );
        assert_eq!(
            rehome(
                &commented,
                &appended(&commented, &commented_main),
                &commented_main,
                &branch
            ),
            None,
            "an HTML comment in the section"
        );
    }

    /// WHICH LINES A JOIN TAKES: only its heading and the lines the landing adds, so a blank line
    /// main holds after it (a compact section's, or the file's last line) stays; a subsection
    /// added directly above one of its kind joins whichever identical heading git's diff gave it;
    /// two of one kind join and are counted; a bare heading the branch added under a release stays
    /// beside a join. CONTROL: a branch that reorders a kind main's own lines repeat adds no
    /// heading main lacks, and the kind stays as it holds it.
    /// NEGATIVE CONTROL (2026-11-10): removing the joined subsection's whole range loses the
    /// blank lines and final newline; without the slide the adjacent subsection never joins;
    /// without the heading count main's repeat is joined; an `emptied` that needs no lost line
    /// removes the bare heading.
    #[test]
    fn a_join_takes_only_the_lines_the_landing_adds() {
        let new = "- **New words.** N.";
        let branch = wrote_unreleased(&[new, "- **B2.** Two.", "- **E5.** e5."]);
        // A compact section: no blank line before the added heading, main's blank line after it.
        let main = "# Changelog\n\n## [Unreleased]\n\n### Changed\n\n- **M.** m.\n\n### Fixed\n\n- \
                    **F1.** one.\n- **F2.** two.\n\n## [0.1.0] - 2026-11-01\n";
        let (compact, added) = insert(main, "\n## [0.1.0]", &format!("### Changed\n\n{new}\n"));
        let rehomed = rehome(&compact, &added, main, &branch).expect("the kinds join");
        assert_eq!(
            rehomed.text,
            "# Changelog\n\n## [Unreleased]\n\n### Changed\n\n- **M.** m.\n\n- **New words.** \
             N.\n\n### Fixed\n\n- **F1.** one.\n- **F2.** two.\n\n## [0.1.0] - 2026-11-01\n"
        );
        // `## [Unreleased]` the file's last section: its final newline stays.
        let last = "# Changelog\n\n## [Unreleased]\n### Changed\n- **M.** m.\n### Fixed\n- **F.** \
                    f.\n";
        let at_end = format!("{last}### Changed\n{new}\n");
        let added: BTreeSet<usize> = [7, 8].into();
        let rehomed = rehome(&at_end, &added, last, &branch).expect("the kinds join");
        assert_eq!(
            rehomed.text,
            "# Changelog\n\n## [Unreleased]\n### Changed\n- **M.** m.\n\n- **New words.** \
             N.\n\n### Fixed\n- **F.** f.\n"
        );
        // Directly above main's `### Changed`, as git's diff gives it: the added run starts at
        // the entry and takes the lower heading.
        let e1 = "- **E1.** e1.\n\n";
        let e2 = "- **E2.** e2.\n\n";
        let e5 = "- **E5.** e5.\n\n";
        let main = changelog(&format!("### Fixed\n\n{e1}### Changed\n\n{e2}"));
        let (adjacent, added) = insert(&main, "- **E2.**", &format!("{e5}### Changed\n\n"));
        let rehomed = rehome(&adjacent, &added, &main, &branch).expect("the adjacent one joins");
        assert_eq!(
            rehomed.text,
            changelog(&format!("### Fixed\n\n{e1}### Changed\n\n{e5}{e2}"))
        );
        assert_eq!(
            rehome(
                &rehomed.text,
                &appended(&rehomed.text, &main),
                &main,
                &branch
            ),
            None,
            "settled"
        );
        // Two `### Changed` the landing adds, and a bare `### Security` under the release.
        let m1 = "- **M1.** Main.\n\n";
        let f1 = "- **F1.** Fix.\n\n";
        let main = changelog(&format!("### Changed\n\n{m1}### Fixed\n\n{f1}"));
        let (step, mut added) = insert(&main, "### Fixed", "### Changed\n\n- **B1.** One.\n\n");
        let (step, more) = insert(&step, "## [0.1.0]", "### Changed\n\n- **B2.** Two.\n\n");
        added.extend(more);
        let at = step.matches('\n').count() - 3;
        let (twice, more) = insert(
            &step,
            "### Fixed\n\n- **The first fix.**",
            "### Security\n\n",
        );
        let more: Vec<usize> = more.into_iter().collect();
        assert_eq!(more[0], at, "the heading sits under the release");
        added.extend(more);
        let branch = wrote_unreleased(&["- **B1.** One.", "- **B2.** Two."]);
        let rehomed = rehome(&twice, &added, &main, &branch).expect("both join");
        assert_eq!(rehomed.joined, [("Changed".to_owned(), 2)]);
        assert_eq!(
            said(&rehomed),
            "joined 2 `### Changed` subsections this branch adds under [Unreleased] beside \
             another of its kind into that one"
        );
        assert_eq!(
            rehomed.text,
            changelog(&format!(
                "### Changed\n\n{m1}- **B1.** One.\n\n- **B2.** Two.\n\n### Fixed\n\n{f1}"
            ))
            .replace(
                "### Fixed\n\n- **The first fix.**",
                "### Security\n\n### Fixed\n\n- **The first fix.**"
            )
        );
        // A branch that reorders main's own repeat: `### Fixed` twice before and after.
        let a1 = "- **A1.** a1.\n\n";
        let b1 = "- **B1.** b1.\n\n";
        let c1 = "- **C1.** c1.\n\n";
        let main = changelog(&format!(
            "### Fixed\n\n{a1}### Changed\n\n{c1}### Fixed\n\n{b1}"
        ));
        let reordered = changelog(&format!(
            "### Changed\n\n{c1}### Fixed\n\n{b1}### Fixed\n\n{a1}"
        ));
        // The diff deletes main's first `### Fixed` (a removal of non-blank lines adds nothing)
        // and adds the last one.
        let start = reordered
            .split('\n')
            .position(|l| l == "- **A1.** a1.")
            .expect("A1")
            - 2;
        let added: BTreeSet<usize> = (start..start + 4).collect();
        assert_eq!(
            rehome(&reordered, &added, &main, &branch),
            None,
            "main's own repeat"
        );
    }

    /// WHAT A COMMIT WROTE is each entry it holds that none of its parents does, a merge's own
    /// resolution (words in neither parent) included, with the release headings its changelog
    /// holds where it wrote it under `[Unreleased]`: one a commit moves down (a cut, merged or
    /// replayed) is not written, one moved up from a release is written under `[Unreleased]`, and
    /// a release beside every write under `[Unreleased]` is kept, in whichever order the writes
    /// come.
    /// NEGATIVE CONTROL (2026-11-08): with a later write replacing an earlier one, the write under
    /// a release replaces the write under `[Unreleased]`.
    #[test]
    fn a_commit_writes_the_entries_none_of_its_parents_holds() {
        let wanted: BTreeSet<String> = [
            "- **New.** One.",
            "- **Resolved.** In the merge.",
            "- **Mine.** Two.",
        ]
        .map(str::to_owned)
        .into();
        let placed = |changelog: &str| placed(changelog, &wanted);
        let main = placed(MAIN);
        let both: BTreeSet<String> = ["## [0.1.0] - 2026-11-01", "## [0.2.0] - 2026-11-08"]
            .map(str::to_owned)
            .into();
        let mut written = Written::new();
        let (with_new, _) = insert(MAIN, "- **Main's fix.**", "- **New.** One.\n\n");
        wrote(&mut written, &placed(&with_new), &[&main]);
        let (resolved, _) = insert(MAIN, "## [0.2.0]", "- **Resolved.** In the merge.\n\n");
        wrote(
            &mut written,
            &placed(&resolved),
            &[&main, &placed(&with_new)],
        );
        assert_eq!(
            written,
            Written::from([
                ("- **New.** One.".to_owned(), Wrote::Released),
                (
                    "- **Resolved.** In the merge.".to_owned(),
                    Wrote::Unreleased(both.clone())
                ),
            ])
        );

        let mine = "- **Mine.** Two.";
        let early = MAIN.replace("## [0.2.0] - 2026-11-08\n\n", "");
        let (unreleased_early, _) = insert(&early, "### Changed", "- **Mine.** Two.\n\n");
        let cut = unreleased_early.replace(
            "## [Unreleased]\n\n",
            "## [Unreleased]\n\n## [0.2.0] - 2026-11-08\n\n",
        );
        let (unreleased_late, _) = insert(MAIN, "## [0.2.0]", "- **Mine.** Two.\n\n");
        let (released, _) = insert(MAIN, "- **Main's fix.**", "- **Mine.** Two.\n\n");
        let [early, unreleased_early, cut, unreleased_late, released] =
            [early, unreleased_early, cut, unreleased_late, released].map(|t| placed(&t));
        let only_early = Wrote::Unreleased(BTreeSet::from(["## [0.1.0] - 2026-11-01".to_owned()]));
        let mut written = Written::new();
        wrote(&mut written, &unreleased_early, &[&early]);
        assert_eq!(written.get(mine), Some(&only_early), "written before 0.2.0");
        wrote(&mut written, &cut, &[&unreleased_early]);
        assert_eq!(written.get(mine), Some(&only_early), "a cut writes nothing");
        wrote(&mut written, &released, &[&main]);
        assert_eq!(
            written.get(mine),
            Some(&only_early),
            "nor does a write under a release"
        );
        wrote(&mut written, &unreleased_late, &[&main]);
        assert_eq!(written.get(mine), Some(&only_early), "beside every write");
        let mut reversed = Written::new();
        wrote(&mut reversed, &released, &[&main]);
        assert_eq!(reversed.get(mine), Some(&Wrote::Released));
        wrote(&mut reversed, &unreleased_late, &[&main]);
        wrote(&mut reversed, &unreleased_early, &[&early]);
        assert_eq!(reversed, written, "in either order");
        let mut moved_up = Written::new();
        wrote(&mut moved_up, &released, &[&main]);
        wrote(&mut moved_up, &unreleased_late, &[&released]);
        assert_eq!(
            moved_up.get(mine),
            Some(&Wrote::Unreleased(both.clone())),
            "a move up writes it under [Unreleased]"
        );
    }

    /// AN ENTRY IN QUESTION is one the landing adds whole and ending clearly whose words main does
    /// not hold, and only those are placed, a rewrapped copy by its words: main's entry moved, an
    /// entry with no clear end, and one the walk does not want are not.
    /// NEGATIVE CONTROL (2026-11-08): with an entry's first words read from its first line alone,
    /// the rewrapped entry is not placed.
    #[test]
    fn only_the_entries_in_question_are_placed() {
        let (text, unreleased) = insert(
            MAIN,
            "## [0.2.0]",
            "- **Main's change.** Shipped in 0.2.0.\n\n- **Mine.**\n  Two.\n\n",
        );
        let (text, fenced) = insert(&text, "## [0.1.0]", "- **Fenced.** Three.\n```\n\n");
        let added: BTreeSet<usize> = unreleased.into_iter().chain(fenced).collect();
        let mine = "- **Mine.** Two.".to_owned();
        assert_eq!(
            in_question(&text, &added, MAIN),
            BTreeSet::from([mine.clone()])
        );
        let mains = "- **Main's fix.** Shipped in 0.2.0, over two lines.".to_owned();
        assert_eq!(
            placed(&text, &BTreeSet::from([mine.clone(), mains.clone()])).entries,
            BTreeMap::from([(mine, true), (mains, false)])
        );
    }
}
