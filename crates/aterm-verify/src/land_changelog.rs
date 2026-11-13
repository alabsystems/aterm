// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Entry-wise three-way merge of the hand-written changelog.

use std::collections::{BTreeMap, BTreeSet};

pub(super) const PATH: &str = "CHANGELOG.md";
const UNRELEASED: &str = "Unreleased";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Conflict {
    pub(super) reason: String,
    pub(super) entries: Vec<String>,
}

impl Conflict {
    fn new(reason: impl Into<String>, entries: impl IntoIterator<Item = String>) -> Self {
        let mut entries: Vec<String> = entries.into_iter().collect();
        entries.sort();
        entries.dedup();
        Self {
            reason: reason.into(),
            entries,
        }
    }
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)?;
        if !self.entries.is_empty() {
            write!(f, ": ")?;
            for (n, entry) in self.entries.iter().enumerate() {
                if n > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "\"{entry}\"")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for Conflict {}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Location {
    section: String,
    kind: Option<String>,
}

#[derive(Clone, Debug)]
struct Entry {
    text: String,
    title: String,
    location: Location,
}

#[derive(Clone, Debug)]
struct Section {
    key: String,
    header: String,
    raw: String,
    entries: Vec<usize>,
    kinds: Vec<Option<String>>,
    structured: bool,
    mergeable: bool,
}

#[derive(Clone, Debug)]
struct Document {
    prefix: String,
    sections: Vec<Section>,
    entries: Vec<Entry>,
}

impl Document {
    fn section(&self, key: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.key == key)
    }

    fn keys(&self) -> Vec<String> {
        self.sections
            .iter()
            .map(|section| section.key.clone())
            .collect()
    }
}

/// Titles whose presence, words, location, or relative order differs between two changelogs.
/// This is intentionally linearithmic in file size; a multiple-base diagnostic must not compare
/// every historical entry with every other one.
pub(super) fn differing_titles(a: &str, b: &str) -> Result<Vec<String>, Conflict> {
    let a = parse(a, "first merge base")?;
    let b = parse(b, "second merge base")?;
    let mut counts = BTreeMap::<(Location, String), (isize, String)>::new();
    for entry in &a.entries {
        let row = counts
            .entry((entry.location.clone(), entry.text.clone()))
            .or_insert((0, entry.title.clone()));
        row.0 += 1;
    }
    for entry in &b.entries {
        let row = counts
            .entry((entry.location.clone(), entry.text.clone()))
            .or_insert((0, entry.title.clone()));
        row.0 -= 1;
    }
    let mut different: BTreeSet<String> = counts
        .values()
        .filter(|(count, _)| *count != 0)
        .map(|(_, title)| title.clone())
        .collect();

    fn sequences(document: &Document) -> BTreeMap<Location, Vec<(&str, &str)>> {
        let mut by_location = BTreeMap::<Location, Vec<(&str, &str)>>::new();
        for entry in &document.entries {
            by_location
                .entry(entry.location.clone())
                .or_default()
                .push((&entry.text, &entry.title));
        }
        by_location
    }
    let a_sequences = sequences(&a);
    let b_sequences = sequences(&b);
    for location in a_sequences
        .keys()
        .chain(b_sequences.keys())
        .collect::<BTreeSet<_>>()
    {
        let left = a_sequences.get(location).map(Vec::as_slice).unwrap_or(&[]);
        let right = b_sequences.get(location).map(Vec::as_slice).unwrap_or(&[]);
        if left
            .iter()
            .map(|(text, _)| text)
            .eq(right.iter().map(|(text, _)| text))
        {
            continue;
        }
        different.extend(left.iter().map(|(_, title)| (*title).to_owned()));
        different.extend(right.iter().map(|(_, title)| (*title).to_owned()));
    }
    Ok(different.into_iter().collect())
}

/// One exact entry at its semantic location. Landing history uses this identity to distinguish
/// an unchanged base entry from the same bytes a person explicitly removed and restored.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ExactEntry {
    location: Location,
    text: String,
    title: String,
}

impl ExactEntry {
    pub(super) fn title(&self) -> &str {
        &self.title
    }
}

/// Exact entries `from` holds and `to` does not, counting duplicate copies independently.
fn exact_removed(from: &str, to: &str) -> Result<Vec<ExactEntry>, Conflict> {
    let from = parse(from, "the history source")?;
    let to = parse(to, "the history endpoint")?;
    let mut held = BTreeMap::<(Location, String), usize>::new();
    for entry in &to.entries {
        *held
            .entry((entry.location.clone(), entry.text.clone()))
            .or_default() += 1;
    }
    let mut removed = Vec::new();
    for entry in from.entries {
        let key = (entry.location.clone(), entry.text.clone());
        if held.get_mut(&key).is_some_and(|count| {
            if *count == 0 {
                false
            } else {
                *count -= 1;
                true
            }
        }) {
            continue;
        }
        removed.push(ExactEntry {
            location: entry.location,
            text: entry.text,
            title: entry.title,
        });
    }
    Ok(removed)
}

/// Base entries a side removed, moved or rewrote.
pub(super) fn removed_entries(base: &str, side: &str) -> Result<Vec<ExactEntry>, Conflict> {
    exact_removed(base, side)
}

/// Entries a side added, moved or rewrote relative to the base.
pub(super) fn added_entries(base: &str, side: &str) -> Result<Vec<ExactEntry>, Conflict> {
    exact_removed(side, base)
}

/// Whether `text` holds this exact complete entry in the same section and subsection.
pub(super) fn holds_entry(text: &str, wanted: &ExactEntry) -> Result<bool, Conflict> {
    let document = parse(text, "the history endpoint")?;
    Ok(document
        .entries
        .iter()
        .any(|entry| entry.location == wanted.location && entry.text == wanted.text))
}

