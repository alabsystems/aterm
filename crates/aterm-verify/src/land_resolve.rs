// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mechanical rules `verify land` resolves a merge conflict by. Where git's merge of main
//! conflicts, [`plan`] asks each conflicted path's rule for its resolution and writes nothing;
//! only where every path has a rule that resolves it does [`apply`] write them, and the merge
//! is committed and judged in full as any other ([`super::Git::merge`]). A path no rule covers,
//! or one its rule refuses, leaves the whole conflict as git left it, for the person
//! ([`crate::exit::CONFLICT`]).
//!
//! - `changelog-to-changes`: `CHANGELOG.md` takes main's file, and each entry the branch added
//!   under `[Unreleased]` becomes its own `changes/<kind>/` file ([`changelog`]).
//! - `tracker-docs-take-main`: `docs/PLAN.md` and `docs/STATUS.md`, which `goals.py
//!   repo-docs` regenerates from the lab tracker, take main's side.
//! - `spec-table-rows`: a `SPECS.md` takes each table row from the side that changed it
//!   against the merge base ([`spec_table`]); a row both sides changed differently has no rule.
//!
//! A rule refuses whatever it cannot carry over whole, so a resolution drops none of the
//! branch's work silently; the judgment then reads every file a rule wrote.

use super::{Git, display_git_path};
use crate::changelog_fragments::{self, Fragment, Section};
use aterm_census::changelog::{CHANGELOG_FILE, HARNESS_ENTRY_HEAD, section_span};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::PathBuf;

/// The mode of a regular, non-executable file: the one every rule reads and writes.
const REGULAR: &str = "100644";

/// The line that opens the paragraph of rule lines a resolved merge's message carries
/// ([`Resolution::note`]), which the landing message carries on ([`super::Git::resolutions`]).
pub(super) const NOTE_HEAD: &str = "Resolved by rule:";

/// One index stage of an unmerged path: its mode and object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stage {
    pub(super) mode: String,
    pub(super) oid: String,
}

/// An unmerged path's stages: the merge base, ours (the branch) and theirs (main), git's stages
/// 1, 2 and 3. A side that deleted the path, or never had it, has none.
pub(super) type Stages = [Option<Stage>; 3];

/// The sides of [`Stages`], in its order, as a refusal names them.
const SIDES: [&str; 3] = ["the merge base", "the branch", "main"];

/// How every conflicted path of one merge is resolved ([`plan`]).
pub(super) struct Resolution {
    /// Each file to write, in order: its repository path and its bytes.
    writes: Vec<(String, Vec<u8>)>,
    /// What each rule did, one line each: `rule: "path" (detail)`.
    pub(super) said: Vec<String>,
}

impl Resolution {
    /// The paragraph the resolved merge's message carries after its subject: [`NOTE_HEAD`],
    /// then each line of [`Self::said`] as `- ` and the line.
    pub(super) fn note(&self) -> String {
        let mut note = NOTE_HEAD.to_owned();
        for line in &self.said {
            note.push_str("\n- ");
            note.push_str(line);
        }
        note
    }
}

/// What a rule resolves one path into: the files to write (repository path and bytes), and
/// what it did, in a few words on one line.
type Resolved = (Vec<(String, Vec<u8>)>, String);

