// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Labels a table claims, said before Write (docs/PLAN.md LT-46).
//!
//! Where a table claims its labels ([`ClaimRule::Label`], the machines' `label`), one live row
//! keeps a label and every other row written under it reads as a clash label of its own
//! (`m22-3f9a`). A Write takes a label another row already reads as, so the staged cell says so
//! before the Write, never after: it wears [`IN_USE`] in the note's ink (a claim is no refusal,
//! and the Write still takes it), and the cell bar, the cell's spoken name and its `NOT WRITTEN`
//! line in the history drawer say, after their visible words, that the label already names the
//! other row (by its words and its key's ending, the pane a press away from it) and what this
//! row reads as once written.
//!
//! What a row will read as is never worked out here: the read keeps how each claimed column settled
//! ([`aterm_db::typed::claim::Settlement`], in the sheet's [`super::Schema`]), and the page asks it
//! what the staged rows read as once one Write of them lands on that read, by the read's own
//! settling of the rows the Write leaves, each in the record the save appends it in
//! ([`aterm_db::att::Compiled::appended`]): a later record observes an earlier one, so its row
//! yields a label to the earlier's. While something staged in the sheet is refused, neither the
//! records nor a new row's key is known yet (nor is anything written), so a label two rows of the
//! Write contend for is said to keep or read under a clash label, as the Write settles it. Two rows
//! given one label are each said, as is a row given a label another row reads as only as its clash
//! label; a row the same Write gives another label, clears or retires lets the label go, and
//! nothing is said of it. Each read and each change staged asks again
//! ([`super::TablesApp::recheck`]), so the note goes as soon as the label changes, the other row
//! lets the label go, or the Write settles it. A cell its rules refuse says that, and nothing of a
//! claim: the claims are worked out apart from what refuses a cell, and each reader leaves a
//! refused cell's out ([`of`]), since a Write's refusal comes and goes with neither the read nor
//! the staged cells moving. No dialog, confirmation, alert or count says it.

use std::collections::BTreeMap;

use aterm_db::typed::ClaimRule;
use aterm_db::typed::claim::{Reading, Staged};

use super::{At, Sheet, SheetRow, TablesApp, TablesViewState, fitted, group};
use crate::native_ui::{Layout, Length, StyleRef, UiNode};

/// The word a staged label another row claims wears in its cell, and leads its line with.
pub(crate) const IN_USE: &str = "in use";

/// What the key of the pane's press to the row a staged label is in use by starts with; its
/// column ends it.
const TO: &str = "tables/pane/row/claim/";

/// The record of one Write of each sheet that carries each cell it writes, by sheet, then row key
/// and column ([`crate::tables_store::Reading::records`]): a sheet absent is one whose Write's
/// records are not known, its grid refused.
pub(super) type Records = BTreeMap<&'static str, BTreeMap<(String, String), usize>>;

/// How the other row stands to the label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum By {
    /// It reads as the label now.
    Names,
    /// The label is staged in it too.
    Staged,
    /// It was written under the label before this row, and keeps it once written.
    First,
}

/// What a row reads as once the Write lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum After {
    /// The label it was given.
    Keeps,
    /// Its clash label.
    Reads(String),
    /// No label: which row keeps it cannot be settled.
    None,
    /// A clash label the read cannot name yet: a new row whose key is not made.
    Clash,
    /// Either: rows of the Write contend for it, in records not known while its grid is refused.
    Unsettled,
}

impl After {
    fn of(reading: Option<&Reading>, label: &str) -> Self {
        match reading.and_then(|r| r.reads.as_deref()) {
            Some(reads) if reads == label => Self::Keeps,
            Some(reads) => Self::Reads(reads.to_owned()),
            None => Self::None,
        }
    }

    fn words(&self) -> String {
        match self {
            Self::Keeps => "keeps it".to_owned(),
            Self::Reads(clash) => format!("reads as {clash}"),
            Self::None => "shows no label until its claim settles".to_owned(),
            Self::Clash => "reads under a clash label".to_owned(),
            Self::Unsettled => {
                "keeps it or reads under a clash label, as the Write settles it".to_owned()
            }
        }
    }
}