/// Merge `tip` and `main` from `base` by complete entry.
///
/// A tip that left the file byte-identical to `base` brings no entry to place, so the result is
/// main's bytes, whatever main did to its own entries since (a rewrite of several entries in one
/// subsection, a release cut): classifying main there could refuse only main's landed history,
/// which no change on the branch can resolve. Main left unchanged gets no such shortcut: the
/// tip's edits are what this merge judges by entry.
pub(super) fn merge(base: &str, tip: &str, main: &str) -> Result<String, Conflict> {
    if tip == base {
        return Ok(main.to_owned());
    }
    let base = parse(base, "merge base")?;
    let tip = parse(tip, "branch tip")?;
    let main = parse(main, "main")?;
    let tip_view = classify(&base, &tip, "branch tip")?;
    let main_view = classify(&base, &main, "main")?;

    let base_keys: BTreeSet<String> = base.keys().into_iter().collect();
    let frozen: BTreeSet<String> = main
        .sections
        .iter()
        .filter(|section| {
            section.mergeable && section.key != UNRELEASED && !base_keys.contains(&section.key)
        })
        .map(|section| section.key.clone())
        .collect();

    // A release section created on main is immutable input to this landing: the cutter already
    // decided its exact bytes. If the branch independently created the same version, equality is
    // the only proof that taking main does not discard branch work.
    for key in &frozen {
        let Some(tip_section) = tip.section(key) else {
            continue;
        };
        let main_section = main.section(key).expect("frozen main section");
        if tip_section.raw != main_section.raw {
            let names = tip_section
                .entries
                .iter()
                .map(|&entry| tip.entries[entry].title.clone())
                .chain(
                    main_section
                        .entries
                        .iter()
                        .map(|&entry| main.entries[entry].title.clone()),
                );
            return Err(Conflict::new(
                format!(
                    "main cut [{key}] differently from the branch's independently created section"
                ),
                names,
            ));
        }
    }
    if let Some(tip_section) = tip.sections.iter().find(|section| {
        section.mergeable
            && section.key != UNRELEASED
            && !base_keys.contains(&section.key)
            && !frozen.contains(&section.key)
    }) {
        let names = tip_section
            .entries
            .iter()
            .map(|&entry| tip.entries[entry].title.clone())
            .chain(frozen.iter().flat_map(|key| {
                let section = main.section(key).expect("frozen main section");
                section
                    .entries
                    .iter()
                    .map(|&entry| main.entries[entry].title.clone())
            }));
        return Err(Conflict::new(
            format!(
                "the branch independently created release [{}]; main's release cut is authoritative",
                tip_section.key
            ),
            names,
        ));
    }

    let mut frozen_base = BTreeSet::new();
    let mut cut_rewrites = Vec::new();
    for (id, old) in base.entries.iter().enumerate() {
        let Some(main_side) = main_view.states[id].exact() else {
            continue;
        };
        let cut = &main.entries[main_side];
        if !frozen.contains(&cut.location.section) {
            continue;
        }
        match tip_view.states[id] {
            SideState::Exact(side) if tip.entries[side].location == old.location => {}
            SideState::Exact(side) => {
                return Err(Conflict::new(
                    format!(
                        "main cut [{}] while the branch moved an entry",
                        cut.location.section
                    ),
                    [old.title.clone(), tip.entries[side].title.clone()],
                ));
            }
            SideState::Rewrite(side) if tip.entries[side].location.section == UNRELEASED => {
                // Main's release is immutable, but the branch's reviewed replacement has not
                // shipped. Keep it as a new Unreleased entry: the old entry's subsection and
                // ordering lineage moved into the frozen release with that old entry.
                cut_rewrites.push((id, side));
            }
            SideState::Rewrite(side) => {
                return Err(Conflict::new(
                    format!(
                        "main cut [{}] while the branch rewrote an entry into another release",
                        cut.location.section
                    ),
                    [old.title.clone(), tip.entries[side].title.clone()],
                ));
            }
            // A removal cannot rewrite history. Main's exact release wins and no copy remains
            // under Unreleased.
            SideState::Absent => {}
        }
        frozen_base.insert(id);
    }

    let mut result = Vec::<ResultEntry>::new();
    let mut base_to_node = vec![None; base.entries.len()];
    for (id, old) in base.entries.iter().enumerate() {
        if frozen_base.contains(&id) {
            continue;
        }
        let Some((text, title, location)) =
            merge_old(old, tip_view.states[id], &tip, main_view.states[id], &main)?
        else {
            continue;
        };
        let node = result.len();
        base_to_node[id] = Some(node);
        result.push(ResultEntry {
            text,
            title,
            location,
            base: Some(old.location.clone()),
            tip: tip_view.states[id].side(),
            main: main_view.states[id].side(),
        });
    }
    for (id, side) in cut_rewrites {
        let entry = &tip.entries[side];
        debug_assert!(frozen_base.contains(&id));
        result.push(ResultEntry {
            text: entry.text.clone(),
            title: entry.title.clone(),
            location: entry.location.clone(),
            base: None,
            tip: Some(side),
            main: None,
        });
    }

    let tip_identities = addition_identities(&tip, &tip_view.additions, "branch tip")?;
    let main_identities = addition_identities(&main, &main_view.additions, "main")?;
    for (identity, &tip_side) in &tip_identities {
        let Some(&main_side) = main_identities.get(identity) else {
            continue;
        };
        if tip.entries[tip_side].text != main.entries[main_side].text {
            return Err(Conflict::new(
                "the branch and main added the same titled entry differently",
                [tip.entries[tip_side].title.clone()],
            ));
        }
    }

    let mut same_addition = BTreeMap::<(Location, String), usize>::new();
    for &side in &main_view.additions {
        let entry = &main.entries[side];
        if frozen.contains(&entry.location.section) {
            continue;
        }
        let node = result.len();
        same_addition.insert((entry.location.clone(), entry.text.clone()), node);
        result.push(ResultEntry::addition(entry, None, Some(side)));
    }
    let shipped_additions: Vec<&Entry> = main
        .entries
        .iter()
        .filter(|entry| frozen.contains(&entry.location.section))
        .collect();
    let already_released: Vec<&Entry> = base
        .entries
        .iter()
        .filter(|entry| entry.location.section != UNRELEASED)
        .collect();
    for &side in &tip_view.additions {
        let entry = &tip.entries[side];
        if frozen.contains(&entry.location.section) {
            continue;
        }
        if let Some(released) = already_released.iter().find(|released| {
            released.location.kind == entry.location.kind && released.text == entry.text
        }) {
            return Err(Conflict::new(
                "the branch copied an already released entry into Unreleased",
                [entry.title.clone(), released.title.clone()],
            ));
        }
        if shipped_additions.iter().any(|shipped| {
            shipped.location.kind == entry.location.kind && shipped.text == entry.text
        }) {
            // Both sides independently wrote the same complete entry, and main already shipped
            // it in the release it cut. Publishing a second Unreleased copy would be a defect.
            continue;
        }
        if let Some(shipped) = shipped_additions.iter().find(|shipped| {
            shipped.location.kind == entry.location.kind
                && entry_identity(&shipped.text).is_some()
                && entry_identity(&shipped.text) == entry_identity(&entry.text)
        }) {
            return Err(Conflict::new(
                "the branch and main wrote one titled entry differently before main released it",
                [entry.title.clone(), shipped.title.clone()],
            ));
        }
        let key = (entry.location.clone(), entry.text.clone());
        if let Some(&node) = same_addition.get(&key) {
            result[node].tip = Some(side);
        } else {
            let node = result.len();
            same_addition.insert(key, node);
            result.push(ResultEntry::addition(entry, Some(side), None));
        }
    }

    let present = section_presence(&base, &tip, &main, &frozen, &result)?;
    let order = section_order(&base, &tip, &main, &present)?;
    let headers = section_headers(&base, &tip, &main, &frozen, &present)?;

    let mut tip_to_node = vec![None; tip.entries.len()];
    let mut main_to_node = vec![None; main.entries.len()];
    for (node, entry) in result.iter().enumerate() {
        if let Some(side) = entry.tip {
            tip_to_node[side] = Some(node);
        }
        if let Some(side) = entry.main {
            main_to_node[side] = Some(node);
        }
    }

    let mut out = scalar(
        &base.prefix,
        &tip.prefix,
        &main.prefix,
        "both sides changed text before the first changelog section",
        Vec::new(),
    )?;
    for (position, key) in order.iter().enumerate() {
        let last = position + 1 == order.len();
        let base_section = base.section(key);
        let tip_section = tip.section(key);
        let main_section = main.section(key);
        if frozen.contains(key) {
            out.push_str(&main_section.expect("frozen main section").raw);
            continue;
        }
        let all_same = base_section
            .zip(tip_section)
            .zip(main_section)
            .is_some_and(|((base, tip), main)| base.raw == tip.raw && base.raw == main.raw);
        if all_same {
            out.push_str(&main_section.expect("same section").raw);
            continue;
        }
        if [base_section, tip_section, main_section]
            .into_iter()
            .flatten()
            .any(|section| !section.mergeable)
        {
            let raw = merge_optional_raw(base_section, tip_section, main_section, key)?;
            out.push_str(&raw);
            continue;
        }
        let relevant: Vec<&Section> = [base_section, tip_section, main_section]
            .into_iter()
            .flatten()
            .collect();
        if relevant.iter().any(|section| !section.structured) {
            let names = [
                base_section.map(|section| (section, base.entries.as_slice())),
                tip_section.map(|section| (section, tip.entries.as_slice())),
                main_section.map(|section| (section, main.entries.as_slice())),
            ]
            .into_iter()
            .flatten()
            .flat_map(|(section, entries)| {
                section
                    .entries
                    .iter()
                    .map(|&entry| entries[entry].title.clone())
            });
            return Err(Conflict::new(
                format!("[{key}] has non-entry text whose placement is ambiguous"),
                names,
            ));
        }
        let kinds = kind_order(key, &base, &tip, &main, &result)?;
        let mut groups = Vec::new();
        for kind in kinds {
            let location = Location {
                section: key.clone(),
                kind: kind.clone(),
            };
            let nodes: Vec<usize> = result
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.location == location)
                .map(|(node, _)| node)
                .collect();
            if nodes.is_empty() {
                continue;
            }
            let nodes = entry_order(
                &location,
                &nodes,
                &base,
                &tip,
                &main,
                &base_to_node,
                &tip_to_node,
                &main_to_node,
                &result,
            )?;
            groups.push((kind, nodes));
        }
        let header = headers.get(key).expect("present header");
        // All entry and ordering decisions above still run. When the branch left this
        // section alone and the result is exactly main's structure, keep main's separators
        // too: inserting blank lines would manufacture an unrelated branch edit.
        let unchanged_tip = base_section
            .zip(tip_section)
            .is_some_and(|(base, tip)| base.raw == tip.raw);
        if let Some(section) = main_section.filter(|section| {
            unchanged_tip
                && section_result_matches(section, &main.entries, header, &groups, &result)
        }) {
            out.push_str(&section.raw);
        } else {
            out.push_str(&render(header, &groups, &result, last));
        }
    }
    Ok(out)
}

