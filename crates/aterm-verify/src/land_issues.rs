// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! THE OPEN-ISSUES FILE ACROSS A LANDING'S MERGES ([`aterm_census::known_issues`]).
//! `docs/KNOWN_ISSUES.md` merges as a union of both sides' lines, which is no merge of its
//! entries: where one side closed an entry (deleted it) and the other changed lines beside it,
//! Git's merge keeps the closed entry, and where Git's diff lines up the lines of one entry with
//! those of another, the merge puts one entry's heading over another's text, or files a side's
//! lines under another entry's heading; a closed entry would reach main as open work, and the
//! lines of one an entry lost would be dropped with the heading they sit under. The landing
//! holds the file to an entry-level
//! merge ([`fixed`]): in the checkout before the landing's fixes commit ([`super::autofix`],
//! after each merge the landing makes and where the branch already holds main), and in a merge
//! of main a person made into a kept judgment ([`super::Git::kept_merge`]). The tree a round of
//! a moved main expects ([`super::Composition::Rechecks`]), and the tree a round judges as Git's
//! own merge ([`super::round_delta`]), are Git's merge with the file so held
//! ([`Git::landing_tree`]), so the rule leaves a round a round.
//!
//! The rule replays the file through each commit HEAD holds unlike main, parents first, from the
//! files main's commits hold, each step an edit made on a repair ([`reapply`]): each part
//! ([`known_issues::parts`]) as the entry-level merge of the text before the edit, the repair of it
//! and the edit decides it ([`known_issues::merge`]), where both changed it unlike each other as
//! their three-way merge line by line ([`known_issues::merge_part`]). A commit of one parent is its
//! change of its parent's file, made on its parent's replay. A merge of two commits is Git's union
//! of its two sides ([`Git::union_tree`]) and what the merge changed of it,
//! made on the entry-level merge of its sides' replays over its base's: the union's parts as that
//! merge decides them, dropped where it holds no such part, and a part of which the union lost the
//! heading put back after the part before it on the side that holds it. So a line one side wrote
//! stays in the entry it was written in, whatever heading the union filed it under, and a person's
//! edit of a merge, or a later commit's, is kept with the repair: the edit's change of a line the
//! repair moved to another part is made there, and a line the edit wrote into an entry the repair
//! closed goes with the text it follows ([`Edit`]). An entry the repair closed stays closed unless
//! the edit wrote a line into it or retitled it; a part the repair holds and the edit does not
//! stays where the edit took out no more than its heading and the step holds its lines nowhere
//! else. No step loses a line of a part's text the edit holds, or makes one up, beyond what the
//! repair moved, took out or put in ([`accounts`]): where the merge line by line would, the step
//! takes each part as the edit holds it where the edit changed it and as the repair holds it
//! elsewhere, which the count always passes. Several merge bases are folded in Git's order into
//! virtual commits: their raw union trees and original parents, with their entry-level replays
//! held separately ([`Replay::virtual_base`]). This fold handles readable regular issue files
//! merged by the built-in union driver, including proven split retitles, not arbitrary
//! whole-tree conflict resolution: a missing,
//! nonregular, binary or unreadable issue file, or a conflict at its path, keeps the earlier
//! first-parent replay instead. A merge with no merge base, or of more than two parents, is also
//! replayed as a commit of its first parent, and a file
//! [`known_issues::parts`] does not read is taken as the commit holds it, except split headings
//! of an existing issue that [`coalesce_retitles`] can bind to the base and both sides before
//! replaying the merge. Main's own reopening of
//! an entry needs no exception: the landing's merge of main's tip takes it from main's side.

use super::{Compose, Git, LOCAL, Seams, short};
pub(super) use aterm_census::known_issues::INDEX;
use aterm_census::known_issues::{self, HEAD, Merged, Side};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::Instant;

/// The paragraph head a commit whose open-issues file the rule changed carries ([`note`]).
pub(super) const NOTE_HEAD: &str = "Known issues merged by entry:";

/// A side's own parts, and the line its replay said of each part it held unlike them
/// ([`Replay::merged`]).
type Repaired<'t> = (BTreeMap<&'t str, &'t str>, &'t BTreeMap<String, String>);

/// One commit's open-issues file and the file the rule holds it to ([`fixed`]).
#[derive(Clone)]
struct Held {
    /// Its blob, `None` where the commit holds no such file.
    blob: Option<String>,
    /// Its text (empty where it holds no file), `None` where it is no UTF-8, which the rule
    /// does not read.
    own: Option<Rc<str>>,
    /// The text the rule holds it to, `None` where that is its own.
    fixed: Option<Rc<str>>,
    /// For each part `fixed` holds unlike `own`, the line the landing says of it.
    why: Rc<BTreeMap<String, String>>,
}

impl Held {
    /// The text the rule holds the commit to.
    fn text(&self) -> Option<&str> {
        self.fixed.as_deref().or(self.own.as_deref())
    }

    /// A commit's own file, held as it is.
    fn own(blob: Option<String>, own: Option<Rc<str>>) -> Self {
        Self {
            blob,
            own,
            fixed: None,
            why: Rc::default(),
        }
    }

    /// A present regular text file; its entries still need the merge's identity checks.
    fn virtual_file(&self) -> bool {
        self.blob.is_some()
            && self
                .own
                .as_deref()
                .is_some_and(|text| !text.as_bytes().contains(&0))
    }

    /// A virtual-base input whose replay is readable, including splits already repaired.
    fn virtual_text(&self) -> bool {
        self.virtual_file()
            && self.text().is_some_and(|text| {
                !text.as_bytes().contains(&0)
                    && known_issues::ordered(text).is_some()
                    && self
                        .own
                        .as_deref()
                        .is_some_and(|own| coalesce_repaired(own, own, text).is_some())
            })
    }
}

/// The open-issues file the rule holds `head` to, and a line for each part it holds unlike
/// `head`'s; `None` where that is `head`'s own file, or where `head` holds none. With
/// `pending`, a merge of `main` into `head` not yet made (the tree Git makes of it), the file
/// that merge is held to instead.
///
/// # Errors
/// A Git read failed.
pub(super) fn fixed(
    git: &Git<'_>,
    main: &str,
    head: &str,
    pending: Option<&str>,
) -> Result<Option<(String, Vec<String>)>, String> {
    let range = format!("{main}..{head}");
    let listed = git.local(&["rev-list", "--reverse", "--topo-order", "--parents", &range])?;
    let mut commits: Vec<(String, Vec<String>)> = listed
        .lines()
        .filter_map(|line| {
            let mut ids = line.split_whitespace().map(str::to_owned);
            Some((ids.next()?, ids.collect()))
        })
        .collect();
    if let Some(tree) = pending {
        commits.push((tree.to_owned(), vec![head.to_owned(), main.to_owned()]));
    }
    // A run of single-parent commits off main changes only what each commit changed.
    if !commits.iter().any(|(_, parents)| parents.len() > 1) {
        return Ok(None);
    }
    let mut replay = Replay {
        git,
        held: HashMap::new(),
        texts: HashMap::new(),
        bases: HashMap::new(),
    };
    let mut last = None;
    for (commit, parents) in commits {
        let held = replay.step(&commit, &parents)?;
        replay.held.insert(commit.clone(), held.clone());
        last = Some(held);
    }
    let Some(Held {
        blob: Some(_),
        fixed: Some(fixed),
        why,
        ..
    }) = last
    else {
        return Ok(None);
    };
    Ok(Some((fixed.to_string(), why.values().cloned().collect())))
}