/// One mechanical rule: its name, the paths it covers, and its resolution of one such path
/// from that path's stages, or why it refuses.
struct Rule {
    name: &'static str,
    covers: fn(&str) -> bool,
    resolve: fn(&Git<'_>, &str, &Stages) -> Result<Resolved, String>,
}

/// The rule table, in the order a path is matched against it.
static RULES: [Rule; 3] = [
    Rule {
        name: "changelog-to-changes",
        covers: is_changelog,
        resolve: changelog_rule,
    },
    Rule {
        name: "tracker-docs-take-main",
        covers: is_tracker_doc,
        resolve: tracker_rule,
    },
    Rule {
        name: "spec-table-rows",
        covers: is_spec_table,
        resolve: spec_rule,
    },
];

/// The resolution of every path of `conflicted`, read from the index and the objects and
/// writing nothing; or, where any path has no rule or its rule refuses, one refusal per such
/// path, and no path is resolved. A read that fails is a refusal too, and so is a resolution
/// naming a file [`super::Git::writable`] refuses, so the conflict is then left exactly as git
/// left it.
pub(super) fn plan(
    git: &Git<'_>,
    conflicted: &BTreeSet<Vec<u8>>,
) -> Result<Resolution, Vec<String>> {
    let stages = git.unmerged().map_err(|why| {
        conflicted
            .iter()
            .map(|path| {
                format!(
                    "{}: its index stages were not read ({why})",
                    display_git_path(path)
                )
            })
            .collect::<Vec<_>>()
    })?;
    let mut writes = Vec::new();
    let mut said = Vec::new();
    let mut refusals = Vec::new();
    for raw in conflicted {
        let shown = display_git_path(raw);
        let found = std::str::from_utf8(raw).ok().and_then(|path| {
            RULES
                .iter()
                .find(|rule| (rule.covers)(path))
                .map(|rule| (path, rule))
        });
        let Some((path, rule)) = found else {
            refusals.push(format!("{shown}: no rule"));
            continue;
        };
        let resolved = match stages.get(raw) {
            Some(sides) => (rule.resolve)(git, path, sides),
            None => Err("the index holds no stage of it".to_owned()),
        };
        // A file the checkout cannot hold where the resolution names it (a path through a
        // symbolic link, say) is the rule's refusal here, before anything is written, not a
        // failed write that aborts the merge the person could resolve by hand.
        let resolved = resolved.and_then(|(files, detail)| {
            match files.iter().find_map(|(file, _)| git.writable(file).err()) {
                Some(why) => Err(why),
                None => Ok((files, detail)),
            }
        });
        match resolved {
            Ok((files, detail)) => {
                writes.extend(files);
                said.push(format!("{}: {shown} ({detail})", rule.name));
            }
            Err(why) => refusals.push(format!("{shown}: {} refused: {why}", rule.name)),
        }
    }
    if refusals.is_empty() {
        Ok(Resolution { writes, said })
    } else {
        Err(refusals)
    }
}

/// Write and stage every file of `resolution`, in order ([`super::Git::put`]). On the first
/// failure every file this call created is removed again before the error returns, so the
/// caller's abort of the merge (which restores the tracked files) finds no file of the rules
/// left behind.
pub(super) fn apply(git: &Git<'_>, resolution: &Resolution) -> Result<(), String> {
    let mut created: Vec<PathBuf> = Vec::new();
    for (path, bytes) in &resolution.writes {
        if let Err(why) = git.put(path, bytes, &mut created) {
            for file in &created {
                let _ = std::fs::remove_file(file);
            }
            return Err(format!("the rules' write of {path:?} failed: {why}"));
        }
    }
    Ok(())
}

/// Each side's copy of a path as text, where all three hold it as a regular file in UTF-8.
fn texts(git: &Git<'_>, stages: &Stages) -> Result<[String; 3], String> {
    let [base, ours, theirs] = stages;
    Ok([
        text(git, SIDES[0], base.as_ref())?,
        text(git, SIDES[1], ours.as_ref())?,
        text(git, SIDES[2], theirs.as_ref())?,
    ])
}

/// `side`'s copy of a path as text: a regular file's blob in UTF-8, else why not.
fn text(git: &Git<'_>, side: &str, stage: Option<&Stage>) -> Result<String, String> {
    let Some(stage) = stage else {
        return Err(format!("{side} has no copy of it"));
    };
    if stage.mode != REGULAR {
        return Err(format!(
            "{side}'s copy is mode {}, not a regular file",
            stage.mode
        ));
    }
    String::from_utf8(git.blob(&stage.oid)?).map_err(|_| format!("{side}'s copy is not UTF-8"))
}

fn is_changelog(path: &str) -> bool {
    path == CHANGELOG_FILE
}

/// The changelog rule on the index: [`changelog`] over the three sides, with a fragment path
/// taken where anything stands at it in the checkout (main's own new files are there already).
/// `CHANGELOG.md` becomes main's file, and each fragment is written beside it.
fn changelog_rule(git: &Git<'_>, path: &str, stages: &Stages) -> Result<Resolved, String> {
    let [base, ours, theirs] = texts(git, stages)?;
    let root = git.root;
    let taken = |candidate: &str| std::fs::symlink_metadata(root.join(candidate)).is_ok();
    let (fragments, detail) = changelog(&base, &ours, &theirs, &taken)?;
    let mut writes = vec![(path.to_owned(), theirs.into_bytes())];
    writes.extend(
        fragments
            .into_iter()
            .map(|fragment| (fragment.path, fragment.text.into_bytes())),
    );
    Ok((writes, detail))
}

/// The longest slug of an entry's words a fragment name carries.
const SLUG_BYTES: usize = 60;

/// How many names one entry tries (`land-<slug>`, then `-2`, `-3`, …) before the rule refuses.
const NAME_TRIES: usize = 100;

/// THE CHANGELOG RULE: `CHANGELOG.md` takes main's file (`theirs`), and each entry the branch
/// (`ours`) added under `[Unreleased]` since the merge base (`base`) becomes its own fragment
/// ([`changelog_fragments::fragment`]), named `land-<slug of its words>` (suffixed `-2`, `-3`, …
/// where `taken` says a path is), with what was done in a few words. An entry main's
/// `[Unreleased]` already holds is not written again, and a harness-versions entry the branch
/// added is dropped, since `ship release` derives it.
///
/// It refuses, so the conflict stays the person's, wherever main's file would drop the
/// branch's work: a change outside `[Unreleased]`, a changed HTML comment in it, a base entry
/// the branch removed, reworded or moved to another kind, an added entry under no `### <kind>`
/// heading, an added entry whose words only a released section of main's file holds, a
/// malformed `[Unreleased]` and an entry no file the checker accepts can hold.
pub(super) fn changelog(
    base: &str,
    ours: &str,
    theirs: &str,
    taken: &dyn Fn(&str) -> bool,
) -> Result<(Vec<Fragment>, String), String> {
    let (Some(base_outside), Some(ours_outside)) =
        (outside_unreleased(base), outside_unreleased(ours))
    else {
        return Err("the merge base or the branch has no ## [Unreleased] section".into());
    };
    if base_outside != ours_outside {
        return Err(
            "the branch changed CHANGELOG.md outside [Unreleased], which main's file would drop"
                .into(),
        );
    }
    let malformed = |side: &str, findings: Vec<String>| {
        format!(
            "{side}'s [Unreleased] is malformed: {}",
            findings.join("; ")
        )
    };
    let base_body =
        changelog_fragments::unreleased_by_kind(base).map_err(|f| malformed(SIDES[0], f))?;
    let ours_body =
        changelog_fragments::unreleased_by_kind(ours).map_err(|f| malformed(SIDES[1], f))?;
    if base_body.comments != ours_body.comments {
        return Err(
            "the branch changed an HTML comment in [Unreleased], which main's file would drop"
                .into(),
        );
    }
    let key = changelog_fragments::entry_key;
    for (kind, text) in &base_body.entries {
        let words = key(text);
        match ours_body
            .entries
            .iter()
            .find(|(_, other)| key(other) == words)
        {
            Some((other, _)) if other == kind => {}
            Some(_) => {
                return Err(format!(
                    "the branch moved the entry {:?} to another kind",
                    first_line(text)
                ));
            }
            None => {
                return Err(format!(
                    "the branch removed or reworded the entry {:?}, which main's file keeps or \
                     shipped",
                    first_line(text)
                ));
            }
        }
    }
    let based: BTreeSet<String> = base_body.entries.iter().map(|(_, t)| key(t)).collect();
    // Main's entries by where they sit: one in its `[Unreleased]` ships in the next notes as
    // the branch's would, while one in a released section says nothing of the branch's change.
    let (mut main_unreleased, mut main_released) = (BTreeSet::new(), BTreeSet::new());
    for entry in changelog_fragments::entries(theirs) {
        if matches!(entry.section, Section::Released(_)) {
            main_released.insert(key(&entry.text));
        } else {
            main_unreleased.insert(key(&entry.text));
        }
    }
    let mut fragments: Vec<Fragment> = Vec::new();
    let mut already = 0;
    for (kind, text) in &ours_body.entries {
        let words = key(text);
        if based.contains(&words) {
            continue;
        }
        if main_unreleased.contains(&words) {
            already += 1;
            continue;
        }
        if main_released.contains(&words) {
            return Err(format!(
                "the branch's entry {:?} has the words of an entry main has already released, \
                 so the next notes would not carry it",
                first_line(text)
            ));
        }
        let Some(kind) = kind else {
            return Err(format!(
                "the branch's entry {:?} sits under no ### <kind> heading",
                first_line(text)
            ));
        };
        let stem = format!("land-{}", slug(first_line(text)));
        let mut named = None;
        for n in 1..=NAME_TRIES {
            let name = if n == 1 {
                stem.clone()
            } else {
                format!("{stem}-{n}")
            };
            let fragment = changelog_fragments::fragment(kind, &name, text).map_err(|found| {
                format!(
                    "the branch's entry {:?} makes no file the checker accepts: {}",
                    first_line(text),
                    found.join("; ")
                )
            })?;
            if !taken(&fragment.path) && !fragments.iter().any(|f| f.path == fragment.path) {
                named = Some(fragment);
                break;
            }
        }
        let Some(fragment) = named else {
            return Err(format!(
                "every name {stem}… up to -{NAME_TRIES} is taken in changes/"
            ));
        };
        fragments.push(fragment);
    }
    let mut detail = if fragments.is_empty() {
        "main's file; the branch added no entry of its own".to_owned()
    } else {
        format!(
            "main's file, and the branch's entries as {}",
            fragments
                .iter()
                .map(|fragment| fragment.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    if already > 0 {
        detail.push_str(&format!(
            "; {already} entr{} the branch added main's file already holds",
            if already == 1 { "y" } else { "ies" }
        ));
    }
    if harness_entries(ours) > harness_entries(base) {
        detail
            .push_str("; the branch's harness-versions entry dropped, which ship release derives");
    }
    Ok((fragments, detail))
}

/// `text`'s lines outside the `[Unreleased]` body (its heading included), and whether it ends
/// in a newline; `None` where it has no `## [Unreleased]`.
fn outside_unreleased(text: &str) -> Option<(Vec<&str>, bool)> {
    let (start, end) = section_span(text, "Unreleased")?;
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = lines[..=start].to_vec();
    kept.extend_from_slice(&lines[end..]);
    Some((kept, text.ends_with('\n')))
}

/// How many harness-versions entries `text`'s `[Unreleased]` body holds.
fn harness_entries(text: &str) -> usize {
    section_span(text, "Unreleased").map_or(0, |(start, end)| {
        text.lines()
            .take(end)
            .skip(start + 1)
            .filter(|line| line.starts_with(HARNESS_ENTRY_HEAD))
            .count()
    })
}

/// An entry's first line.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// An entry line's words as a fragment name's slug: lowercase ASCII letters and digits, every
/// run of anything else one `-`, none at either end, at most [`SLUG_BYTES`] bytes; `entry`
/// where nothing is left.
fn slug(line: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in changelog_fragments::entry_key(line).chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    out.truncate(SLUG_BYTES);
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "entry".to_owned()
    } else {
        out.to_owned()
    }
}

/// The documents `goals.py repo-docs` regenerates whole from the lab tracker.
const TRACKER_DOCS: [&str; 2] = ["docs/PLAN.md", "docs/STATUS.md"];

/// The start of the line a tracker-generated document carries in its first lines.
const GENERATED: &str = "Generated from the lab tracker";

fn is_tracker_doc(path: &str) -> bool {
    TRACKER_DOCS.contains(&path)
}

/// THE TRACKER-DOCS RULE: `docs/PLAN.md` and `docs/STATUS.md` take main's copy, which drops
/// only the branch's copy of a file the tracker regenerates; main's copy may be older than the
/// tracker, so what the rule says names the command that regenerates it. It refuses where any
/// side's copy is not a regular file, and where main's copy does not say, in its first 5 lines,
/// that it is generated by `goals.py repo-docs` and not to be edited (`docs/OWNER_RULINGS.md`,
/// only partly generated, does not say so, and is not covered either).
fn tracker_rule(git: &Git<'_>, path: &str, stages: &Stages) -> Result<Resolved, String> {
    for (side, stage) in SIDES.iter().zip(stages) {
        if let Some(stage) = stage
            && stage.mode != REGULAR
        {
            return Err(format!(
                "{side}'s copy is mode {}, not a regular file",
                stage.mode
            ));
        }
    }
    let theirs = text(git, SIDES[2], stages[2].as_ref())?;
    if !generated(&theirs) {
        return Err(
            "main's copy does not say it is generated from the lab tracker and not to be edited"
                .into(),
        );
    }
    // Main's copy may predate a tracker change the branch regenerated its own copy from, so the
    // line every reader of the resolution sees names the regeneration that restores it.
    Ok((
        vec![(path.to_owned(), theirs.into_bytes())],
        "main's copy; where the branch changed the tracker, regenerate it with `goals.py \
         repo-docs aterm --out <worktree>`"
            .into(),
    ))
}

/// Whether one of `text`'s first 5 lines is the line `goals.py repo-docs` heads a document it
/// generates whole with.
fn generated(text: &str) -> bool {
    text.lines().take(5).any(|line| {
        line.starts_with(GENERATED)
            && line.contains("by `goals.py repo-docs")
            && line.contains("Do not edit this file")
    })
}

/// The file name of a crate's spec table.
const SPEC_FILE: &str = "SPECS.md";

fn is_spec_table(path: &str) -> bool {
    path.rsplit('/').next() == Some(SPEC_FILE)
}

/// The spec-table rule on the index: [`spec_table`] over the three sides.
fn spec_rule(git: &Git<'_>, path: &str, stages: &Stages) -> Result<Resolved, String> {
    let [base, ours, theirs] = texts(git, stages)?;
    let (merged, detail) = spec_table(&base, &ours, &theirs)?;
    Ok((vec![(path.to_owned(), merged.into_bytes())], detail))
}

/// The line that opens a spec table, and the one that closes it.
const TABLE_OPEN: &str = "```tsv";
const TABLE_CLOSE: &str = "```";

/// One side's `SPECS.md` split at its tables: its lines (split at `\n`, so a final newline is a
/// last empty line), and each table's rows, the lines strictly between its fences.
struct Tables<'a> {
    lines: Vec<&'a str>,
    blocks: Vec<Range<usize>>,
}

/// `side`'s `text` split at its tables, or why it is not read: a carriage return, or a table
/// that never closes.
fn tables<'a>(side: &str, text: &'a str) -> Result<Tables<'a>, String> {
    if text.contains('\r') {
        return Err(format!("{side}'s copy holds a carriage return"));
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut blocks = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        if lines[at] != TABLE_OPEN {
            at += 1;
            continue;
        }
        let start = at + 1;
        let Some(len) = lines[start..].iter().position(|line| *line == TABLE_CLOSE) else {
            return Err(format!(
                "{side}'s copy has a {TABLE_OPEN} table that never closes"
            ));
        };
        blocks.push(start..start + len);
        at = start + len + 1;
    }
    Ok(Tables { lines, blocks })
}