fn parse(text: &str, which: &str) -> Result<Document, Conflict> {
    if text.contains('\r') {
        return Err(Conflict::new(
            format!("{which}'s {PATH} contains a carriage return"),
            Vec::new(),
        ));
    }
    let mut headings = Vec::<(usize, String, String, bool)>::new();
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        if line.starts_with("## ") {
            let mergeable = bracket_key(line);
            let key = mergeable.clone().unwrap_or_else(|| format!("plain:{line}"));
            headings.push((offset, key, line.to_owned(), mergeable.is_some()));
        }
        offset += raw.len();
    }
    if headings.is_empty() {
        return Err(Conflict::new(
            format!("{which}'s {PATH} has no level-two section"),
            Vec::new(),
        ));
    }
    let mut seen = BTreeSet::new();
    for (_, key, _, _) in &headings {
        if !seen.insert(key.clone()) {
            return Err(Conflict::new(
                format!("{which}'s {PATH} repeats section {key}"),
                Vec::new(),
            ));
        }
    }

    let prefix = text[..headings[0].0].to_owned();
    let mut sections = Vec::new();
    let mut entries = Vec::new();
    for (n, (start, key, header, mergeable)) in headings.iter().enumerate() {
        let end = headings.get(n + 1).map_or(text.len(), |next| next.0);
        let raw = text[*start..end].to_owned();
        let (local, kinds, structured) = if *mergeable {
            parse_body(&raw)
        } else {
            (Vec::new(), Vec::new(), false)
        };
        let mut indices = Vec::new();
        for (kind, entry_text) in local {
            let id = entries.len();
            indices.push(id);
            entries.push(Entry {
                title: entry_title(&entry_text),
                text: entry_text,
                location: Location {
                    section: key.clone(),
                    kind,
                },
            });
        }
        sections.push(Section {
            key: key.clone(),
            header: header.clone(),
            raw,
            entries: indices,
            kinds,
            structured,
            mergeable: *mergeable,
        });
    }
    Ok(Document {
        prefix,
        sections,
        entries,
    })
}

fn bracket_key(line: &str) -> Option<String> {
    let rest = line.strip_prefix("## [")?;
    let (key, tail) = rest.split_once(']')?;
    if key.is_empty() || !(tail.is_empty() || tail.starts_with([' ', '\t', '-'])) {
        return None;
    }
    Some(key.to_owned())
}

type ParsedBody = (Vec<(Option<String>, String)>, Vec<Option<String>>, bool);

fn kind_heading(line: &str) -> Option<&str> {
    line.strip_prefix("### ")
        .map(|heading| heading.trim().trim_end_matches('#').trim())
}

fn parse_body(raw: &str) -> ParsedBody {
    let body = raw.split_once('\n').map_or("", |(_, body)| body);
    let lines: Vec<&str> = body.split('\n').collect();
    let mut entries = Vec::new();
    let mut kinds = Vec::new();
    let mut kind = None::<String>;
    let mut structured = true;
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        if let Some(heading) = kind_heading(line) {
            kind = Some(heading.to_owned());
            if !kinds.contains(&kind) {
                kinds.push(kind.clone());
            }
            at += 1;
            continue;
        }
        if is_entry(line) {
            if !kinds.contains(&kind) {
                kinds.push(kind.clone());
            }
            let start = at;
            let mut end = at + 1;
            at += 1;
            while at < lines.len()
                && (lines[at].trim().is_empty() || lines[at].starts_with(char::is_whitespace))
            {
                if !lines[at].trim().is_empty() {
                    end = at + 1;
                }
                at += 1;
            }
            entries.push((kind.clone(), lines[start..end].join("\n")));
            continue;
        }
        if !line.trim().is_empty() {
            structured = false;
        }
        at += 1;
    }
    (entries, kinds, structured)
}