/// The replay of the open-issues file through a run of commits ([`fixed`]).
struct Replay<'g, 'r> {
    git: &'g Git<'r>,
    /// Each commit replayed, and each other commit read, by its id.
    held: HashMap<String, Held>,
    /// Each blob read, by its id.
    texts: HashMap<String, Option<Rc<str>>>,
    /// Completed virtual bases, by Git's ordered base list; `None` where unsupported.
    bases: HashMap<Vec<String>, Option<String>>,
}

impl Replay<'_, '_> {
    /// Commit (or tree) `at`'s own file, read once.
    fn read(&mut self, at: &str) -> Result<Held, String> {
        if let Some(held) = self.held.get(at) {
            return Ok(held.clone());
        }
        let blob = self.git.issues_blob(at)?;
        let own = match &blob {
            None => Some(Rc::from("")),
            Some(blob) => match self.texts.get(blob) {
                Some(text) => text.clone(),
                None => {
                    let text = self.git.issues_text(blob)?.map(Rc::from);
                    self.texts.insert(blob.clone(), text.clone());
                    text
                }
            },
        };
        let held = Held::own(blob, own);
        self.held.insert(at.to_owned(), held.clone());
        Ok(held)
    }

    /// Replay `commit` (a commit, or a tree a merge not yet made would commit) of `parents`.
    fn step(&mut self, commit: &str, parents: &[String]) -> Result<Held, String> {
        let own = self.read(commit)?;
        let Some(first) = parents.first() else {
            return Ok(own);
        };
        let ours = self.read(first)?;
        if let [_, second] = parents {
            let theirs = self.read(second)?;
            let bases = self.git.merge_bases(first, second)?;
            let several = bases.len() > 1;
            let base = match &bases[..] {
                [] => None,
                [base] => Some(self.read(base)?),
                _ if own.virtual_file() && ours.virtual_text() && theirs.virtual_text() => {
                    self.virtual_base(bases)?
                }
                _ => None,
            };
            let Some(base) = base else {
                return Ok(self.single(own, &ours));
            };
            // Where a side's file is the base's, or both sides' are one, Git's merge of the file
            // is the other side's, and no merge need run.
            let union = if ours.blob == theirs.blob || theirs.blob == base.blob {
                ours.clone()
            } else if ours.blob == base.blob {
                theirs.clone()
            } else {
                let union = if several {
                    let (tree, conflict) = self.git.union_tree_of(first, second, false)?;
                    if conflict {
                        return Ok(self.single(own, &ours));
                    }
                    tree
                } else {
                    self.git.union_tree(first, second)?
                };
                self.read(&union)?
            };
            if several && !union.virtual_file() {
                return Ok(self.single(own, &ours));
            }
            let names = [first.as_str(), second.as_str()];
            if let Some(held) = self.merged(&own, &union, names, [&base, &ours, &theirs]) {
                return Ok(held);
            }
        }
        Ok(self.single(own, &ours))
    }

    /// Replay several best bases in the reverse of `merge-base --all`'s date order, as Git
    /// folds its virtual ancestors. Each virtual object holds the raw union tree and its two
    /// original parents; `held` alone keeps its repair. Recursive bases are older original
    /// ancestors, so a virtual object's fixed date cannot reorder a later base list.
    ///
    /// Git's internal merge differs from a top-level merge for file deletion, type and binary
    /// conflicts. Those issue-file shapes, unreadable parts and actual issue-path conflicts
    /// propagate `None` to the original step rather than treating a partial fold as its base.
    fn virtual_base(&mut self, mut bases: Vec<String>) -> Result<Option<Held>, String> {
        bases.reverse();
        if let Some(cached) = self.bases.get(&bases).cloned() {
            return cached.map(|at| self.read(&at)).transpose();
        }
        let folded = self.fold_bases(&bases)?;
        self.bases.insert(bases, folded.clone());
        folded.map(|at| self.read(&at)).transpose()
    }

    /// Complete one ordered fold, publishing no partial result as the requested base.
    fn fold_bases(&mut self, bases: &[String]) -> Result<Option<String>, String> {
        let Some((first, rest)) = bases.split_first() else {
            return Ok(None);
        };
        let mut at = first.clone();
        for next in rest {
            let (ours, theirs) = (self.read(&at)?, self.read(next)?);
            if !ours.virtual_text() || !theirs.virtual_text() {
                return Ok(None);
            }
            let older = self.git.merge_bases(&at, next)?;
            let base = match &older[..] {
                // Git's internal merge of unrelated older roots uses an empty tree. This
                // does not change the top-level no-base fallback in `step`.
                [] => Held::own(None, Some(Rc::from(""))),
                [base] => {
                    let base = self.read(base)?;
                    if !base.virtual_text() {
                        return Ok(None);
                    }
                    base
                }
                _ => {
                    let Some(base) = self.virtual_base(older)? else {
                        return Ok(None);
                    };
                    base
                }
            };
            let (tree, conflict) = self.git.union_tree_of(&at, next, true)?;
            if conflict {
                return Ok(None);
            }
            let union = self.read(&tree)?;
            if !union.virtual_file() {
                return Ok(None);
            }
            let names = [at.as_str(), next.as_str()];
            let Some(held) = self.merged(&union, &union, names, [&base, &ours, &theirs]) else {
                return Ok(None);
            };
            let commit = self.git.virtual_commit(&tree, &at, next)?;
            // Only a completed replay is associated with a virtual commit. Its tree stays
            // raw, so later edits are still accounted against what Git actually merged.
            self.held.insert(commit.clone(), held);
            at = commit;
        }
        Ok(Some(at))
    }

