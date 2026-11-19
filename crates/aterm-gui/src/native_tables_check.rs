// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's Check view (docs/PLAN.md LT-28; docs/DESIGN-tables-app-2026-11-14.md
//! §3.5): every finding `aterm db table check` and `aterm db alab check` report of the registry,
//! each said in a person's words by the one function those verbs' `--tsv` form says it by
//! (`aterm_db::typed::check::said`), with the way to each row it names; and under them the
//! store's state: whether this machine can write and why not, whether its history is complete
//! here, its records and bytes against the store's caps, this machine's records against its writer
//! cap and its automatic-record quota, the refused and inert records by id, the generation, how
//! many machines have written, the roll line where check asks for a roll, and who holds its
//! checkpoints back: whether this machine may sign one and, of the enrolled machines and of
//! those that wrote a record in the last 30 days, how many hold them back for each reason.
//! Those three sentences wrap, so each is whole at any width.
//!
//! Check is a view a person opens from the rail (or, under 900 px, from the table picker): it
//! raises no alert, badge or banner, the rail shows no count of it, and its findings are counted
//! inside the view. The reader works it out only while a view shows it
//! ([`crate::native_tables::TablesApp`]'s `checking`), so a page that never opens Check spends
//! nothing on it. The page keeps no list of finding kinds: a kind this build writes no sentence
//! for is said by its word.

use std::cell::Cell;

use crate::native_tables::{
    FixedHeight, TablesApp, TablesViewState, button, fitted, group, press, row_height, scale,
    styled,
};
use crate::native_ui::{
    Insets, Layout, Length, SemanticRole, StyleRef, TextSpec, UiContent, UiNode,
};
use crate::native_workers::{one_line, text_width};

/// The rail's line (and the picker's option) that opens Check.
pub(crate) const OPEN: &str = "tables/view/check";
/// A finding's line in the list: a press selects it.
const FINDING: &str = "tables/check/finding/";
/// A row a finding names: a press shows it in its table, selected.
const ROW: &str = "tables/check/row/";

/// One finding as the view says it ([`aterm_db::att::command::Line`], read on the page's
/// thread): its word, what it means, the table and rows it names, its columns, and what else it
/// says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) kind: String,
    pub(crate) said: String,
    pub(crate) collection: String,
    pub(crate) pks: Vec<String>,
    pub(crate) columns: Vec<String>,
    pub(crate) detail: String,
}

/// The registry's store as one read found it, under the findings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Store {
    /// Why this machine cannot write the registry, where it cannot.
    pub(crate) cannot_write: Option<String>,
    /// Whether no record held here still waits on an earlier one, and how many do.
    pub(crate) complete: bool,
    pub(crate) awaiting: usize,
    /// The records and bytes the registry holds here, and the store's caps on them.
    pub(crate) records: usize,
    pub(crate) records_cap: usize,
    pub(crate) bytes: usize,
    pub(crate) bytes_cap: usize,
    /// This machine's own records, and the cap on one writer's.
    pub(crate) mine: usize,
    pub(crate) mine_cap: usize,
    /// This machine's automatic records against the table's quota on them.
    pub(crate) quota_spent: usize,
    pub(crate) quota: usize,
    /// The records this build never projects (a newer build's, a cut-off writer's), by id.
    pub(crate) refused: Vec<String>,
    /// Automatic records past their writer's quota, by id, and an owned row's inert records,
    /// which have none to show.
    pub(crate) inert: Vec<String>,
    pub(crate) inert_rows: usize,
    /// The generation in use: its number and its table's name.
    pub(crate) generation: u32,
    pub(crate) table: String,
    /// How many machines have written the registry.
    pub(crate) writers: usize,
    /// What the roll finding says, where check asks for a roll.
    pub(crate) roll: Option<String>,
    /// Who holds the registry's checkpoints back, a sentence each as `aterm db table check
    /// --fence` says it (`aterm_db::att::command::fence_lines`): whether this machine may sign
    /// one, the enrolled machines, and those that wrote a record in the last 30 days.
    pub(crate) fence: Vec<String>,
}

/// The keys of the fence's sentences, in their order ([`Store::fence`]). The list wraps the
/// lines under these keys, which say counts at their ends; every other line is cut to its width.
pub(crate) const FENCE: [&str; 3] = ["fence", "fence-roster", "fence-recent"];