fn is_entry(line: &str) -> bool {
    line.starts_with("- ") || line.starts_with("* ")
}

fn entry_title(text: &str) -> String {
    let words = text
        .lines()
        .flat_map(str::split_whitespace)
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    entry_identity_from_words(&words).unwrap_or_else(|| words.chars().take(72).collect())
}

fn entry_identity(text: &str) -> Option<String> {
    let words = text
        .lines()
        .flat_map(str::split_whitespace)
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    entry_identity_from_words(&words)
}

fn entry_identity_from_words(words: &str) -> Option<String> {
    let lead = words.strip_prefix("**")?;
    let (bold, _) = lead.split_once("**")?;
    (!bold.is_empty()).then(|| bold.to_owned())
}

#[derive(Clone, Copy, Debug)]
enum SideState {
    Exact(usize),
    Rewrite(usize),
    Absent,
}

impl SideState {
    fn side(self) -> Option<usize> {
        match self {
            Self::Exact(side) | Self::Rewrite(side) => Some(side),
            Self::Absent => None,
        }
    }

    fn exact(self) -> Option<usize> {
        match self {
            Self::Exact(side) => Some(side),
            Self::Rewrite(_) | Self::Absent => None,
        }
    }
}

struct SideView {
    states: Vec<SideState>,
    additions: Vec<usize>,
}

fn addition_identities(
    document: &Document,
    additions: &[usize],
    which: &str,
) -> Result<BTreeMap<(Location, String), usize>, Conflict> {
    let mut identities = BTreeMap::new();
    for &side in additions {
        let entry = &document.entries[side];
        let Some(identity) = entry_identity(&entry.text) else {
            continue;
        };
        let key = (entry.location.clone(), identity);
        if let Some(previous) = identities.insert(key, side) {
            return Err(Conflict::new(
                format!("{which} repeats one newly added titled entry"),
                [
                    document.entries[previous].title.clone(),
                    entry.title.clone(),
                ],
            ));
        }
    }
    Ok(identities)
}

fn classify(base: &Document, side: &Document, which: &str) -> Result<SideView, Conflict> {
    let mut states = vec![SideState::Absent; base.entries.len()];
    let mut used = vec![false; side.entries.len()];
    let mut base_by_text = BTreeMap::<&str, Vec<usize>>::new();
    let mut side_by_text = BTreeMap::<&str, Vec<usize>>::new();
    for (id, entry) in base.entries.iter().enumerate() {
        base_by_text.entry(&entry.text).or_default().push(id);
    }
    for (id, entry) in side.entries.iter().enumerate() {
        side_by_text.entry(&entry.text).or_default().push(id);
    }
    for (text, base_ids) in base_by_text {
        let mut side_ids = side_by_text.remove(text).unwrap_or_default();
        // Match copies in their old semantic location first, then match remaining copies by file
        // order. The latter is sufficient for a move and keeps duplicate accounting bounded.
        for &base_id in &base_ids {
            if let Some(at) = side_ids.iter().position(|&side_id| {
                side.entries[side_id].location == base.entries[base_id].location
            }) {
                let side_id = side_ids.remove(at);
                used[side_id] = true;
                states[base_id] = SideState::Exact(side_id);
            }
        }
        let unmatched: Vec<usize> = base_ids
            .into_iter()
            .filter(|&id| matches!(states[id], SideState::Absent))
            .collect();
        for (base_id, side_id) in unmatched.into_iter().zip(side_ids) {
            used[side_id] = true;
            states[base_id] = SideState::Exact(side_id);
        }
    }

    // A bold lead is the stable identity used by the hand-written notes and their integrity
    // guard. It can therefore prove a rewrite even when that rewrite also moves subsection.
    let identities: BTreeSet<String> = base
        .entries
        .iter()
        .enumerate()
        .filter(|(id, _)| matches!(states[*id], SideState::Absent))
        .filter_map(|(_, entry)| entry_identity(&entry.text))
        .chain(
            side.entries
                .iter()
                .enumerate()
                .filter(|(id, _)| !used[*id])
                .filter_map(|(_, entry)| entry_identity(&entry.text)),
        )
        .collect();
    for identity in identities {
        let old: Vec<usize> = base
            .entries
            .iter()
            .enumerate()
            .filter(|(id, entry)| {
                matches!(states[*id], SideState::Absent)
                    && entry_identity(&entry.text).as_deref() == Some(identity.as_str())
            })
            .map(|(id, _)| id)
            .collect();
        let new: Vec<usize> = side
            .entries
            .iter()
            .enumerate()
            .filter(|(id, entry)| {
                !used[*id] && entry_identity(&entry.text).as_deref() == Some(identity.as_str())
            })
            .map(|(id, _)| id)
            .collect();
        if old.is_empty() || new.is_empty() {
            continue;
        }
        if let ([old], [new]) = (old.as_slice(), new.as_slice()) {
            states[*old] = SideState::Rewrite(*new);
            used[*new] = true;
            continue;
        }
        return Err(Conflict::new(
            format!("{which} has ambiguous copies of the titled entry {identity:?}"),
            old.iter()
                .map(|&id| base.entries[id].title.clone())
                .chain(new.iter().map(|&id| side.entries[id].title.clone())),
        ));
    }

    let mut locations = BTreeSet::new();
    for (id, entry) in base.entries.iter().enumerate() {
        if matches!(states[id], SideState::Absent) {
            locations.insert(entry.location.clone());
        }
    }
    for (id, entry) in side.entries.iter().enumerate() {
        if !used[id] {
            locations.insert(entry.location.clone());
        }
    }
    for location in locations {
        let old: Vec<usize> = base
            .entries
            .iter()
            .enumerate()
            .filter(|(id, entry)| {
                matches!(states[*id], SideState::Absent) && entry.location == location
            })
            .map(|(id, _)| id)
            .collect();
        let new: Vec<usize> = side
            .entries
            .iter()
            .enumerate()
            .filter(|(id, entry)| !used[*id] && entry.location == location)
            .map(|(id, _)| id)
            .collect();
        match (old.as_slice(), new.as_slice()) {
            ([old], [new]) => {
                states[*old] = SideState::Rewrite(*new);
                used[*new] = true;
            }
            ([], _) | (_, []) => {}
            _ => {
                let names = old
                    .iter()
                    .map(|&id| base.entries[id].title.clone())
                    .chain(new.iter().map(|&id| side.entries[id].title.clone()));
                return Err(Conflict::new(
                    format!(
                        "{which} has an ambiguous multi-entry replacement in [{}]{}",
                        location.section,
                        location
                            .kind
                            .as_ref()
                            .map_or_else(String::new, |kind| format!(" / ### {kind}"))
                    ),
                    names,
                ));
            }
        }
    }
    let additions = used
        .iter()
        .enumerate()
        .filter_map(|(id, used)| (!used).then_some(id))
        .collect();
    Ok(SideView { states, additions })
}