    /// Replay a merge of `names` (its two sides, the one merged into first) whose file is `own`,
    /// and whose sides' file Git's union merges to `union`, on the replays of its merge base and
    /// its two sides (`held`). The entry-level merge of the replays ([`known_issues::merge`])
    /// takes the place of `union`: each part as the side that changed it holds it, a part both
    /// sides changed as their three-way merge line by line ([`known_issues::merge_part`]; where
    /// one side deleted it, as the other changed it), and a part whose heading `union` lost put
    /// back after the part before it on the side that holds it ([`place`]). The merge is `union`
    /// and what a person changed of it, and that change is made on the entry-level merge
    /// ([`reapply`]). Split headings of an existing issue are gathered first
    /// ([`coalesce_retitles`]); `None` where a text still cannot be read as parts.
    fn merged(&self, own: &Held, union: &Held, names: [&str; 2], held: [&Held; 3]) -> Option<Held> {
        let (Some(text), Some(gits)) = (own.own.as_deref(), union.own.as_deref()) else {
            return None;
        };
        let [base, ours, theirs] = held.map(|held| held.text().and_then(known_issues::ordered));
        let (Some(base), Some(ours), Some(theirs)) = (base, ours, theirs) else {
            return None;
        };
        let [base_of, ours_of, theirs_of] =
            [&base, &ours, &theirs].map(|parts| parts.iter().copied().collect::<BTreeMap<_, _>>());
        let sources = [&base_of, &ours_of, &theirs_of];
        // A side may still carry split headings whose earlier replay proved one identity.
        // Keep that proof when the other side has since closed the issue.
        let repairs: Vec<_> = held
            .iter()
            .filter_map(|side| Some((side.own.as_deref()?, side.fixed.as_deref()?)))
            .collect();
        let gits = coalesce_retitles(gits, sources, &repairs)?;
        let edited = coalesce_retitles(text, sources, &repairs)?;
        let gits_order = known_issues::ordered(&gits)?;
        let gits_of = gits_order.iter().copied().collect::<BTreeMap<_, _>>();
        let decided = known_issues::merge(&base_of, &ours_of, &theirs_of);
        let both = |key: &str| match (ours_of.get(key), theirs_of.get(key)) {
            (Some(&mine), Some(&other)) => known_issues::merge_part(
                key,
                base_of.get(key).copied().unwrap_or_default(),
                mine,
                other,
            ),
            (Some(&one), None) | (None, Some(&one)) => one.to_owned(),
            (None, None) => String::new(),
        };
        // A part none of the three replays holds came from a side's own file, whose replay took
        // it out.
        let mut want: Vec<(&str, Cow<'_, str>)> = Vec::with_capacity(gits_order.len());
        for &(key, _) in &gits_order {
            match decided.get(key) {
                Some(Merged::Holds(now, _)) => want.push((key, Cow::Borrowed(*now))),
                Some(Merged::Open) => want.push((key, Cow::Owned(both(key)))),
                Some(Merged::Gone(_)) | None => {}
            }
        }
        let mut lost = BTreeMap::new();
        for (&key, &decision) in &decided {
            if gits_of.contains_key(key) {
                continue;
            }
            match decision {
                Merged::Holds(now, _) => lost.insert(key, Cow::Borrowed(now)),
                Merged::Open => lost.insert(key, Cow::Owned(both(key))),
                Merged::Gone(_) => continue,
            };
        }
        place(&mut want, lost, &[ours.as_slice(), theirs.as_slice()]);
        let want: Vec<(&str, &str)> = want
            .iter()
            .map(|(key, body)| (*key, body.as_ref()))
            .collect();
        let entry_level = known_issues::rewrite(&gits, &want)?;
        let now = reapply(&gits, &entry_level, &edited)?;
        // The line a side's replay said of a part, where the merge holds that part (or none) as
        // the side's own file does: the merge took it from that file, which the replay changed.
        let mine = known_issues::parts(text).unwrap_or_default();
        let sides: Vec<Repaired<'_>> = [held[1], held[2]]
            .into_iter()
            .filter(|side| !side.why.is_empty())
            .filter_map(|side| Some((known_issues::parts(side.own.as_deref()?)?, &*side.why)))
            .collect();
        let carried = |key: &str| {
            sides.iter().find_map(|(parts, why)| {
                (parts.get(key) == mine.get(key))
                    .then(|| why.get(key))
                    .flatten()
                    .cloned()
            })
        };
        let why = changed(text, &now, |key| {
            let decision = decided
                .get(key)
                .copied()
                .unwrap_or(Merged::Gone(Side::Both));
            carried(key).unwrap_or_else(|| said(key, names, decision, !gits_of.contains_key(key)))
        });
        Some(held_to(own, now, why))
    }

    /// Replay a commit whose file is `own` on its parent's replay `parent`: what the commit
    /// changed of its parent's file, made on that replay ([`reapply`]).
    fn single(&self, own: Held, parent: &Held) -> Held {
        if own.blob == parent.blob {
            return Held {
                fixed: parent.fixed.clone(),
                why: parent.why.clone(),
                ..own
            };
        }
        let Some(fixed) = parent.fixed.as_deref() else {
            return own;
        };
        let (Some(was), Some(text)) = (parent.own.as_deref(), own.own.as_deref()) else {
            return own;
        };
        let Some(now) = reapply(was, fixed, text) else {
            return own;
        };
        let why = changed(text, &now, |key| {
            parent.why.get(key).cloned().unwrap_or_else(|| {
                let part = if key == HEAD {
                    "the text above its first entry"
                } else {
                    key
                };
                format!("{INDEX}: {part} keeps the fix of an earlier merge on the branch")
            })
        });
        held_to(&own, now, why)
    }
}

/// Gather a union's repeated headings only for an issue present in the base and both sides,
/// or bound to one identity by an earlier repair. Each heading must come from those sources;
/// independent new issues sharing an identifier remain unreadable. Keep every body byte, in
/// occurrence order, at the last occurrence (the union can leave an earlier bare heading above
/// a new neighboring issue). The entry merge
/// decides the heading, so a later edit choosing a different single heading remains an edit.
pub(super) fn coalesce_retitles<'t>(
    text: &'t str,
    sources: [&BTreeMap<&str, &str>; 3],
    repairs: &[(&str, &str)],
) -> Option<Cow<'t, str>> {
    coalesce(text, |key, headings| {
        retitle(key, headings, sources).or_else(|| {
            repairs
                .iter()
                .find_map(|(was, fixed)| repaired_title(key, headings, was, fixed))
        })
    })
}

/// Choose a heading only where the base and both sides identify the same issue.
fn retitle(key: &str, headings: &[&str], sources: [&BTreeMap<&str, &str>; 3]) -> Option<String> {
    let [base, ours, theirs] = sources.map(|parts| parts.get(key).copied());
    let (Some(base), Some(ours), Some(theirs)) = (base, ours, theirs) else {
        return None;
    };
    let allowed = [base, ours, theirs].map(|body| body.lines().next());
    if headings
        .iter()
        .any(|heading| !allowed.contains(&Some(*heading)))
    {
        return None;
    }
    Some(known_issues::merge_part(
        key,
        allowed[0]?,
        allowed[1]?,
        allowed[2]?,
    ))
}