/// A staged label another row claims, as the read settles it once written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claimed {
    /// The label staged, as the save writes it.
    pub(crate) label: String,
    /// The other row, by the key the page shows it under.
    pub(crate) other: String,
    /// How that row is named: its words and its key's ending, or a new row's words.
    pub(crate) named: String,
    pub(crate) by: By,
    /// What this row reads as once written.
    pub(crate) this: After,
    /// What the other row reads as once written, where this row keeps the label.
    pub(crate) that: Option<After>,
    /// The sentence the cell bar, the cell and its drawer line say.
    pub(crate) words: String,
}

impl Claimed {
    fn sentence(&self) -> String {
        let first = match self.by {
            By::Names => format!("{} already names {}", self.label, self.named),
            By::Staged => format!("{} is staged for {} too", self.label, self.named),
            By::First => format!("{} was written for {} first", self.label, self.named),
        };
        let then = match (&self.this, &self.that) {
            (After::Keeps, Some(that)) => {
                format!("written, this row keeps it and that row {}", that.words())
            }
            (this, _) => format!("written, this row {}", this.words()),
        };
        format!("{first}; {then}")
    }
}

/// Each staged label of the page that another row claims, by its cell: what the read's
/// settlement of each sheet's claimed columns says one Write of the sheet's staged rows makes
/// them read as, each row in the record of it `records` holds.
pub(super) fn claimed(app: &TablesApp, records: &Records) -> BTreeMap<At, Claimed> {
    let mut out = BTreeMap::new();
    let Some(snapshot) = &app.snapshot else {
        return out;
    };
    for sheet in &snapshot.sheets {
        let Some(schema) = &sheet.schema else {
            continue;
        };
        for (column, settlement) in schema.settled.iter() {
            if settlement.rule() != ClaimRule::Label {
                continue;
            }
            let Some(compiled) = schema.collection.column(column) else {
                continue;
            };
            let records = records.get(sheet.name);
            said(app, sheet, column, compiled, settlement, records, &mut out);
        }
    }
    out
}