#[derive(Clone, Debug)]
struct ResultEntry {
    text: String,
    title: String,
    location: Location,
    base: Option<Location>,
    tip: Option<usize>,
    main: Option<usize>,
}

impl ResultEntry {
    fn addition(entry: &Entry, tip: Option<usize>, main: Option<usize>) -> Self {
        Self {
            text: entry.text.clone(),
            title: entry.title.clone(),
            location: entry.location.clone(),
            base: None,
            tip,
            main,
        }
    }
}

fn merge_old(
    old: &Entry,
    tip_state: SideState,
    tip: &Document,
    main_state: SideState,
    main: &Document,
) -> Result<Option<(String, String, Location)>, Conflict> {
    let tip_entry = tip_state.side().map(|side| &tip.entries[side]);
    let main_entry = main_state.side().map(|side| &main.entries[side]);
    let tip_moved = tip_entry.is_some_and(|entry| entry.location != old.location);
    let main_moved = main_entry.is_some_and(|entry| entry.location != old.location);
    match (tip_state, main_state) {
        (SideState::Absent, SideState::Absent) => return Ok(None),
        (SideState::Absent, SideState::Exact(_)) if !main_moved => return Ok(None),
        (SideState::Exact(_), SideState::Absent) if !tip_moved => return Ok(None),
        (SideState::Absent, SideState::Exact(_) | SideState::Rewrite(_))
        | (SideState::Exact(_) | SideState::Rewrite(_), SideState::Absent) => {
            let changed = tip_entry.or(main_entry).expect("one side holds the entry");
            return Err(Conflict::new(
                "one side removed an entry while the other rewrote or moved it",
                [old.title.clone(), changed.title.clone()],
            ));
        }
        _ => {}
    }
    let tip_entry = tip_entry.expect("present tip entry");
    let main_entry = main_entry.expect("present main entry");
    let tip_rewrite = matches!(tip_state, SideState::Rewrite(_));
    let main_rewrite = matches!(main_state, SideState::Rewrite(_));
    let chosen = match (tip_rewrite, main_rewrite) {
        (false, false) => old,
        (true, false) => tip_entry,
        (false, true) => main_entry,
        (true, true) if tip_entry.text == main_entry.text => tip_entry,
        (true, true) => {
            return Err(Conflict::new(
                "the branch and main rewrote one entry differently",
                [
                    old.title.clone(),
                    tip_entry.title.clone(),
                    main_entry.title.clone(),
                ],
            ));
        }
    };
    let location = scalar(
        &old.location,
        &tip_entry.location,
        &main_entry.location,
        "the branch and main moved one entry differently",
        [old.title.clone()],
    )?;
    Ok(Some((chosen.text.clone(), chosen.title.clone(), location)))
}

fn scalar<T: Clone + PartialEq>(
    base: &T,
    tip: &T,
    main: &T,
    reason: &str,
    entries: impl IntoIterator<Item = String>,
) -> Result<T, Conflict> {
    if tip == main {
        Ok(tip.clone())
    } else if tip == base {
        Ok(main.clone())
    } else if main == base {
        Ok(tip.clone())
    } else {
        Err(Conflict::new(reason, entries))
    }
}

fn section_presence(
    base: &Document,
    tip: &Document,
    main: &Document,
    frozen: &BTreeSet<String>,
    result: &[ResultEntry],
) -> Result<BTreeSet<String>, Conflict> {
    let keys: BTreeSet<String> = base
        .keys()
        .into_iter()
        .chain(tip.keys())
        .chain(main.keys())
        .collect();
    let mut present = BTreeSet::new();
    for key in keys {
        let held = if frozen.contains(&key) {
            true
        } else {
            scalar(
                &base.section(&key).is_some(),
                &tip.section(&key).is_some(),
                &main.section(&key).is_some(),
                &format!("the branch and main changed section [{key}] incompatibly"),
                Vec::new(),
            )?
        };
        if held {
            present.insert(key);
        }
    }
    for entry in result {
        if !present.contains(&entry.location.section) {
            return Err(Conflict::new(
                format!(
                    "an entry remains while section [{}] was removed",
                    entry.location.section
                ),
                [entry.title.clone()],
            ));
        }
    }
    Ok(present)
}