/// An earlier repair established the identity of these split headings. A later edit may keep
/// them and change their prose; it may not introduce another duplicated identifier or heading.
fn coalesce_repaired<'t>(text: &'t str, was: &str, repaired: &str) -> Option<Cow<'t, str>> {
    coalesce(text, |key, headings| {
        repaired_title(key, headings, was, repaired)
    })
}

/// Carry an established identity only for headings already present in the repaired source.
fn repaired_title(key: &str, headings: &[&str], was: &str, repaired: &str) -> Option<String> {
    let parts = known_issues::parts(repaired)?;
    let before = known_issues::entries(was);
    let allowed: Vec<_> = before
        .iter()
        .filter(|entry| entry.id == Some(key))
        .filter_map(|entry| entry.body.lines().next())
        .collect();
    if allowed.len() < 2 || headings.iter().any(|heading| !allowed.contains(heading)) {
        return None;
    }
    parts.get(key)?.lines().next().map(str::to_owned)
}

/// Gather repeated parts only where `title` establishes their identity and heading.
fn coalesce<'t>(
    text: &'t str,
    mut title: impl FnMut(&str, &[&str]) -> Option<String>,
) -> Option<Cow<'t, str>> {
    if known_issues::ordered(text).is_some() {
        return Some(Cow::Borrowed(text));
    }
    let entries = known_issues::entries(text);
    let first = entries.first()?.span.start;
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut at = first;
    for (i, entry) in entries.iter().enumerate() {
        if entry.span.start != at {
            return None;
        }
        at = entry.span.end;
        groups.entry(entry.id?).or_default().push(i);
    }
    if at != text.len() {
        return None;
    }
    let mut gathered = BTreeMap::new();
    for (&key, positions) in &groups {
        if positions.len() == 1 {
            continue;
        }
        let headings: Vec<_> = positions
            .iter()
            .map(|&i| entries[i].body.lines().next())
            .collect::<Option<_>>()?;
        let mut body = title(key, &headings)?;
        body.push_str(line_end(text));
        for &i in positions {
            let entry = &entries[i];
            if let Some((_, rest)) = text[entry.span.clone()].split_once('\n') {
                body.push_str(rest);
            }
        }
        gathered.insert(key, body);
    }
    let mut out = text[..first].to_owned();
    for (i, entry) in entries.iter().enumerate() {
        let key = entry.id?;
        if groups.get(key)?.last() != Some(&i) {
            continue;
        }
        out.push_str(
            gathered
                .get(key)
                .map_or(&text[entry.span.clone()], String::as_str),
        );
    }
    known_issues::ordered(&out)?;
    Some(Cow::Owned(out))
}

/// `edited`, an edit of the text `was`, made instead on `repaired`, the rule's repair of `was`
/// ([`Edit`]); `None` where a text remains unreadable after gathering proven split headings.
/// Each part is as
/// the entry-level merge of the three decides it ([`known_issues::merge`]); a part the edit and
/// the repair both changed is their three-way merge line by line ([`known_issues::merge_part`]),
/// with the edit's change of a line the repair moved to another part made there. Where that
/// result fails the line count ([`accounts`]), each part is instead the edit's where the edit
/// changed it and the repair's where it did not, which the count always passes: no step of the
/// rule loses a line Git's merge or a person kept, or makes one up, beyond what the repair moved,
/// took out or put in. Split headings already bound to one entry by the repair are gathered
/// before replay; newly introduced duplicate identifiers or headings remain unreadable.
pub(super) fn reapply(was: &str, repaired: &str, edited: &str) -> Option<String> {
    let normalized = coalesce_repaired(was, was, repaired)?;
    let edited = coalesce_repaired(edited, was, repaired)?;
    let was = normalized.as_ref();
    let edited = edited.as_ref();
    let edit = Edit::of(was, repaired, edited)?;
    let now = edit.made(true)?;
    if accounts(was, repaired, edited, &now) {
        Some(now)
    } else {
        edit.made(false)
    }
}

/// How many times a text holds each line ([`text_lines`]).
type Counts<'t> = HashMap<&'t str, usize>;

/// The lines of part `key`'s text `body` that hold more than white space, each with its index
/// among the part's lines (`str::lines`) and no white space at its end: an entry's heading
/// aside, which the parser holds to one part each and the entry-level decision keeps or drops.
fn text_lines<'t>(key: &str, body: &'t str) -> impl Iterator<Item = (usize, &'t str)> {
    body.lines()
        .enumerate()
        .skip(usize::from(key != HEAD))
        .map(|(at, line)| (at, line.trim_end()))
        .filter(|(_, line)| !line.is_empty())
}

/// How many times part `key`'s text `body` holds each of its [`text_lines`].
fn counts<'t>(key: &str, body: &'t str) -> Counts<'t> {
    let mut counts = HashMap::new();
    for (_, line) in text_lines(key, body) {
        *counts.entry(line).or_insert(0) += 1;
    }
    counts
}

/// Each part's [`counts`], by its key.
fn counts_of<'t>(order: &[(&'t str, &'t str)]) -> BTreeMap<&'t str, Counts<'t>> {
    order
        .iter()
        .map(|&(key, body)| (key, counts(key, body)))
        .collect()
}

/// How many times a text holds each line, in all its parts.
fn total<'t>(parts: &BTreeMap<&'t str, Counts<'t>>) -> Counts<'t> {
    let mut total = HashMap::new();
    for counts in parts.values() {
        for (&line, &count) in counts {
            *total.entry(line).or_insert(0) += count;
        }
    }
    total
}

/// How many times `counts` holds `line`.
fn count(counts: &Counts<'_>, line: &str) -> usize {
    counts.get(line).copied().unwrap_or(0)
}

/// The line end of `text`: CRLF where it holds one.
fn line_end(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// What the edit changed of a line the repair moved to another part, made there ([`Edit`]).
struct Carry<'t> {
    /// The part the repair moved the line to.
    to: &'t str,
    /// The line, with no white space at its end.
    line: &'t str,
    /// The edit's lines that take its place there, or with `after` that follow it.
    lines: Vec<&'t str>,
    after: bool,
}