impl Tables<'_> {
    /// Table `table`'s rows.
    fn rows(&self, table: usize) -> &[&str] {
        &self.lines[self.blocks[table].clone()]
    }

    /// The index of the first line outside the tables that states the counts ([`counts`]).
    fn result_line(&self) -> Option<usize> {
        (0..self.lines.len()).find(|at| {
            !self.blocks.iter().any(|block| block.contains(at)) && counts(self.lines[*at]).is_some()
        })
    }

    /// The text outside the tables, line by line: each table's rows one marker line, and the
    /// counts of the line at `masked`, where it is one, masked.
    fn skeleton(&self, masked: Option<usize>) -> Vec<String> {
        let mut out = Vec::new();
        let (mut at, mut table) = (0, 0);
        while at < self.lines.len() {
            if let Some(block) = self.blocks.get(table)
                && block.start == at
            {
                out.push(format!("\0the rows of table {table}"));
                at = block.end;
                table += 1;
                continue;
            }
            let line = self.lines[at];
            out.push(match counts(line) {
                Some(found) if masked == Some(at) => found.masked(),
                _ => line.to_owned(),
            });
            at += 1;
        }
        out
    }

    /// This side's text with each table's rows `rows`, and the line at `result.0`, where one is
    /// given, written `result.1`.
    fn rebuild(&self, rows: &[Vec<&str>], result: Option<(usize, &str)>) -> String {
        let mut out: Vec<&str> = Vec::new();
        let (mut at, mut table) = (0, 0);
        while at < self.lines.len() {
            if let Some(block) = self.blocks.get(table)
                && block.start == at
            {
                out.extend(rows[table].iter().copied());
                at = block.end;
                table += 1;
                continue;
            }
            out.push(match result {
                Some((line, text)) if line == at => text,
                _ => self.lines[at],
            });
            at += 1;
        }
        out.join("\n")
    }
}