fn section_headers(
    base: &Document,
    tip: &Document,
    main: &Document,
    frozen: &BTreeSet<String>,
    present: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, Conflict> {
    let mut headers = BTreeMap::new();
    for key in present {
        if frozen.contains(key) {
            headers.insert(
                key.clone(),
                main.section(key)
                    .expect("frozen main section")
                    .header
                    .clone(),
            );
            continue;
        }
        let b = base.section(key).map(|section| section.header.as_str());
        let t = tip.section(key).map(|section| section.header.as_str());
        let m = main.section(key).map(|section| section.header.as_str());
        let header = match (b, t, m) {
            (Some(b), Some(t), Some(m)) => scalar(
                &b,
                &t,
                &m,
                &format!("the branch and main renamed section [{key}] differently"),
                Vec::new(),
            )?
            .to_owned(),
            (None, Some(t), None) => t.to_owned(),
            (None, None, Some(m)) => m.to_owned(),
            (None, Some(t), Some(m)) if t == m => t.to_owned(),
            (Some(_), Some(t), None) => t.to_owned(),
            (Some(_), None, Some(m)) => m.to_owned(),
            _ => {
                return Err(Conflict::new(
                    format!("section [{key}] has no unambiguous heading"),
                    Vec::new(),
                ));
            }
        };
        headers.insert(key.clone(), header);
    }
    Ok(headers)
}

fn merge_optional_raw(
    base: Option<&Section>,
    tip: Option<&Section>,
    main: Option<&Section>,
    key: &str,
) -> Result<String, Conflict> {
    match (base, tip, main) {
        (Some(base), Some(tip), Some(main)) => scalar(
            &base.raw,
            &tip.raw,
            &main.raw,
            &format!("both sides changed plain section {key}"),
            Vec::new(),
        ),
        (None, Some(tip), None) => Ok(tip.raw.clone()),
        (None, None, Some(main)) => Ok(main.raw.clone()),
        (None, Some(tip), Some(main)) if tip.raw == main.raw => Ok(tip.raw.clone()),
        (Some(_), Some(tip), None) => Ok(tip.raw.clone()),
        (Some(_), None, Some(main)) => Ok(main.raw.clone()),
        _ => Err(Conflict::new(
            format!("plain section {key} has no unambiguous contents"),
            Vec::new(),
        )),
    }
}

fn section_order(
    base: &Document,
    tip: &Document,
    main: &Document,
    present: &BTreeSet<String>,
) -> Result<Vec<String>, Conflict> {
    let base_order: Vec<String> = base
        .keys()
        .into_iter()
        .filter(|key| present.contains(key))
        .collect();
    let base_set: BTreeSet<String> = base_order.iter().cloned().collect();
    let old_order = |document: &Document| {
        document
            .keys()
            .into_iter()
            .filter(|key| present.contains(key) && base_set.contains(key))
            .collect::<Vec<_>>()
    };
    let tip_old = old_order(tip);
    let main_old = old_order(main);
    let order = changed_order(
        &base_order,
        &tip_old,
        &main_old,
        "the branch and main reordered changelog sections differently",
        base_order.iter().map(|key| format!("## [{key}]")),
    )?;
    let old_positions: BTreeMap<String, usize> = order
        .iter()
        .enumerate()
        .map(|(position, key)| (key.clone(), position))
        .collect();
    let tip_sequence: Vec<String> = tip
        .keys()
        .into_iter()
        .filter(|key| present.contains(key))
        .collect();
    let main_sequence: Vec<String> = main
        .keys()
        .into_iter()
        .filter(|key| present.contains(key))
        .collect();
    let new: Vec<String> = present
        .iter()
        .filter(|key| !base_set.contains(*key))
        .cloned()
        .collect();
    let gap_for = |sequence: &[String], key: &str| -> Result<usize, Conflict> {
        let at = sequence
            .iter()
            .position(|held| held == key)
            .expect("new section has a side source");
        let previous = sequence[..at]
            .iter()
            .rev()
            .find_map(|candidate| old_positions.get(candidate).copied());
        let next = sequence[at + 1..]
            .iter()
            .find_map(|candidate| old_positions.get(candidate).copied());
        match (previous, next) {
            (Some(previous), Some(next)) if previous < next => Ok(next),
            (Some(_), Some(_)) => Err(Conflict::new(
                "a new changelog section's anchors were reversed by the other side's reorder",
                [format!("## [{key}]")],
            )),
            (Some(previous), None) => Ok(previous + 1),
            (None, Some(next)) => Ok(next),
            (None, None) => Ok(order.len()),
        }
    };
    let mut gaps = BTreeMap::<String, usize>::new();
    for key in &new {
        let tip_gap = tip_sequence
            .contains(key)
            .then(|| gap_for(&tip_sequence, key))
            .transpose()?;
        let main_gap = main_sequence
            .contains(key)
            .then(|| gap_for(&main_sequence, key))
            .transpose()?;
        let gap = match (tip_gap, main_gap) {
            (Some(tip), Some(main)) if tip != main => {
                return Err(Conflict::new(
                    "the branch and main placed the same new changelog section differently",
                    [format!("## [{key}]")],
                ));
            }
            (Some(gap), _) | (_, Some(gap)) => gap,
            (None, None) => unreachable!("new section has a side source"),
        };
        gaps.insert(key.clone(), gap);
    }

    let mut by_gap = BTreeMap::<usize, Vec<String>>::new();
    for key in new {
        by_gap.entry(gaps[&key]).or_default().push(key);
    }
    let mut ordered_new = BTreeMap::<usize, Vec<String>>::new();
    for (gap, keys) in by_gap {
        let key_set: BTreeSet<String> = keys.iter().cloned().collect();
        let mut edges: BTreeMap<String, BTreeSet<String>> = keys
            .iter()
            .map(|key| (key.clone(), BTreeSet::new()))
            .collect();
        let mut indegree: BTreeMap<String, usize> =
            keys.iter().map(|key| (key.clone(), 0)).collect();
        for sequence in [&tip_sequence, &main_sequence] {
            let sequence: Vec<&String> = sequence
                .iter()
                .filter(|key| key_set.contains(*key) && gaps[*key] == gap)
                .collect();
            for pair in sequence.windows(2) {
                if edges
                    .get_mut(pair[0])
                    .expect("section edge")
                    .insert((*pair[1]).clone())
                {
                    *indegree.get_mut(pair[1]).expect("section indegree") += 1;
                }
            }
        }
        let mut remaining = key_set;
        let mut merged = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .filter(|key| indegree[*key] == 0)
                .min_by_key(|key| {
                    let main_at = main_sequence.iter().position(|held| held == *key);
                    let tip_at = tip_sequence.iter().position(|held| held == *key);
                    (
                        if main_at.is_some() { 0_u8 } else { 1_u8 },
                        main_at.or(tip_at).unwrap_or(usize::MAX),
                        (*key).clone(),
                    )
                })
                .cloned();
            let Some(next) = next else {
                return Err(Conflict::new(
                    "the branch and main ordered new changelog sections incompatibly",
                    remaining.iter().map(|key| format!("## [{key}]")),
                ));
            };
            remaining.remove(&next);
            for successor in &edges[&next] {
                *indegree.get_mut(successor).expect("section indegree") -= 1;
            }
            merged.push(next);
        }
        ordered_new.insert(gap, merged);
    }

    let mut merged = Vec::with_capacity(present.len());
    for gap in 0..=order.len() {
        merged.extend(ordered_new.remove(&gap).unwrap_or_default());
        if let Some(key) = order.get(gap) {
            merged.push(key.clone());
        }
    }
    debug_assert!(ordered_new.is_empty());
    Ok(merged)
}