/// [`claimed`] of one sheet's claimed `column`, the Write's records as `records` holds them.
fn said(
    app: &TablesApp,
    sheet: &Sheet,
    column: &str,
    compiled: &aterm_db::typed::Column,
    settlement: &aterm_db::typed::claim::Settlement,
    records: Option<&BTreeMap<(String, String), usize>>,
    out: &mut BTreeMap<At, Claimed>,
) {
    // Each row the Write carries, by the key the save writes it under (a new row's as the
    // save would make it, where it can be made now), in the record that carries it, and the key
    // the page shows it under.
    let mut staged = BTreeMap::<String, Staged>::new();
    let mut shown = BTreeMap::<String, String>::new();
    // The new rows whose key is not made yet (something staged in the sheet is refused), with
    // the label staged in each: the clash label one of them reads as hashes its key, so the
    // read cannot name it, nor say which of two of them keeps a label.
    let mut unkeyed = Vec::<(String, String)>::new();
    for ((of, pk, col), text) in &app.staged {
        if of != sheet.name || (col != column && col != aterm_db::typed::LIFECYCLE) {
            continue;
        }
        let value = (col == column)
            .then(|| aterm_db::typed::grid::read(&compiled.ty, text).ok())
            .flatten();
        let key = if super::rows::is_new(pk) {
            let Some(key) = app.born_keys.get(&(sheet.name, pk.clone())) else {
                if let Some(label) = value.as_ref().and_then(|v| v.as_str()) {
                    unkeyed.push((pk.clone(), label.to_owned()));
                }
                continue;
            };
            key.clone()
        } else {
            pk.clone()
        };
        // A cell the save carries in no record writes nothing: the read holds what it stages.
        let record = match records {
            Some(records) => match records.get(&(key.clone(), col.clone())) {
                Some(record) => *record,
                None => continue,
            },
            None => 0,
        };
        shown.insert(key.clone(), pk.clone());
        let row = staged.entry(key).or_default();
        row.record = record;
        if col == column {
            row.value = value;
        } else {
            row.live = Some(text == aterm_db::typed::grid::LIVE);
        }
    }
    if unkeyed.is_empty() && !staged.values().any(|row| row.value.is_some()) {
        return;
    }
    let now = settlement.reads();
    let after = settlement.written(&staged);
    let page_key = |key: &str| shown.get(key).cloned().unwrap_or_else(|| key.to_owned());
    let label_of = |row: &Staged| {
        row.value
            .as_ref()
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    for (key, row) in &staged {
        let Some(label) = label_of(row) else {
            continue;
        };
        let label = label.as_str();
        let pk = page_key(key);
        let at = (sheet.name.to_owned(), pk.clone(), column.to_owned());
        let Some(mine) = after.get(key) else {
            continue;
        };
        // The rows that read as the label now, those it is staged in too, and the new rows with
        // no key yet given it.
        let reading: Vec<&str> = now
            .iter()
            .filter(|(other, r)| *other != key && r.reads.as_deref() == Some(label))
            .map(|(other, _)| other.as_str())
            .collect();
        let given: Vec<&str> = staged
            .iter()
            .filter(|(other, r)| *other != key && label_of(r).as_deref() == Some(label))
            .map(|(other, _)| other.as_str())
            .collect();
        let new: Option<&str> = unkeyed
            .iter()
            .find(|(_, l)| l == label)
            .map(|(pk, _)| pk.as_str());
        let reads_after = |other: &str| after.get(other).and_then(|r| r.reads.as_deref());
        let (other, this, that) = if mine.reads.as_deref() == Some(label) {
            // This row keeps it: said where it takes it from a row that reads as it now and,
            // once written, claims what it claims now and reads as something else; else from a
            // row it is staged in too. A row the same Write gives another label, clears or
            // retires lets the label go, and takes nothing from this one.
            let held = reading.iter().copied().find(|other| {
                let before = now.get(*other).map(|r| r.claims.as_str());
                after.get(*other).is_some_and(|r| {
                    Some(r.claims.as_str()) == before && r.reads.as_deref() != Some(label)
                })
            });
            let staged_too = given.iter().copied().find(|other| {
                after
                    .get(*other)
                    .is_some_and(|r| r.reads.as_deref() != Some(label))
            });
            let Some(other) = held.or(staged_too).or(new) else {
                continue;
            };
            let that = After::of(after.get(other), label);
            (other, After::Keeps, Some(that))
        } else {
            // Another row keeps it, or none can be settled: of the rows that claim it once
            // written, the one that keeps it, else one that reads as it now.
            let with = || mine.with.iter().map(String::as_str);
            let Some(other) = with()
                .find(|other| reads_after(other) == Some(label))
                .or_else(|| with().find(|other| reading.contains(other)))
                .or_else(|| with().next())
            else {
                continue;
            };
            (other, After::of(Some(mine), label), None)
        };
        let by = if reading.contains(&other) {
            By::Names
        } else if given.contains(&other) || Some(other) == new {
            By::Staged
        } else {
            By::First
        };
        // Which of the Write's own rows given the label keeps it turns on the records the save
        // appends them in, not known while the grid is refused; and a new row whose key is not
        // made yet, given the same label, may keep it instead of a row that does.
        let contends = match this {
            After::Keeps => !given.is_empty() || new.is_some(),
            _ => given.contains(&other),
        };
        let (this, that) = if records.is_none() && contends {
            (After::Unsettled, None)
        } else if new.is_some() {
            (After::Clash, that.map(|_| After::Clash))
        } else {
            (this, that)
        };
        let other = page_key(other);
        let mut claimed = Claimed {
            label: label.to_owned(),
            named: named(app, sheet, &other),
            other,
            by,
            this,
            that,
            words: String::new(),
        };
        claimed.words = claimed.sentence();
        out.insert(at, claimed);
    }
    // A new row with no key yet, given a label a row claims and reads as now and keeps once
    // written: that row's claim came first, so this one reads under a clash label the read
    // cannot name. Given a label another row of the Write is given too, which keeps it is not
    // known while the grid is refused.
    for (pk, label) in &unkeyed {
        let at = (sheet.name.to_owned(), pk.clone(), column.to_owned());
        let first = now.iter().find_map(|(other, r)| {
            (r.claims == *label
                && r.reads.as_deref() == Some(label)
                && after.get(other).and_then(|r| r.reads.as_deref()) == Some(label))
            .then(|| page_key(other))
        });
        let too = || {
            staged
                .iter()
                .find(|(_, row)| label_of(row).as_deref() == Some(label))
                .map(|(other, _)| page_key(other))
                .or_else(|| {
                    unkeyed
                        .iter()
                        .find(|(other, l)| other != pk && l == label)
                        .map(|(other, _)| other.clone())
                })
        };
        let (other, by, this) = match first {
            Some(other) => (other, By::Names, After::Clash),
            None => match too() {
                Some(other) => (other, By::Staged, After::Unsettled),
                None => continue,
            },
        };
        let mut claimed = Claimed {
            label: label.clone(),
            named: named(app, sheet, &other),
            other,
            by,
            this,
            that: None,
            words: String::new(),
        };
        claimed.words = claimed.sentence();
        out.insert(at, claimed);
    }
}

/// The claim the cell at `at` says: none where its rules refuse it (the page's check, or a Write
/// the save refused there), which it says instead. Every reader of a cell's claim leaves a
/// refused cell's out so ([`super::TablesApp::refusal`]).
fn of<'a>(app: &'a TablesApp, at: &At) -> Option<&'a Claimed> {
    app.claimed.get(at).filter(|_| app.refusal(at).is_none())
}