/// The counts a `SPECS.md` states, `Result: **<n> spec…** … **<m> …`: the two numbers and the
/// text around them.
struct Counts<'a> {
    n: u64,
    m: u64,
    head: &'a str,
    mid: &'a str,
    tail: &'a str,
}

impl Counts<'_> {
    /// The line with both numbers masked.
    fn masked(&self) -> String {
        format!("{}#{}#{}", self.head, self.mid, self.tail)
    }
}

/// `line`'s counts, where it begins `Result: **<n> spec` and its next bold run begins with
/// digits and a space (`**<m> `).
fn counts(line: &str) -> Option<Counts<'_>> {
    const HEAD: &str = "Result: **";
    let rest = line.strip_prefix(HEAD)?;
    let n_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    if n_len == 0 || !rest[n_len..].starts_with(" spec") {
        return None;
    }
    let n = rest[..n_len].parse().ok()?;
    let after_n = HEAD.len() + n_len;
    let close = after_n + line[after_n..].find("**")?;
    let m_start = close + 2 + line[close + 2..].find("**")? + 2;
    let m_len = line[m_start..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if m_len == 0 || !line[m_start + m_len..].starts_with(' ') {
        return None;
    }
    let m = line[m_start..m_start + m_len].parse().ok()?;
    Some(Counts {
        n,
        m,
        head: &line[..HEAD.len()],
        mid: &line[after_n..m_start],
        tail: &line[m_start + m_len..],
    })
}