/// An edit of the text `was` and the rule's repair of it, read for [`reapply`].
///
/// The repair moves a line where it takes it out of one part and puts it in another (a line the
/// union filed under another entry's heading, put back in its own): each line it took out of a
/// part, in `was`' order, is paired with a part it put that line in, in the repair's order. Of
/// a part the edit and the repair both changed, an edit's hunk (the lines it took out and wrote
/// at one place, [`known_issues::matched`]) that took out a line the repair moved is made where
/// the repair moved it, its lines in that line's place; and where the repair took the part out,
/// the lines the edit wrote right after a moved line (blank lines aside) follow it there. So a
/// line stays in the entry it was written in, and where the rule closed that entry, it goes with
/// the text it was written after.
struct Edit<'t> {
    edited: &'t str,
    repaired_order: Vec<(&'t str, &'t str)>,
    edited_order: Vec<(&'t str, &'t str)>,
    was_of: BTreeMap<&'t str, &'t str>,
    repaired_of: BTreeMap<&'t str, &'t str>,
    edited_of: BTreeMap<&'t str, &'t str>,
    decided: BTreeMap<&'t str, Merged<'t>>,
    /// Each line's count in all of `was`' parts, and in all of the edit's.
    was_lines: Counts<'t>,
    edited_lines: Counts<'t>,
    carries: Vec<Carry<'t>>,
    /// The edit's text of a part a carry left, without the lines it carried.
    edits: HashMap<&'t str, String>,
    /// The text before the edit of a part the repair took out, without the lines it moved.
    rests: HashMap<&'t str, String>,
}

impl<'t> Edit<'t> {
    /// Read the three texts; `None` where one is a text [`known_issues::parts`] does not read.
    fn of(was: &'t str, repaired: &'t str, edited: &'t str) -> Option<Self> {
        let was_order = known_issues::ordered(was)?;
        let repaired_order = known_issues::ordered(repaired)?;
        let edited_order = known_issues::ordered(edited)?;
        let [was_of, repaired_of, edited_of] = [&was_order, &repaired_order, &edited_order]
            .map(|parts| parts.iter().copied().collect::<BTreeMap<_, _>>());
        let decided = known_issues::merge(&was_of, &repaired_of, &edited_of);
        let [was_counts, repaired_counts, edited_counts] =
            [&was_order, &repaired_order, &edited_order].map(|order| counts_of(order));
        let moves = moves(&was_order, &repaired_order, &was_counts, &repaired_counts);
        let mut edit = Self {
            edited,
            repaired_order,
            edited_order,
            was_of,
            repaired_of,
            edited_of,
            decided,
            was_lines: total(&was_counts),
            edited_lines: total(&edited_counts),
            carries: Vec::new(),
            edits: HashMap::new(),
            rests: HashMap::new(),
        };
        for index in 0..edit.edited_order.len() {
            let (key, body) = edit.edited_order[index];
            if edit.decided.get(key) == Some(&Merged::Open)
                && let Some(&before) = edit.was_of.get(key)
            {
                edit.carry(key, before, body, &moves);
            }
        }
        Some(edit)
    }

    /// Carry the edit's hunks of part `key` (its text `before` the edit and `body` after) that
    /// change a line the repair moved ([`Edit`]).
    fn carry(
        &mut self,
        key: &'t str,
        before: &'t str,
        body: &'t str,
        moves: &HashMap<(&'t str, &'t str), Vec<&'t str>>,
    ) {
        let mut left: HashMap<&str, std::collections::VecDeque<&'t str>> = moves
            .iter()
            .filter(|((from, _), _)| *from == key)
            .map(|(&(_, line), to)| (line, to.iter().copied().collect()))
            .collect();
        let mut moved = BTreeMap::new();
        for (at, line) in text_lines(key, before) {
            if let Some(to) = left.get_mut(line).and_then(|to| to.pop_front()) {
                moved.insert(at, to);
            }
        }
        if moved.is_empty() {
            return;
        }
        let was: Vec<&'t str> = before.lines().collect();
        let now: Vec<&'t str> = body.lines().collect();
        let first = usize::from(key != HEAD);
        let closed = !self.repaired_of.contains_key(key);
        let mut keep = vec![true; now.len()];
        let (mut from, mut from_now) = (0, 0);
        let stops = known_issues::matched(&was, &now);
        for (to, to_now) in stops
            .into_iter()
            .map(Some)
            .chain([None])
            .map(|stop| stop.unwrap_or((was.len(), now.len())))
        {
            let wrote = &now[from_now..to_now];
            let took: Vec<usize> = (from..to).filter(|at| moved.contains_key(at)).collect();
            if let Some((&one, others)) = took.split_first() {
                self.carries.push(Carry {
                    to: moved[&one],
                    line: was[one].trim_end(),
                    lines: wrote.to_vec(),
                    after: false,
                });
                for &other in others {
                    self.carries.push(Carry {
                        to: moved[&other],
                        line: was[other].trim_end(),
                        lines: Vec::new(),
                        after: false,
                    });
                }
                keep[from_now..to_now].fill(false);
            } else if closed
                && !wrote.is_empty()
                && let Some(after) = (first..from).rev().find(|&at| !was[at].trim().is_empty())
                && let Some(&to) = moved.get(&after)
            {
                self.carries.push(Carry {
                    to,
                    line: was[after].trim_end(),
                    lines: wrote.to_vec(),
                    after: true,
                });
                keep[from_now..to_now].fill(false);
            }
            (from, from_now) = (to + 1, to_now + 1);
        }
        let kept: Vec<&str> = now
            .iter()
            .zip(&keep)
            .filter_map(|(&line, &keep)| keep.then_some(line))
            .collect();
        self.edits
            .insert(key, kept.join(line_end(body)).trim_end().to_owned());
        if closed {
            let rest: Vec<&str> = was
                .iter()
                .enumerate()
                .filter_map(|(at, &line)| (!moved.contains_key(&at)).then_some(line))
                .collect();
            self.rests
                .insert(key, rest.join(line_end(before)).trim_end().to_owned());
        }
    }

    /// Whether part `key`'s text `body`, which the repair holds and the edit does not, stands
    /// beside the parts `others` (each line's count in them): the text before the edit held a
    /// line of it, and the edit still holds each such line as many times (counting one it
    /// changed where the repair moved it to this part), more times than `others` do. A part
    /// whose every line the edit took out, whose text the union never held (only its heading),
    /// or whose lines the edit put in another part that stands, is out.
    fn stands(&self, key: &str, body: &str, others: &Counts<'_>) -> bool {
        let held: std::collections::BTreeSet<&str> = text_lines(key, body)
            .map(|(_, line)| line)
            .filter(|line| count(&self.was_lines, line) > 0)
            .collect();
        !held.is_empty()
            && held.iter().all(|line| {
                let replaced = self
                    .carries
                    .iter()
                    .filter(|carry| !carry.after && carry.to == key && carry.line == *line)
                    .count();
                let edit = count(&self.edited_lines, line) + replaced;
                edit >= count(&self.was_lines, line) && count(others, line) < edit
            })
    }