fn merge_new_around_old<T: Clone + Ord>(
    old: &[T],
    tip: &[T],
    main: &[T],
    new: &BTreeSet<T>,
    noun: &str,
    name: impl Fn(&T) -> String,
) -> Result<Vec<T>, Conflict> {
    let old_positions: BTreeMap<T, usize> = old
        .iter()
        .enumerate()
        .map(|(position, value)| (value.clone(), position))
        .collect();
    let gap_for = |sequence: &[T], value: &T| -> Result<usize, Conflict> {
        let at = sequence
            .iter()
            .position(|held| held == value)
            .expect("new value has a side source");
        let previous = sequence[..at]
            .iter()
            .rev()
            .find_map(|candidate| old_positions.get(candidate).copied());
        let next = sequence[at + 1..]
            .iter()
            .find_map(|candidate| old_positions.get(candidate).copied());
        match (previous, next) {
            (Some(previous), Some(next)) if previous < next => Ok(next),
            (Some(_), Some(_)) => Err(Conflict::new(
                format!("a new {noun}'s anchors were reversed by the other side's reorder"),
                [name(value)],
            )),
            (Some(previous), None) => Ok(previous + 1),
            (None, Some(next)) => Ok(next),
            (None, None) => Ok(old.len()),
        }
    };
    let mut gaps = BTreeMap::<T, usize>::new();
    for value in new {
        let tip_gap = tip
            .contains(value)
            .then(|| gap_for(tip, value))
            .transpose()?;
        let main_gap = main
            .contains(value)
            .then(|| gap_for(main, value))
            .transpose()?;
        let gap = match (tip_gap, main_gap) {
            (Some(tip), Some(main)) if tip != main => {
                return Err(Conflict::new(
                    format!("the branch and main placed the same new {noun} differently"),
                    [name(value)],
                ));
            }
            (Some(gap), _) | (_, Some(gap)) => gap,
            (None, None) => unreachable!("new value has a side source"),
        };
        gaps.insert(value.clone(), gap);
    }

    let mut by_gap = BTreeMap::<usize, Vec<T>>::new();
    for value in new {
        by_gap.entry(gaps[value]).or_default().push(value.clone());
    }
    let mut ordered_new = BTreeMap::<usize, Vec<T>>::new();
    for (gap, values) in by_gap {
        let value_set: BTreeSet<T> = values.iter().cloned().collect();
        let mut edges: BTreeMap<T, BTreeSet<T>> = values
            .iter()
            .map(|value| (value.clone(), BTreeSet::new()))
            .collect();
        let mut indegree: BTreeMap<T, usize> =
            values.iter().map(|value| (value.clone(), 0)).collect();
        for sequence in [tip, main] {
            let sequence: Vec<&T> = sequence
                .iter()
                .filter(|value| value_set.contains(*value) && gaps[*value] == gap)
                .collect();
            for pair in sequence.windows(2) {
                if edges
                    .get_mut(pair[0])
                    .expect("new-value edge")
                    .insert((*pair[1]).clone())
                {
                    *indegree.get_mut(pair[1]).expect("new-value indegree") += 1;
                }
            }
        }
        let mut remaining = value_set;
        let mut merged = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .filter(|value| indegree[*value] == 0)
                .min_by_key(|value| {
                    let main_at = main.iter().position(|held| held == *value);
                    let tip_at = tip.iter().position(|held| held == *value);
                    (
                        if main_at.is_some() { 0_u8 } else { 1_u8 },
                        main_at.or(tip_at).unwrap_or(usize::MAX),
                        (*value).clone(),
                    )
                })
                .cloned();
            let Some(next) = next else {
                return Err(Conflict::new(
                    format!("the branch and main ordered new {noun}s incompatibly"),
                    remaining.iter().map(&name),
                ));
            };
            remaining.remove(&next);
            for successor in &edges[&next] {
                *indegree.get_mut(successor).expect("new-value indegree") -= 1;
            }
            merged.push(next);
        }
        ordered_new.insert(gap, merged);
    }

    let mut merged = Vec::with_capacity(old.len() + new.len());
    for gap in 0..=old.len() {
        merged.extend(ordered_new.remove(&gap).unwrap_or_default());
        if let Some(value) = old.get(gap) {
            merged.push(value.clone());
        }
    }
    debug_assert!(ordered_new.is_empty());
    Ok(merged)
}