/// THE SPEC-TABLE RULE: a `SPECS.md` whose tables both sides changed takes each row from the
/// side that changed it against the merge base, keyed by the narrowest leading one or two
/// columns unique in all three versions of a table. The text outside the tables comes from
/// the side that changed it (both may only where they made the same change). Rows stay in
/// main's order; a row the branch added follows the nearest row before it in the branch's
/// order, after any rows main added right there, or opens the table where none is. Where the
/// file has exactly two tables and states `Result: **<n> spec…** … **<m> …` on all three
/// sides, the counts are main's moved by the rows each table gained or lost against main's
/// (main's own offset kept, so the branch's miscount does not carry over); anywhere else that
/// line is plain text. The merged text, and what came from where.
///
/// It refuses, so the conflict stays the person's: a carriage return, an unclosed table, a
/// different number of tables, text outside the tables both sides changed differently, a
/// table no key of one or two columns is unique in (a wider one would pair unrelated edits),
/// a branch that reordered rows (main's order would drop that), and a row both sides changed
/// differently (an edit against an edit, a deletion against an edit, or two different rows
/// added under one key).
pub(super) fn spec_table(base: &str, ours: &str, theirs: &str) -> Result<(String, String), String> {
    let sides = [
        tables(SIDES[0], base)?,
        tables(SIDES[1], ours)?,
        tables(SIDES[2], theirs)?,
    ];
    let count = sides[0].blocks.len();
    if sides.iter().any(|side| side.blocks.len() != count) {
        return Err(format!(
            "the sides hold different numbers of {TABLE_OPEN} tables"
        ));
    }
    let results = sides.each_ref().map(Tables::result_line);
    let recounted = count == 2 && results.iter().all(Option::is_some);
    let skeletons: Vec<Vec<String>> = sides
        .iter()
        .zip(results)
        .map(|(side, at)| side.skeleton(at.filter(|_| recounted)))
        .collect();
    let chosen = if skeletons[1] == skeletons[0] {
        2
    } else if skeletons[2] == skeletons[0] || skeletons[1] == skeletons[2] {
        1
    } else {
        return Err("both sides changed the text outside the tables".into());
    };
    let mut rows: Vec<Vec<&str>> = Vec::with_capacity(count);
    let (mut from_branch, mut from_main) = (0, 0);
    for table in 0..count {
        let (merged, branch, main) = merge_rows(
            table + 1,
            sides[0].rows(table),
            sides[1].rows(table),
            sides[2].rows(table),
        )?;
        from_branch += branch;
        from_main += main;
        rows.push(merged);
    }
    let result = match (recounted, results[chosen], results[2]) {
        (true, Some(at), Some(main_at)) => {
            let (Some(line), Some(main)) = (
                counts(sides[chosen].lines[at]),
                counts(sides[2].lines[main_at]),
            ) else {
                return Err("the Result line was not read again".into());
            };
            let moved = |stated: u64, table: usize| {
                let gained = rows[table].len() as i128 - sides[2].rows(table).len() as i128;
                u64::try_from(i128::from(stated) + gained).ok()
            };
            let (Some(n), Some(m)) = (moved(main.n, 0), moved(main.m, 1)) else {
                return Err("the recounted Result line would count below zero".into());
            };
            Some((at, format!("{}{n}{}{m}{}", line.head, line.mid, line.tail)))
        }
        _ => None,
    };
    let text = sides[chosen].rebuild(
        &rows,
        result.as_ref().map(|(at, line)| (*at, line.as_str())),
    );
    let detail = format!(
        "{from_branch} row{} from the branch, {from_main} from main{}",
        if from_branch == 1 { "" } else { "s" },
        if result.is_some() { ", recounted" } else { "" }
    );
    Ok((text, detail))
}