    /// Whether `now`, the merge of a part the repair took out, holds what the edit wrote into it:
    /// a heading other than `rest`'s, or a line more times than `rest` that the text before the
    /// edit held fewer times than the edit does (a line it only moved, or took out, is not one).
    fn reopens(&self, key: &str, now: &str, rest: &str) -> bool {
        if key != HEAD && now.lines().next() != rest.lines().next() {
            return true;
        }
        let rest = counts(key, rest);
        counts(key, now).into_iter().any(|(line, held)| {
            held > count(&rest, line)
                && count(&self.edited_lines, line) > count(&self.was_lines, line)
        })
    }

    /// The edit made on the repair: with `merged`, each part both changed merged line by line
    /// and each carry made; else each part the edit's where it changed it and the repair's where
    /// it did not. `None` where the result is a text the parser does not read.
    fn made(&self, merged: bool) -> Option<String> {
        let mut want: Vec<(&str, Cow<'_, str>)> = Vec::with_capacity(self.edited_order.len());
        for &(key, body) in &self.edited_order {
            let part = match self.decided.get(key) {
                Some(&Merged::Holds(now, _)) => Cow::Borrowed(now),
                Some(Merged::Gone(_)) => continue,
                _ if !merged => Cow::Borrowed(body),
                _ => {
                    let edit = self.edits.get(key).map_or(body, String::as_str);
                    match (self.was_of.get(key), self.repaired_of.get(key)) {
                        (None, fixed) => Cow::Owned(known_issues::merge_part(
                            key,
                            "",
                            fixed.copied().unwrap_or_default(),
                            body,
                        )),
                        (Some(before), Some(fixed)) => {
                            Cow::Owned(known_issues::merge_part(key, before, fixed, edit))
                        }
                        (Some(before), None) => {
                            let rest = self.rests.get(key).map_or(*before, String::as_str);
                            let now = known_issues::merge_part(key, before, rest, edit);
                            if !self.reopens(key, &now, rest) {
                                continue;
                            }
                            Cow::Owned(now)
                        }
                    }
                }
            };
            want.push((key, part));
        }
        let mut others: Counts<'_> = HashMap::new();
        for (key, body) in &want {
            for (line, held) in counts(key, body) {
                *others.entry(line).or_insert(0) += held;
            }
        }
        let mut kept = BTreeMap::new();
        for &(key, body) in &self.repaired_order {
            if self.edited_of.contains_key(key) {
                continue;
            }
            let stays = match self.decided.get(key) {
                // The repair put it in (a part whose heading Git's union lost): it stays unless
                // the edit took its lines out.
                Some(Merged::Holds(_, Side::Ours)) => {
                    !merged
                        || text_lines(key, body).all(|(_, line)| count(&self.was_lines, line) == 0)
                        || self.stands(key, body, &others)
                }
                // The repair changed it and the edit took it out, or the edit took out a part
                // the repair left: it stays where the edit took out no more than its heading.
                Some(Merged::Open | Merged::Gone(Side::Theirs)) => {
                    merged && self.stands(key, body, &others)
                }
                _ => false,
            };
            if stays {
                kept.insert(key, Cow::Borrowed(body));
            }
        }
        place(&mut want, kept, &[self.repaired_order.as_slice()]);
        if merged {
            for carry in &self.carries {
                let Some((_, part)) = want.iter_mut().find(|(key, _)| *key == carry.to) else {
                    continue;
                };
                let mut lines: Vec<&str> = part.lines().collect();
                let Some(at) = lines.iter().position(|line| line.trim_end() == carry.line) else {
                    continue;
                };
                let end = line_end(part);
                let rest = lines.split_off(at + 1);
                if !carry.after {
                    lines.pop();
                }
                lines.extend(&carry.lines);
                lines.extend(rest);
                *part = Cow::Owned(lines.join(end).trim_end().to_owned());
            }
        }
        let want: Vec<(&str, &str)> = want
            .iter()
            .map(|(key, body)| (*key, body.as_ref()))
            .collect();
        known_issues::rewrite(self.edited, &want)
    }
}

/// The lines the repair moved ([`Edit`]): by the part of `was` a line left and the line, the
/// part of `repaired` it went to, once for each time it went. `counts` are each part's
/// ([`counts_of`]).
fn moves<'t>(
    was: &[(&'t str, &'t str)],
    repaired: &[(&'t str, &'t str)],
    was_counts: &BTreeMap<&'t str, Counts<'t>>,
    repaired_counts: &BTreeMap<&'t str, Counts<'t>>,
) -> HashMap<(&'t str, &'t str), Vec<&'t str>> {
    let empty = HashMap::new();
    // Where each line went: each part that holds it more times than `was` does, in order.
    let mut put: HashMap<&str, std::collections::VecDeque<(&'t str, usize)>> = HashMap::new();
    for &(key, _) in repaired {
        let before = was_counts.get(key).unwrap_or(&empty);
        for (&line, &held) in &repaired_counts[key] {
            if held > count(before, line) {
                put.entry(line)
                    .or_default()
                    .push_back((key, held - count(before, line)));
            }
        }
    }
    let mut moves: HashMap<(&'t str, &'t str), Vec<&'t str>> = HashMap::new();
    for &(key, body) in was {
        let after = repaired_counts.get(key).unwrap_or(&empty);
        let mut seen = std::collections::HashSet::new();
        for (_, line) in text_lines(key, body) {
            if !seen.insert(line) {
                continue;
            }
            let took = count(&was_counts[key], line).saturating_sub(count(after, line));
            let Some(to) = put.get_mut(line) else {
                continue;
            };
            for _ in 0..took {
                let Some((part, left)) = to.front_mut() else {
                    break;
                };
                moves.entry((key, line)).or_default().push(*part);
                *left -= 1;
                if *left == 0 {
                    to.pop_front();
                }
            }
        }
    }
    moves
}

/// Whether `result` holds each line ([`text_lines`], in all its parts) as many times as `edited`
/// does, less at most the times the repair of `was` took it out of a part (summed over the
/// parts that `repaired` holds it fewer times than `was`), and more by at most the times it put
/// it in one: the result answers for every line the edit holds by a change of the repair's, and
/// a line the repair moved from one part to another may be held where either holds it. False
/// where a text is one the parser does not read.
pub(super) fn accounts(was: &str, repaired: &str, edited: &str, result: &str) -> bool {
    let [Some(was), Some(repaired), Some(edited), Some(result)] =
        [was, repaired, edited, result].map(known_issues::ordered)
    else {
        return false;
    };
    let [was, repaired, edited, result] =
        [&was, &repaired, &edited, &result].map(|order| counts_of(order));
    let (edit, now) = (total(&edited), total(&result));
    let (mut took, mut put): (Counts<'_>, Counts<'_>) = (HashMap::new(), HashMap::new());
    let empty = HashMap::new();
    let keys: std::collections::BTreeSet<&str> =
        was.keys().chain(repaired.keys()).copied().collect();
    for key in keys {
        let (before, after) = (
            was.get(key).unwrap_or(&empty),
            repaired.get(key).unwrap_or(&empty),
        );
        for (&line, &held) in before {
            *took.entry(line).or_insert(0) += held.saturating_sub(count(after, line));
        }
        for (&line, &held) in after {
            *put.entry(line).or_insert(0) += held.saturating_sub(count(before, line));
        }
    }
    let lines: std::collections::BTreeSet<&str> = [&edit, &now, &took, &put]
        .into_iter()
        .flat_map(|counts| counts.keys().copied())
        .collect();
    lines.into_iter().all(|line| {
        let edit = count(&edit, line);
        let low = edit.saturating_sub(count(&took, line));
        let high = edit + count(&put, line);
        (low..=high).contains(&count(&now, line))
    })
}