/// How row `pk` of `sheet` is named where a label it claims is staged in another: its words
/// ([`super::rows::Target::words`]) and its key's ending among the sheet's rows, which the grid
/// shows rows written under one label by; a new row by that word.
fn named(app: &TablesApp, sheet: &Sheet, pk: &str) -> String {
    if super::rows::is_new(pk) {
        return format!("a new {}", sheet.singular.to_lowercase());
    }
    let words = app
        .snapshot
        .as_ref()
        .and_then(|s| s.targets.get(sheet.name)?.rows.get(pk))
        .map_or(pk, |target| target.words.as_str());
    let keys: Vec<&str> = sheet.rows.iter().map(|row| row.pk.as_str()).collect();
    format!("{words} \u{b7} \u{2026}{}", super::rows::ending(pk, &keys))
}

/// The pane's lines for `row`'s staged labels another row claims, each with its height and the
/// key of its press: `In use: m22 → m22 · …00a1`, which shows that row (spoken from its words
/// on: `…, show it in Machines`).
pub(super) fn pane_lines(
    app: &TablesApp,
    sheet: &Sheet,
    row: &SheetRow,
    inner: f32,
    line: f32,
) -> Vec<(UiNode, f32, Option<String>)> {
    let title = app.title_of(sheet.name).unwrap_or(sheet.title);
    super::ordered(sheet)
        .into_iter()
        .filter_map(|column| {
            let at = (sheet.name.to_owned(), row.pk.clone(), column.name.clone());
            let claimed = of(app, &at)?;
            let key = format!("{TO}{}", column.name);
            let words = format!("In use: {} \u{2192} {}", claimed.label, claimed.named);
            let face = group(format!("{key}/face"))
                .layout(Layout::row().height(Length::Fixed(line)))
                .children(vec![fitted(
                    format!("{key}/words"),
                    &words,
                    StyleRef::Accent,
                    inner,
                )])
                .paint_only_tree();
            let node = group(format!("{key}/seat"))
                .layout(Layout::default().height(Length::Fixed(line)))
                .children(vec![
                    super::press(&key, format!("{words}, show it in {title}")),
                    face,
                ]);
            Some((node, line, Some(key)))
        })
        .collect()
}

/// What a press on a key of this module does; `None` for a key not its own.
pub(super) fn act(app: &TablesApp, view: &mut TablesViewState, key: &str) -> Option<bool> {
    let column = key.strip_prefix(TO)?;
    let sheet = app.current(view)?;
    let pk = view.row.clone()?;
    let claimed = of(app, &(sheet.to_owned(), pk, column.to_owned()))?;
    let other = claimed.other.clone();
    view.show_row(app, sheet, &other, Some(column));
    Some(true)
}

#[cfg(all(test, unix))]
#[path = "native_tables_claims_tests.rs"]
mod tests;