/// A row's key: its first `width` cells (a missing one empty), tab-joined.
fn key(row: &str, width: usize) -> String {
    let mut cells = row.split('\t');
    (0..width)
        .map(|_| cells.next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\t")
}

/// Whether every row of `rows` has its own key of `width` cells.
fn unique(rows: &[&str], width: usize) -> bool {
    let mut seen = BTreeSet::new();
    rows.iter().all(|row| seen.insert(key(row, width)))
}

/// Each row of `rows` with its key of `width` cells, in order.
fn keyed<'a>(rows: &[&'a str], width: usize) -> Vec<(String, &'a str)> {
    rows.iter().map(|row| (key(row, width), *row)).collect()
}

/// `rows` by key.
fn by_key<'k, 'a>(rows: &'k [(String, &'a str)]) -> BTreeMap<&'k str, &'a str> {
    rows.iter().map(|(key, row)| (key.as_str(), *row)).collect()
}

/// Table `table`'s (1-based) rows merged from the base's, ours and theirs ([`spec_table`]),
/// and how many came from the branch and from main where the other side left them alone.
fn merge_rows<'a>(
    table: usize,
    base: &[&'a str],
    ours: &[&'a str],
    theirs: &[&'a str],
) -> Result<(Vec<&'a str>, usize, usize), String> {
    let width = [1, 2]
        .into_iter()
        .find(|width| [base, ours, theirs].iter().all(|rows| unique(rows, *width)))
        .ok_or_else(|| format!("table {table}: no key of one or two columns is unique"))?;
    let (base, ours, theirs) = (keyed(base, width), keyed(ours, width), keyed(theirs, width));
    let (in_base, in_ours, in_theirs) = (by_key(&base), by_key(&ours), by_key(&theirs));
    let kept = |rows: &[(String, &str)], other: &BTreeMap<&str, &str>| -> Vec<String> {
        rows.iter()
            .filter(|(key, _)| other.contains_key(key.as_str()))
            .map(|(key, _)| key.clone())
            .collect()
    };
    if kept(&base, &in_ours) != kept(&ours, &in_base) {
        return Err(format!(
            "table {table}: the branch reordered rows, which main's order would drop"
        ));
    }
    let every: BTreeSet<&str> = in_base
        .keys()
        .chain(in_ours.keys())
        .chain(in_theirs.keys())
        .copied()
        .collect();
    let mut resolved: BTreeMap<&str, Option<&'a str>> = BTreeMap::new();
    let (mut from_branch, mut from_main) = (0, 0);
    for key in every {
        let (b, o, t) = (
            in_base.get(key).copied(),
            in_ours.get(key).copied(),
            in_theirs.get(key).copied(),
        );
        let row = if o == t {
            o
        } else if o == b {
            t
        } else if t == b {
            o
        } else {
            return Err(format!(
                "table {table}: row `{}` changed differently on both sides",
                key.replace('\t', " ")
            ));
        };
        if row.is_some() && o != b && row == o {
            from_branch += 1;
        } else if row.is_some() && t != b && row == t {
            from_main += 1;
        }
        resolved.insert(key, row);
    }
    let main_added: BTreeSet<&str> = in_theirs
        .keys()
        .filter(|key| !in_base.contains_key(*key))
        .copied()
        .collect();
    let mut out: Vec<(&str, &'a str)> = theirs
        .iter()
        .filter_map(|(key, _)| {
            resolved
                .get(key.as_str())
                .copied()
                .flatten()
                .map(|row| (key.as_str(), row))
        })
        .collect();
    for (at, (key, row)) in ours.iter().enumerate() {
        if in_base.contains_key(key.as_str()) || in_theirs.contains_key(key.as_str()) {
            continue;
        }
        let after = ours[..at].iter().rev().find_map(|(earlier, _)| {
            out.iter()
                .position(|(placed, _)| *placed == earlier.as_str())
        });
        let place = match after {
            Some(found) => {
                let mut place = found + 1;
                while place < out.len() && main_added.contains(out[place].0) {
                    place += 1;
                }
                place
            }
            None => 0,
        };
        out.insert(place, (key.as_str(), *row));
    }
    Ok((
        out.into_iter().map(|(_, row)| row).collect(),
        from_branch,
        from_main,
    ))
}