/// The line `why` gives of each part `now` holds unlike `text` (changed, put in or taken out),
/// by its key.
fn changed(text: &str, now: &str, mut why: impl FnMut(&str) -> String) -> BTreeMap<String, String> {
    let (before, after) = (
        known_issues::parts(text).unwrap_or_default(),
        known_issues::parts(now).unwrap_or_default(),
    );
    let keys: std::collections::BTreeSet<&str> =
        before.keys().chain(after.keys()).copied().collect();
    keys.into_iter()
        .filter(|key| before.get(key) != after.get(key))
        .map(|key| (key.to_owned(), why(key)))
        .collect()
}

/// Put each part of `parts` into `want` (parts in order, by key) after the part before it in the
/// first of `orders` (texts' parts in order) that holds it, each part put in counted as one
/// `want` holds; one that has no part before it there goes first, and one none of `orders`
/// holds goes last.
fn place<'t>(
    want: &mut Vec<(&'t str, Cow<'t, str>)>,
    mut parts: BTreeMap<&'t str, Cow<'t, str>>,
    orders: &[&[(&'t str, &'t str)]],
) {
    for order in orders {
        let mut after = None;
        for &(key, _) in order.iter() {
            if parts.is_empty() {
                return;
            }
            if let Some(body) = parts.remove(key) {
                let at = after
                    .and_then(|before| want.iter().position(|&(held, _)| held == before))
                    .map_or(0, |at| at + 1);
                want.insert(at, (key, body));
                after = Some(key);
            } else if want.iter().any(|&(held, _)| held == key) {
                after = Some(key);
            }
        }
    }
    want.extend(parts);
}

/// The line the landing says of part `key`, which the merge of `names` held unlike `decided`:
/// held otherwise, or, where `lost`, under no heading of its own.
fn said(key: &str, names: [&str; 2], decided: Merged<'_>, lost: bool) -> String {
    let part = if key == HEAD {
        "the text above its first entry".to_owned()
    } else {
        key.to_owned()
    };
    let merge = format!("the merge of {} into {}", short(names[1]), short(names[0]));
    let held = if lost {
        format!("{merge} lost its heading")
    } else {
        format!("{merge} held it as neither side does")
    };
    let one = |side: Side| match side {
        Side::Ours => Some(short(names[0])),
        Side::Theirs => Some(short(names[1])),
        Side::Both => None,
    };
    match decided {
        Merged::Gone(side) => match one(side) {
            Some(one) => {
                format!("{INDEX}: {part} is dropped: {one} deleted it, and {merge} kept it")
            }
            None => {
                format!("{INDEX}: {part} is dropped: both sides deleted it, and {merge} kept it")
            }
        },
        Merged::Holds(_, side) => match one(side) {
            Some(one) => format!("{INDEX}: {part} is as {one} holds it: {held}"),
            None => format!("{INDEX}: {part} is as both sides hold it: {held}"),
        },
        Merged::Open => format!(
            "{INDEX}: {part} merges both sides' changes line by line: both changed it, and {held}"
        ),
    }
}

/// `own` held to the text `now`, with `why` said of each part it changed; `own` where `now` is
/// its own text.
fn held_to(own: &Held, now: String, why: BTreeMap<String, String>) -> Held {
    if own.own.as_deref() == Some(now.as_str()) {
        return own.clone();
    }
    Held {
        fixed: Some(Rc::from(now)),
        why: Rc::new(why),
        ..own.clone()
    }
}

/// Hold the checkout's open-issues file to the file the rule holds HEAD to over main's tip
/// `main` ([`fixed`]), and say each part it changed: the lines said, for the commit's [`note`];
/// none where it changed nothing, and the checkout as it was.
///
/// # Errors
/// A Git read failed, or the file could not be written.
pub(super) fn merge_in_checkout(
    git: &Git<'_>,
    seams: &mut Seams<'_>,
    main: &str,
) -> Result<Vec<String>, String> {
    let head = git.head()?;
    let Some((text, said)) = fixed(git, main, &head, None)? else {
        return Ok(Vec::new());
    };
    git.writable(INDEX)?;
    std::fs::write(git.root.join(INDEX), text)
        .map_err(|error| format!("the landing's write of {INDEX} failed: {error}"))?;
    for line in &said {
        (seams.say)(&format!("verify land: {line}"));
    }
    Ok(said)
}

/// The paragraph a commit whose open-issues file the rule changed carries after its message:
/// [`NOTE_HEAD`], then each line [`merge_in_checkout`] said, as `- ` and the line.
pub(super) fn note(said: &[String]) -> String {
    let mut note = NOTE_HEAD.to_owned();
    for line in said {
        note.push_str("\n- ");
        note.push_str(line);
    }
    note
}