/// What Check shows: every finding, and the store's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Checked {
    pub(crate) findings: Vec<Found>,
    pub(crate) store: Store,
}

/// The view's own state: whether it is open, the finding selected, and its list's scroll.
#[derive(Default)]
pub(crate) struct CheckView {
    pub(crate) open: bool,
    pub(crate) finding: Option<usize>,
    pub(crate) scroll: f32,
    pub(crate) limit: Cell<f32>,
}

/// `1,234`: a count with its thousands apart.
fn count_words(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `1.4 MB`, `128 MB`: bytes in the largest unit (of 1,024) that leaves at least one, to a
/// tenth where it is not whole.
fn bytes_words(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else if value.fract() == 0.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The ids of records, each by its first 8 hex.
fn ids_words(ids: &[String]) -> String {
    ids.iter()
        .map(|id| id.get(..8).unwrap_or(id))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Store {
    /// The store's state, a line each, by the key its line carries: plain sentences, no alert.
    pub(crate) fn lines(&self) -> Vec<(&'static str, String)> {
        let mut lines = vec![
            (
                "write",
                match &self.cannot_write {
                    None => "This machine can write the registry".to_owned(),
                    Some(why) => format!("This machine cannot write the registry: {why}"),
                },
            ),
            (
                "history",
                if self.complete {
                    "Its history is complete here".to_owned()
                } else {
                    format!(
                        "Its history is not complete here: {} waiting on an earlier one",
                        match self.awaiting {
                            1 => "1 record is".to_owned(),
                            n => format!("{} records are", count_words(n)),
                        }
                    )
                },
            ),
            (
                "records",
                format!(
                    "Records: {} of {}",
                    count_words(self.records),
                    count_words(self.records_cap)
                ),
            ),
            (
                "bytes",
                format!(
                    "Bytes: {} of {}",
                    bytes_words(self.bytes),
                    bytes_words(self.bytes_cap)
                ),
            ),
            (
                "mine",
                format!(
                    "This machine's records: {} of {} one writer may hold",
                    count_words(self.mine),
                    count_words(self.mine_cap)
                ),
            ),
            (
                "quota",
                format!(
                    "This machine's automatic records: {} of its quota of {}",
                    count_words(self.quota_spent),
                    count_words(self.quota)
                ),
            ),
            (
                "refused",
                match self.refused.len() {
                    0 => "Refused records: none".to_owned(),
                    n => format!("Refused records: {n}: {}", ids_words(&self.refused)),
                },
            ),
            (
                "inert",
                match (self.inert.len(), self.inert_rows) {
                    (0, 0) => "Inert records: none".to_owned(),
                    (n, 0) => format!("Inert records: {n}: {}", ids_words(&self.inert)),
                    (0, rows) => format!("Inert records: {rows} of the machines' own rows"),
                    (n, rows) => format!(
                        "Inert records: {}: {}, and {rows} of the machines' own rows",
                        n + rows,
                        ids_words(&self.inert)
                    ),
                },
            ),
            (
                "generation",
                format!("Generation {} ({})", self.generation, self.table),
            ),
            (
                "writers",
                match self.writers {
                    1 => "1 machine has written it".to_owned(),
                    n => format!("{n} machines have written it"),
                },
            ),
        ];
        lines.push((
            "roll",
            match &self.roll {
                None => "Check asks for no roll".to_owned(),
                Some(why) => format!("Check asks for a roll: {why}"),
            },
        ));
        lines.extend(FENCE.into_iter().zip(self.fence.iter().cloned()));
        lines
    }
}

/// The sheet of the page that holds row `pk`, the finding's own table first: a finding of one
/// table may name a row of another (a machine's facts and the machine row they disagree with).
/// `None` where no sheet read holds it.
pub(crate) fn sheet_of(app: &TablesApp, found: &Found, pk: &str) -> Option<&'static str> {
    let holds = |name: &str| {
        app.sheet_named(name)
            .is_some_and(|sheet| sheet.rows.iter().any(|row| row.pk == pk))
    };
    let listed: Vec<&'static str> = app.listed_names();
    listed
        .iter()
        .copied()
        .find(|name| *name == found.collection && holds(name))
        .or_else(|| listed.iter().copied().find(|name| holds(name)))
}

/// A press of a Check control (`key`); `None` where `key` is none of Check's.
pub(crate) fn act(app: &mut TablesApp, view: &mut TablesViewState, key: &str) -> Option<bool> {
    if key == OPEN {
        view.close_edit(app, false);
        view.picking = false;
        if !view.check.open {
            view.check.open = true;
            view.check.scroll = 0.0;
        }
        app.check(true);
        view.rail_reveal(OPEN);
        view.rest_choice(OPEN);
        return Some(true);
    }
    if let Some(n) = key.strip_prefix(FINDING) {
        let n = n.parse::<usize>().ok()?;
        view.check.finding = Some(n);
        view.pane_scroll = 0.0;
        view.focus(key);
        return Some(true);
    }
    if let Some(rest) = key.strip_prefix(ROW) {
        let (n, m) = rest.split_once('/')?;
        let (n, m) = (n.parse::<usize>().ok()?, m.parse::<usize>().ok()?);
        let found = app.checked()?.findings.get(n)?.clone();
        let pk = found.pks.get(m)?.clone();
        let Some(name) = sheet_of(app, &found, &pk) else {
            return Some(true);
        };
        view.check.open = false;
        app.check(false);
        view.show_row(app, name, &pk, found.columns.first().map(String::as_str));
        return Some(true);
    }
    None
}

/// Each row `found` names, by the sheet that holds it and the words it is known by
/// ([`TablesApp::row_words`]; its key where no sheet read holds it), told apart ([`apart`]).
fn named_rows(app: &TablesApp, found: &Found) -> Vec<(Option<&'static str>, String)> {
    let rows = found
        .pks
        .iter()
        .map(|pk| {
            let sheet = sheet_of(app, found, pk);
            let words = sheet
                .and_then(|name| app.row_words(name, pk))
                .unwrap_or_else(|| pk.clone());
            (sheet, words)
        })
        .collect();
    apart(rows, &found.pks)
}

/// `rows` (each row's sheet and words, the row keyed `pks[n]` at `n`) with each one's key beside
/// its words where another row of its sheet reads the same (two enrollments of one machine,
/// each `m1 (<its key>)`), so a finding about two rows never reads as one.
pub(crate) fn apart(
    mut rows: Vec<(Option<&'static str>, String)>,
    pks: &[String],
) -> Vec<(Option<&'static str>, String)> {
    let same: Vec<bool> = rows
        .iter()
        .enumerate()
        .map(|(n, row)| {
            rows.iter()
                .enumerate()
                .any(|(m, other)| m != n && other == row)
        })
        .collect();
    for ((row, pk), same) in rows.iter_mut().zip(pks).zip(same) {
        if same && row.1 != *pk {
            row.1 = format!("{} ({pk})", row.1);
        }
    }
    rows
}

/// Where a finding stands, in words: its table's title, its rows by their words (a row of
/// another table with that table's title) and its columns.
fn where_words(app: &TablesApp, found: &Found) -> String {
    let title = app
        .title_of(&found.collection)
        .map_or_else(|| found.collection.clone(), str::to_owned);
    let mut parts = Vec::new();
    if !title.is_empty() {
        parts.push(title);
    }
    if !found.pks.is_empty() {
        let rows: Vec<String> = named_rows(app, found)
            .into_iter()
            .map(|(sheet, words)| match sheet {
                Some(name) if name != found.collection => match app.title_of(name) {
                    Some(title) => format!("{words} ({title})"),
                    None => words,
                },
                _ => words,
            })
            .collect();
        parts.push(rows.join(", "));
    }
    if !found.columns.is_empty() {
        parts.push(found.columns.join(", "));
    }
    parts.join(" \u{b7} ")
}

/// The list: every finding a line, the selected one banded, then the store's state. Its height
/// is `height`; it scrolls where it is taller.
pub(crate) fn list(app: &TablesApp, view: &TablesViewState, width: f32, height: f32) -> UiNode {
    let line = row_height();
    let note = 22.0 * scale().max(1.0);
    let mut children = Vec::new();
    let mut tall = 0.0_f32;
    let mut push = |children: &mut Vec<UiNode>, node: UiNode, h: f32| {
        children.push(node.layout_height(h));
        tall += h + 2.0;
    };
    match app.checked_read() {
        None => push(
            &mut children,
            fitted(
                "tables/check/reading",
                "Reading\u{2026}",
                StyleRef::Note,
                width,
            ),
            note,
        ),
        Some(Err(why)) => push(
            &mut children,
            crate::native_about::wrapped(
                "tables/check/unread",
                &format!("Check could not be read here: {why}"),
                width,
                SemanticRole::Text,
                StyleRef::Note,
            ),
            note * 2.0,
        ),
        Some(Ok(checked)) => {
            push(
                &mut children,
                fitted("tables/check/findings", "FINDINGS", StyleRef::Note, width),
                note,
            );
            if checked.findings.is_empty() {
                push(
                    &mut children,
                    fitted(
                        "tables/check/none",
                        "Check finds nothing",
                        StyleRef::Plain,
                        width,
                    ),
                    line,
                );
            }
            for (n, found) in checked.findings.iter().enumerate() {
                let key = format!("{FINDING}{n}");
                let place = where_words(app, found);
                let chosen = view.check.finding == Some(n);
                let said_w = (width - 20.0) * 0.6;
                let face = group(format!("{key}/face"))
                    .layout(
                        Layout::row()
                            .gap(8.0)
                            .padding(Insets::symmetric(10.0, 0.0))
                            .height(Length::Fixed(line)),
                    )
                    .children(vec![
                        fitted(format!("{key}/said"), &found.said, StyleRef::Plain, said_w),
                        fitted(
                            format!("{key}/where"),
                            &place,
                            StyleRef::Note,
                            (width - 28.0 - said_w).max(1.0),
                        ),
                    ])
                    .paint_only_tree();
                let mut seat = Vec::new();
                if chosen {
                    seat.push(styled(format!("{key}/band"), StyleRef::Code));
                }
                seat.push(press(&key, format!("{}: {place}", found.said)));
                seat.push(face);
                push(
                    &mut children,
                    group(format!("{key}/seat"))
                        .layout(Layout::default().height(Length::Fixed(line)))
                        .children(seat),
                    line,
                );
            }
            push(
                &mut children,
                fitted("tables/check/store", "THE STORE", StyleRef::Note, width),
                note + 8.0,
            );
            for (key, words) in checked.store.lines() {
                let node_key = format!("tables/check/store/{key}");
                if FENCE.contains(&key) {
                    let mut said = crate::native_about::wrapped(
                        &node_key,
                        &words,
                        width - 20.0,
                        SemanticRole::Text,
                        StyleRef::Plain,
                    );
                    let tall = match said.layout.height {
                        Length::Fixed(tall) => tall.max(note),
                        _ => note,
                    };
                    said.layout = said.layout.padding(Insets::symmetric(10.0, 0.0));
                    push(&mut children, said, tall);
                    continue;
                }
                push(
                    &mut children,
                    fitted(node_key, &words, StyleRef::Plain, width - 20.0)
                        .layout(Layout::default().padding(Insets::symmetric(10.0, 0.0))),
                    note,
                );
            }
        }
    }
    let limit = (tall - height).max(0.0);
    view.check.limit.set(limit);
    group("tables/check/list")
        .layout(
            Layout::column()
                .gap(2.0)
                .width(Length::Fixed(width))
                .height(Length::Fixed(height))
                .clipped()
                .scrolled(view.check.scroll.min(limit)),
        )
        .room(height, limit > 0.0)
        .lists_every_row()
        .children(children)
}

/// The selected finding, whole: its heading and the word `check --tsv` says it by, what it
/// means, where it is, each row it names by its words with the way to it, and what else it says.
/// Beside the list where the page is wide, below it where it is narrow.
pub(crate) fn detail(app: &TablesApp, view: &TablesViewState, width: f32) -> Vec<(UiNode, f32)> {
    let inner = width - 28.0;
    let line = 22.0 * scale().max(1.0);
    let mut out = Vec::new();
    let found = view
        .check
        .finding
        .and_then(|n| Some((n, app.checked()?.findings.get(n)?)));
    let Some((n, found)) = found else {
        out.push((
            fitted(
                "tables/pane/none",
                "Select a finding to see the rows it names",
                StyleRef::Note,
                inner,
            ),
            line,
        ));
        return out;
    };
    let title = crate::native_about::wrapped(
        "tables/pane/title",
        &aterm_db::typed::check::headed(&found.kind),
        inner,
        SemanticRole::Heading,
        StyleRef::Plain,
    );
    let title_h = match title.layout.height {
        Length::Fixed(h) => h,
        _ => line,
    };
    out.push((title, title_h));
    out.push((
        fitted("tables/pane/kind", &found.kind, StyleRef::FaintNote, inner),
        line,
    ));
    let said = crate::native_about::wrapped(
        "tables/pane/said",
        &found.said,
        inner,
        SemanticRole::Text,
        StyleRef::Plain,
    );
    let said_h = match said.layout.height {
        Length::Fixed(h) => h,
        _ => line,
    };
    out.push((said, said_h));
    if !found.collection.is_empty() {
        let title = app
            .title_of(&found.collection)
            .map_or_else(|| found.collection.clone(), str::to_owned);
        out.push((
            fitted(
                "tables/pane/table",
                &format!("Table: {title}"),
                StyleRef::Note,
                inner,
            ),
            line,
        ));
    }
    if !found.columns.is_empty() {
        out.push((
            fitted(
                "tables/pane/columns",
                &format!("Columns: {}", found.columns.join(", ")),
                StyleRef::Note,
                inner,
            ),
            line,
        ));
    }
    if !found.detail.is_empty() {
        let detail = crate::native_about::wrapped(
            "tables/pane/detail",
            &found.detail,
            inner,
            SemanticRole::Text,
            StyleRef::Note,
        );
        let h = match detail.layout.height {
            Length::Fixed(h) => h,
            _ => line,
        };
        out.push((detail, h));
    }
    let show_w = crate::native_ui::native_button_label_width("Show row") + 24.0;
    for (m, (sheet, words)) in named_rows(app, found).into_iter().enumerate() {
        let key = format!("{ROW}{n}/{m}");
        let title = sheet.and_then(|name| app.title_of(name)).unwrap_or("");
        let shown = if title.is_empty() {
            words.clone()
        } else {
            format!("{words} \u{b7} {title}")
        };
        out.push((
            group(format!("tables/pane/row/{m}"))
                .layout(Layout::row().gap(8.0))
                .children(vec![
                    fitted(
                        format!("tables/pane/row/{m}/key"),
                        &shown,
                        StyleRef::Plain,
                        (inner - show_w - 8.0).max(1.0),
                    ),
                    button(
                        &key,
                        "Show row",
                        &format!(
                            "Show row {words} in {}",
                            if title.is_empty() { "its table" } else { title }
                        ),
                        StyleRef::Secondary,
                        sheet.is_some(),
                    ),
                ]),
            line + 6.0,
        ));
    }
    out
}

/// The view's heading: `Check` (or, under 900 px, the table picker `title` in its place), and
/// how many findings it lists.
pub(crate) fn heading(app: &TablesApp, width: f32, title: Option<UiNode>) -> UiNode {
    let count = app
        .checked()
        .map_or_else(String::new, |checked| match checked.findings.len() {
            0 => "no findings".to_owned(),
            1 => "1 finding".to_owned(),
            n => format!("{n} findings"),
        });
    let count_w = text_width(&count, SemanticRole::Text, StyleRef::Note).min(width * 0.4);
    group("tables/header")
        .layout(
            Layout::row()
                .gap(12.0)
                .height(Length::Fixed(36.0 * scale().max(1.0))),
        )
        .children(vec![
            title.unwrap_or_else(|| {
                UiNode::new(
                    "tables/title",
                    UiContent::Text(TextSpec::heading(one_line(
                        "Check",
                        SemanticRole::Heading,
                        StyleRef::Plain,
                        width - count_w - 12.0,
                    ))),
                )
                .layout(Layout::default().width(Length::Fill))
            }),
            fitted("tables/count", &count, StyleRef::Note, count_w),
        ])
}

#[cfg(all(test, unix))]
#[path = "native_tables_check_tests.rs"]
mod native_tables_check_tests;