fn kind_order(
    section: &str,
    base: &Document,
    tip: &Document,
    main: &Document,
    result: &[ResultEntry],
) -> Result<Vec<Option<String>>, Conflict> {
    let wanted: BTreeSet<Option<String>> = result
        .iter()
        .filter(|entry| entry.location.section == section)
        .map(|entry| entry.location.kind.clone())
        .collect();
    // A kind is inherited only while a base-derived entry still occupies that same location.
    // If its last old entry was removed and only a concurrent addition keeps the heading alive,
    // the heading is new for ordering and takes its anchors from the side that added the entry.
    let inherited: BTreeSet<Option<String>> = result
        .iter()
        .filter(|entry| entry.location.section == section)
        .filter(|entry| entry.base.as_ref() == Some(&entry.location))
        .map(|entry| entry.location.kind.clone())
        .collect();
    let sequence = |document: &Document| {
        document.section(section).map_or_else(Vec::new, |section| {
            section
                .kinds
                .iter()
                .filter(|kind| wanted.contains(*kind))
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    let b: Vec<Option<String>> = sequence(base)
        .into_iter()
        .filter(|kind| inherited.contains(kind))
        .collect();
    let t = sequence(tip);
    let m = sequence(main);
    let tip_old: Vec<Option<String>> = t
        .iter()
        .filter(|kind| inherited.contains(*kind))
        .cloned()
        .collect();
    let main_old: Vec<Option<String>> = m
        .iter()
        .filter(|kind| inherited.contains(*kind))
        .cloned()
        .collect();
    let order = changed_order(
        &b,
        &tip_old,
        &main_old,
        &format!("the branch and main reordered subsections in [{section}] differently"),
        b.iter().map(|kind| {
            kind.as_ref().map_or_else(
                || "entries without a subsection".to_owned(),
                |k| format!("### {k}"),
            )
        }),
    )?;
    let new: BTreeSet<Option<String>> = wanted.difference(&inherited).cloned().collect();
    merge_new_around_old(
        &order,
        &t,
        &m,
        &new,
        &format!("subsection in [{section}]"),
        |kind| {
            kind.as_ref().map_or_else(
                || "entries without a subsection".to_owned(),
                |kind| format!("### {kind}"),
            )
        },
    )
}

fn changed_order<T: Clone + Eq>(
    base: &[T],
    tip: &[T],
    main: &[T],
    reason: &str,
    names: impl IntoIterator<Item = String>,
) -> Result<Vec<T>, Conflict> {
    if tip == main {
        Ok(tip.to_vec())
    } else if tip == base {
        Ok(main.to_vec())
    } else if main == base {
        Ok(tip.to_vec())
    } else {
        Err(Conflict::new(reason, names))
    }
}

#[allow(clippy::too_many_arguments)]
fn entry_order(
    location: &Location,
    nodes: &[usize],
    base: &Document,
    tip: &Document,
    main: &Document,
    base_to_node: &[Option<usize>],
    tip_to_node: &[Option<usize>],
    main_to_node: &[Option<usize>],
    result: &[ResultEntry],
) -> Result<Vec<usize>, Conflict> {
    let wanted: BTreeSet<usize> = nodes.iter().copied().collect();
    let inherited: Vec<usize> = base
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.location == *location)
        .filter_map(|(id, _)| base_to_node[id])
        .filter(|node| wanted.contains(node))
        .collect();
    let inherited_set: BTreeSet<usize> = inherited.iter().copied().collect();
    let side_sequence = |document: &Document, mapping: &[Option<usize>]| {
        document
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.location == *location)
            .filter_map(|(id, _)| mapping[id])
            .filter(|node| wanted.contains(node))
            .collect::<Vec<_>>()
    };
    let tip_sequence = side_sequence(tip, tip_to_node);
    let main_sequence = side_sequence(main, main_to_node);
    let tip_old: Vec<usize> = tip_sequence
        .iter()
        .copied()
        .filter(|node| inherited_set.contains(node))
        .collect();
    let main_old: Vec<usize> = main_sequence
        .iter()
        .copied()
        .filter(|node| inherited_set.contains(node))
        .collect();
    let old = changed_order(
        &inherited,
        &tip_old,
        &main_old,
        &format!(
            "the branch and main reordered entries in [{}]{} differently",
            location.section,
            location
                .kind
                .as_ref()
                .map_or_else(String::new, |kind| format!(" / ### {kind}"))
        ),
        inherited.iter().map(|node| result[*node].title.clone()),
    )?;

    let old_positions: BTreeMap<usize, usize> = old
        .iter()
        .enumerate()
        .map(|(position, node)| (*node, position))
        .collect();
    let gap_for = |sequence: &[usize], node: usize| -> Result<(usize, usize), Conflict> {
        let side_at = sequence
            .iter()
            .position(|held| *held == node)
            .expect("result addition has a side source");
        let previous = sequence[..side_at]
            .iter()
            .rev()
            .find_map(|candidate| old_positions.get(candidate).copied());
        let next = sequence[side_at + 1..]
            .iter()
            .find_map(|candidate| old_positions.get(candidate).copied());
        let gap = match (previous, next) {
            (Some(previous), Some(next)) if previous < next => next,
            (Some(_), Some(_)) => {
                return Err(Conflict::new(
                    "an added entry's anchors were reversed by the other side's reorder",
                    [result[node].title.clone()],
                ));
            }
            (Some(previous), None) => previous + 1,
            (None, Some(next)) => next,
            (None, None) => old.len(),
        };
        Ok((gap, side_at))
    };

    let additions: Vec<usize> = nodes
        .iter()
        .copied()
        .filter(|node| !inherited_set.contains(node))
        .collect();
    let mut gaps = BTreeMap::<usize, usize>::new();
    for &node in &additions {
        let tip_place = tip_sequence
            .contains(&node)
            .then(|| gap_for(&tip_sequence, node))
            .transpose()?;
        let main_place = main_sequence
            .contains(&node)
            .then(|| gap_for(&main_sequence, node))
            .transpose()?;
        let gap = match (tip_place, main_place) {
            (Some((tip_gap, _)), Some((main_gap, _))) if tip_gap != main_gap => {
                return Err(Conflict::new(
                    "the branch and main placed the same added entry at different anchors",
                    [result[node].title.clone()],
                ));
            }
            (Some((gap, _)), _) | (_, Some((gap, _))) => gap,
            (None, None) => unreachable!("result addition has a side source"),
        };
        gaps.insert(node, gap);
    }

    let mut by_gap = BTreeMap::<usize, Vec<usize>>::new();
    for node in additions {
        by_gap.entry(gaps[&node]).or_default().push(node);
    }

    let mut ordered_additions = BTreeMap::<usize, Vec<usize>>::new();
    for (gap, additions) in by_gap {
        let additions_set: BTreeSet<usize> = additions.iter().copied().collect();
        let mut edges: BTreeMap<usize, BTreeSet<usize>> = additions
            .iter()
            .map(|&node| (node, BTreeSet::new()))
            .collect();
        let mut indegree: BTreeMap<usize, usize> =
            additions.iter().map(|&node| (node, 0)).collect();
        for sequence in [&tip_sequence, &main_sequence] {
            let sequence: Vec<usize> = sequence
                .iter()
                .copied()
                .filter(|node| additions_set.contains(node) && gaps[node] == gap)
                .collect();
            for pair in sequence.windows(2) {
                if edges
                    .get_mut(&pair[0])
                    .expect("addition edge")
                    .insert(pair[1])
                {
                    *indegree.get_mut(&pair[1]).expect("addition indegree") += 1;
                }
            }
        }

        let mut remaining = additions_set;
        let mut ordered = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .copied()
                .filter(|node| indegree[node] == 0)
                .min_by_key(|node| {
                    let main_at = main_sequence.iter().position(|held| held == node);
                    let tip_at = tip_sequence.iter().position(|held| held == node);
                    (
                        if main_at.is_some() { 0_u8 } else { 1_u8 },
                        main_at.or(tip_at).unwrap_or(usize::MAX),
                        *node,
                    )
                });
            let Some(next) = next else {
                return Err(Conflict::new(
                    "the branch and main ordered added entries incompatibly",
                    remaining.iter().map(|node| result[*node].title.clone()),
                ));
            };
            remaining.remove(&next);
            ordered.push(next);
            for successor in &edges[&next] {
                *indegree.get_mut(successor).expect("successor indegree") -= 1;
            }
        }
        ordered_additions.insert(gap, ordered);
    }

    for &node in nodes {
        debug_assert!(inherited_set.contains(&node) || gaps.contains_key(&node));
    }
    let mut merged = Vec::with_capacity(nodes.len());
    for gap in 0..=old.len() {
        merged.extend(ordered_additions.remove(&gap).unwrap_or_default());
        if let Some(&node) = old.get(gap) {
            merged.push(node);
        }
    }
    debug_assert!(ordered_additions.is_empty());
    Ok(merged)
}

fn section_result_matches(
    section: &Section,
    entries: &[Entry],
    header: &str,
    groups: &[(Option<String>, Vec<usize>)],
    result: &[ResultEntry],
) -> bool {
    section.header == header
        && section.kinds.iter().eq(groups.iter().map(|(kind, _)| kind))
        && section
            .raw
            .lines()
            .filter_map(kind_heading)
            .eq(groups.iter().filter_map(|(kind, _)| kind.as_deref()))
        && section
            .entries
            .iter()
            .map(|&node| (&entries[node].location.kind, &entries[node].text))
            .eq(groups.iter().flat_map(|(kind, nodes)| {
                nodes.iter().map(move |&node| (kind, &result[node].text))
            }))
}

fn render(
    header: &str,
    groups: &[(Option<String>, Vec<usize>)],
    result: &[ResultEntry],
    last: bool,
) -> String {
    let mut out = header.to_owned();
    for (kind, nodes) in groups {
        out.push_str("\n\n");
        if let Some(kind) = kind {
            out.push_str("### ");
            out.push_str(kind);
            out.push_str("\n\n");
        }
        for (n, node) in nodes.iter().enumerate() {
            if n > 0 {
                out.push_str("\n\n");
            }
            out.push_str(result[*node].text.trim_end_matches('\n'));
        }
    }
    out.push('\n');
    if !last {
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "land_changelog_tests.rs"]
mod tests;