impl Git<'_> {
    /// The open-issues file's blob in `at` (a commit or a tree), `None` where it holds none, or
    /// holds something other than a regular file there (a link, a directory), which no rule here
    /// reads.
    fn issues_blob(&self, at: &str) -> Result<Option<String>, String> {
        let args = ["--literal-pathspecs", "ls-tree", "-z", at, "--", INDEX];
        let out = self.complete(&args, LOCAL)?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let Some(record) = out.stdout.split(|byte| *byte == 0).find(|r| !r.is_empty()) else {
            return Ok(None);
        };
        let head = record
            .split(|byte| *byte == b'\t')
            .next()
            .unwrap_or_default();
        let head = String::from_utf8_lossy(head);
        Ok(match head.split(' ').collect::<Vec<_>>()[..] {
            ["100644" | "100755", "blob", blob] => Some(blob.to_owned()),
            _ => None,
        })
    }

    /// The tree of Git's own merge of `other` into `one` (`one`'s lines first), whether or not it
    /// conflicts: what a union merge makes of the open-issues file, which never conflicts.
    fn union_tree(&self, one: &str, other: &str) -> Result<String, String> {
        self.union_tree_of(one, other, false).map(|(tree, _)| tree)
    }

    /// [`Self::union_tree`], and whether Git reports a conflict at the issues path. Only an
    /// internal virtual-base fold allows unrelated roots, as Git's own recursive merge does.
    fn union_tree_of(
        &self,
        one: &str,
        other: &str,
        unrelated: bool,
    ) -> Result<(String, bool), String> {
        let mut args = vec![
            "merge-tree",
            "--write-tree",
            "--no-messages",
            "--name-only",
            "-z",
        ];
        if unrelated {
            args.push("--allow-unrelated-histories");
        }
        args.extend([one, other]);
        let out = self.complete(&args, LOCAL)?;
        let mut records = out.stdout.split(|byte| *byte == 0);
        let tree = String::from_utf8_lossy(records.next().unwrap_or_default()).into_owned();
        match out.status.code() {
            Some(0 | 1) if aterm_land_trailer::object_id(&tree) => {
                Ok((tree, records.any(|path| path == INDEX.as_bytes())))
            }
            _ => Err(format!(
                "git merge-tree {one} {other}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    /// An unreferenced virtual commit: raw merge tree and original parents, with fixed
    /// metadata. The normal progress-aware commit writer changes no ref, index or worktree;
    /// `commit-tree` is invoked without a signing request.
    fn virtual_commit(&self, tree: &str, one: &str, other: &str) -> Result<String, String> {
        if ![tree, one, other]
            .into_iter()
            .all(aterm_land_trailer::object_id)
        {
            return Err("a virtual merge has an invalid tree or parent object id".into());
        }
        let make = |root: &std::path::Path| {
            let mut cmd = (self.make)(root);
            cmd.env("GIT_AUTHOR_NAME", "verify land")
                .env("GIT_AUTHOR_EMAIL", "verify-land@aterm.invalid")
                .env("GIT_AUTHOR_DATE", "@0 +0000")
                .env("GIT_COMMITTER_NAME", "verify land")
                .env("GIT_COMMITTER_EMAIL", "verify-land@aterm.invalid")
                .env("GIT_COMMITTER_DATE", "@0 +0000")
                .args(["-c", "i18n.commitEncoding=UTF-8"]);
            cmd
        };
        let git = Git {
            root: self.root,
            make: &make,
        };
        let id = git.commit(
            tree,
            &[one, other],
            "verify land: virtual open-issues merge base",
        )?;
        if !aterm_land_trailer::object_id(&id) {
            return Err("git commit-tree returned no virtual commit object id".into());
        }
        Ok(id)
    }

    /// The merge bases of `one` and `other`: none where their histories share no commit.
    fn merge_bases(&self, one: &str, other: &str) -> Result<Vec<String>, String> {
        let out = self.complete(&["merge-base", "--all", one, other], LOCAL)?;
        match out.status.code() {
            Some(0) => Ok(String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .map(str::to_owned)
                .collect()),
            Some(1) if out.stdout.is_empty() => Ok(Vec::new()),
            _ => Err(format!(
                "git merge-base --all {one} {other}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    /// Blob `blob`'s text, `None` where it is no UTF-8, which no rule here reads.
    fn issues_text(&self, blob: &str) -> Result<Option<String>, String> {
        Ok(String::from_utf8(self.blob(blob)?).ok())
    }

    /// What Git's clean merge of main's tip `main` into `checked` is to the landing: Git's tree
    /// ([`Git::merge_tree`]) with the open-issues file the rule holds that merge to ([`fixed`]),
    /// as the landing's own merge holds it ([`merge_in_checkout`]); Git's conflict where it has
    /// one.
    ///
    /// # Errors
    /// A Git read or write failed.
    pub(super) fn landing_tree(&self, main: &str, checked: &str) -> Result<Compose, String> {
        Ok(match self.merge_tree(main, checked)? {
            Compose::Tree(tree) => Compose::Tree(self.landing_tree_of(main, checked, &tree)?),
            conflict @ Compose::Conflict(_) => conflict,
        })
    }

    /// [`Self::landing_tree`] of `tree`, Git's clean merge of `main` into `checked`.
    ///
    /// # Errors
    /// A Git read or write failed.
    pub(super) fn landing_tree_of(
        &self,
        main: &str,
        checked: &str,
        tree: &str,
    ) -> Result<String, String> {
        let Some((text, _)) = fixed(self, main, checked, Some(tree))? else {
            return Ok(tree.to_owned());
        };
        let blob = self.with_input(
            &["hash-object", "-w", "--no-filters", "--stdin"],
            text.as_bytes(),
        )?;
        let path: Vec<&str> = INDEX.split('/').collect();
        self.put_in_tree(tree, &path, &blob)
    }

    /// Tree `tree` with the regular file at `path` (its names, in order) holding blob `blob`:
    /// the new tree, each tree off that path reused by its id.
    fn put_in_tree(&self, tree: &str, path: &[&str], blob: &str) -> Result<String, String> {
        let [name, rest @ ..] = path else {
            return Err("no path to put the open-issues file at".into());
        };
        let listed = self.complete(&["ls-tree", "-z", tree], LOCAL)?;
        if !listed.status.success() {
            return Err(format!(
                "git ls-tree {tree}: {}",
                String::from_utf8_lossy(&listed.stderr).trim()
            ));
        }
        let mut entries = Vec::new();
        let mut found = false;
        for record in listed.stdout.split(|byte| *byte == 0) {
            if record.is_empty() {
                continue;
            }
            let at = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| format!("git ls-tree {tree}: an entry has no name"))?;
            if &record[at + 1..] != name.as_bytes() {
                entries.extend_from_slice(record);
                entries.push(0);
                continue;
            }
            let head = String::from_utf8_lossy(&record[..at]).into_owned();
            let [mode, kind, id] = head.split(' ').collect::<Vec<_>>()[..] else {
                return Err(format!("git ls-tree {tree}: {head:?} is no entry"));
            };
            let id = match (rest, kind) {
                ([], "blob") if mode == "100644" || mode == "100755" => blob.to_owned(),
                ([_, ..], "tree") => self.put_in_tree(id, rest, blob)?,
                _ => {
                    return Err(format!(
                        "{INDEX}: {name} in {tree} is a {kind} of mode {mode}"
                    ));
                }
            };
            entries.extend_from_slice(format!("{mode} {kind} {id}\t{name}").as_bytes());
            entries.push(0);
            found = true;
        }
        if !found {
            return Err(format!("{INDEX}: tree {tree} holds no {name}"));
        }
        self.with_input(&["mktree", "-z"], &entries)
    }
}

impl Git<'_> {
    /// A local Git command fed `input`: its output, trimmed.
    fn with_input(&self, args: &[&str], input: &[u8]) -> Result<String, String> {
        let mut cmd = (self.make)(self.root);
        cmd.args(args);
        let out = aterm_uds::deadline::output_with_input_until(
            &mut cmd,
            aterm_uds::secret::Secret::new(input),
            Instant::now() + LOCAL,
            aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        )
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}
