// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rows on the Tables page (docs/PLAN.md LT-24; docs/DESIGN-tables-app-2026-11-14.md §3.2):
//! adding a row where the grid's birth rules let one be born, retiring or ending one by writing
//! its lifecycle cell, and restoring one, all through the one Write every staged cell goes by
//! ([`TablesApp::write`]).
//!
//! A new row is a grid row with its key left empty: the page gives it a key of its own until it
//! is written ([`NEW`]), the save derives its real one from its cells (the registry's
//! `aterm_db::alab::registry::birth`), and the check as each cell is staged derives it the same
//! way, so a key a cell cannot make, or one the table holds already, shows refused before Write.
//! Where the grid adds no row (a reference row every build compiles, a Mac's own rows), the
//! tools line says who adds one in place of an Add. A new row's secret is never typed: the pane
//! names the verb that sets it, and no secret-store entry, which only that verb makes. A cell of
//! a new row staged while its Write is out is moved to the key the row was born under.
//!
//! Retire names, before anything is staged, the live rows that still name the row, by the rule
//! the save's own check refuses such a retire by (`aterm_db::typed::check::referrers`, the one
//! `retired_referenced` reads), each by its words; a row whose end (or whose reference pointed
//! elsewhere) is staged no longer holds it back, and the Write saves its table first ([`order`]).
//! Restore waits while the row names a row that is not live, which the save refuses
//! (`aterm_db::typed::check::names_ended`). A row with no lifecycle written is live, as the
//! registry reads it. The lifecycle filter steps through live, the word a removed row takes, each
//! other word of the column's enum a row holds, conflicted rows where there are any, then all. A
//! change to a row's lifecycle stands in the grid as one word in the row number's seat, in the
//! not-written ink: `new`, `retire` (`end` for a link) or `restore`.
//!
//! Each read says whether this Mac can write the registry and why not
//! (`aterm_db::att::Handle::cannot_write`); where it cannot, the page takes no edit and the Write
//! bar's place holds that one sentence.
//!
//! A reference is a row, not a label (LT-30). Each read lists the rows every table holds as a
//! reference may name them ([`Targets`]): by the words each is known by (a machine's label, an
//! account's provider word and address, `aterm_db::alab::registry::Names`; else the column that
//! names its row), and where two rows share those words, by each one's key ending, its status
//! and whether a Mac holds it (a Mac's own row names it). A reference cell shows its row that
//! way, never its raw key; it opens on a list of the target's live rows less any a staged change
//! ends ([`choice`], the list SF-37 opens an enum on) and stages the chosen row's key. Typed
//! words mark a row only where they are its words whole or its key, never the first row whose
//! words start with them. Words that name no row, and a key of a row that is not live once
//! written, are refused at their cell as they are staged
//! ([`TablesApp::reference_refusals`]), by the save's own rules
//! (`aterm_db::typed::check::missing_row`, `names_ended`), so a link is never written by a label
//! (docs/KNOWN_ISSUES.md ATERM-KI-1112-155). The pane offers each row the selected row's
//! references name, a press away. A reference's history, and a pane's title, name its row by its
//! words and key ending alone, which its status now does not change.

use std::collections::{BTreeMap, BTreeSet};

use aterm_db::typed::Ty;

use super::{At, Sheet, SheetColumn, SheetRow, TablesApp, TablesViewState, button, fitted, group};
use crate::native_tables_choices::Choice;
use crate::native_ui::{Insets, Layout, Length, SemanticRole, StyleRef, UiNode};

/// What every key the page gives a new row starts with: a key no row of the registry has, since
/// a key's grammar takes no `+`.
pub(crate) const NEW: &str = "+new-";

/// The tools line's Add.
pub(crate) const ADD: &str = "tables/rows/add";
/// The tools line's lifecycle filter.
pub(crate) const FILTER: &str = "tables/rows/filter";
/// What every key the pane's row buttons start with.
pub(crate) const PANE: &str = "tables/pane/row/";
const RETIRE: &str = "tables/pane/row/retire";
const RESTORE: &str = "tables/pane/row/restore";
const DROP: &str = "tables/pane/row/drop";
const REFERRER: &str = "tables/pane/row/by/";
/// What the key of the pane's press to the row a reference names starts with; its column ends it.
const TO: &str = "tables/pane/row/to/";

/// Whether `pk` is a key the page gave a new row not yet written.
pub(crate) fn is_new(pk: &str) -> bool {
    pk.starts_with(NEW)
}

/// How a cells sheet's rows are added, retired, ended and restored, as its read found them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowLife {
    /// The words its lifecycle column takes, as the column's enum declares them.
    pub(crate) words: &'static [&'static str],
    /// The word a row the owner removes takes: `ended` for a link, else `retired`.
    pub(crate) ends: &'static str,
    /// Who adds a new row where the grid adds none; `None` where a grid row is born here.
    pub(crate) born_elsewhere: Option<&'static str>,
    /// The columns a new row is typed into: every one the grid does not hold read only.
    pub(crate) born_columns: Vec<String>,
    /// The columns only a verb sets (a secret, a sealed number), with that verb.
    pub(crate) set_by: Vec<(String, &'static str)>,
}

/// A live row that names a row, by its collection and key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Referrer {
    pub(crate) sheet: &'static str,
    pub(crate) pk: String,
}

/// Which rows a sheet shows by their lifecycle: the live ones, those under one other word of
/// the lifecycle column's enum, the conflicted ones, or all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Filter {
    Live,
    Word(&'static str),
    Conflicted,
    All,
}

impl Filter {
    /// Whether a sheet showing these rows shows `row`. A new row shows wherever live rows do, and
    /// a row with no lifecycle written is live, as the registry reads it
    /// (`aterm_db::typed::cell::Row::live`).
    pub(crate) fn admits(self, row: &SheetRow) -> bool {
        let life = row.cells.get(aterm_db::typed::LIFECYCLE);
        let conflicted = life.is_some_and(|c| !c.values.is_empty());
        let word = life.map_or("", |c| c.text.as_str());
        match self {
            Self::All => true,
            Self::Live if is_new(&row.pk) => true,
            Self::Live => !conflicted && is_live(word),
            Self::Word(w) => !conflicted && word == w,
            Self::Conflicted => conflicted,
        }
    }

    /// The filter's name on its button: `Live`, `Retired`, `Conflicted`, `All`.
    pub(crate) fn label(self) -> String {
        let word = match self {
            Self::Live => aterm_db::typed::grid::LIVE,
            Self::Word(w) => w,
            Self::Conflicted => "conflicted",
            Self::All => "all",
        };
        let mut chars = word.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
            .unwrap_or_default()
    }

    /// What the cell bar says of a selected row the filter leaves out: `not among the live
    /// rows`, `… the retired rows`, `… the conflicted rows`. All rows leave none out.
    pub(crate) fn left_out_words(self) -> String {
        match self {
            Self::Live => format!("not among the {} rows", aterm_db::typed::grid::LIVE),
            Self::Word(w) => format!("not among the {w} rows"),
            Self::Conflicted => "not among the conflicted rows".to_owned(),
            Self::All => "not among the rows shown".to_owned(),
        }
    }

    /// What the grid says where the filter leaves no row.
    pub(crate) fn none_words(self) -> String {
        match self {
            Self::Live => "No live rows".to_owned(),
            Self::Word(w) => format!("No {w} rows"),
            Self::Conflicted => "No row's lifecycle is conflicted".to_owned(),
            Self::All => "No rows".to_owned(),
        }
    }

    /// The filter after this one on `sheet`'s button ([`filters`]); live after the last, and
    /// after one the sheet no longer offers.
    fn next(self, sheet: &Sheet) -> Self {
        let all = filters(sheet);
        all.iter()
            .position(|f| *f == self)
            .and_then(|at| all.get(at + 1))
            .copied()
            .unwrap_or(Self::Live)
    }
}

/// Whether a lifecycle cell's word is a live row's: `live`, or none written.
fn is_live(word: &str) -> bool {
    word.is_empty() || word == aterm_db::typed::grid::LIVE
}

/// The filters `sheet`'s button steps through: live; the word a row the owner removes takes
/// (`retired`, or `ended` for a link); each other word of the lifecycle column's enum that a row
/// of the sheet holds, in the enum's order; conflicted, where a row's lifecycle is; then all. A
/// word no row of the table can take is never a step.
pub(crate) fn filters(sheet: &Sheet) -> Vec<Filter> {
    let Some(life) = &sheet.life else {
        return vec![Filter::All];
    };
    let held = |filter: Filter| sheet.rows.iter().any(|row| filter.admits(row));
    let mut all = vec![Filter::Live, Filter::Word(life.ends)];
    all.extend(
        life.words
            .iter()
            .filter(|w| !is_live(w) && **w != life.ends)
            .map(|w| Filter::Word(w))
            .filter(|f| held(*f)),
    );
    if held(Filter::Conflicted) {
        all.push(Filter::Conflicted);
    }
    all.push(Filter::All);
    all
}

impl TablesViewState {
    /// Which rows `sheet` shows by their lifecycle: live ones unless the view chose others; all
    /// of a sheet with no lifecycle.
    pub(crate) fn filter(&self, sheet: &Sheet) -> Filter {
        if sheet.life.is_none() {
            return Filter::All;
        }
        self.life.get(sheet.name).copied().unwrap_or(Filter::Live)
    }

    /// Whether the grid shows `row` of `sheet` by its lifecycle, the find aside: the filter
    /// admits it, or a change to it is staged (a row the filter leaves out shows while one is).
    pub(crate) fn life_shows(&self, app: &TablesApp, sheet: &Sheet, row: &SheetRow) -> bool {
        self.filter(sheet).admits(row)
            || app
                .staged
                .keys()
                .any(|(c, p, _)| c == sheet.name && *p == row.pk)
    }
}

/// The rows of `sheet` that `filter` leaves out and a staged change shows, each once: beside
/// the read and the filter, all that [`TablesViewState::life_shows`] reads, so the rows the grid
/// shows by the filter, and the rows its fills count, change only where this does.
pub(crate) fn shown_by_staging(app: &TablesApp, sheet: &Sheet, filter: Filter) -> BTreeSet<String> {
    if filter == Filter::All {
        return BTreeSet::new();
    }
    let staged: BTreeSet<&str> = app
        .staged
        .keys()
        .filter(|(of, ..)| of == sheet.name)
        .map(|(_, pk, _)| pk.as_str())
        .collect();
    if staged.is_empty() {
        return BTreeSet::new();
    }
    sheet
        .rows
        .iter()
        .filter(|row| staged.contains(row.pk.as_str()) && !filter.admits(row))
        .map(|row| row.pk.clone())
        .collect()
}

/// The rail's count of a sheet: its rows written and live, as the registry holds them.
pub(crate) fn counted(sheet: &Sheet) -> usize {
    if sheet.life.is_none() {
        return sheet.rows.iter().filter(|r| !is_new(&r.pk)).count();
    }
    sheet
        .rows
        .iter()
        .filter(|r| !is_new(&r.pk) && Filter::Live.admits(r))
        .count()
}

/// How many rows of `sheet` the view's lifecycle filter shows, a row with a staged change among
/// them, before any find: what the header's count is out of.
pub(crate) fn admitted(app: &TablesApp, view: &TablesViewState, sheet: &Sheet) -> usize {
    let filter = view.filter(sheet);
    sheet
        .rows
        .iter()
        .filter(|row| {
            filter.admits(row)
                || app
                    .staged
                    .keys()
                    .any(|(c, p, _)| c == sheet.name && *p == row.pk)
        })
        .count()
}

impl TablesApp {
    /// Put the new rows not written under their sheets' rows in the read the page holds, each
    /// with no cell yet, so the grid lays them out as rows of their own.
    pub(super) fn show_born(&mut self) {
        let born = &self.born;
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        for sheet in &mut snapshot.sheets {
            sheet.rows.retain(|row| !is_new(&row.pk));
            sheet
                .rows
                .extend(
                    born.iter()
                        .filter(|(name, _)| *name == sheet.name)
                        .map(|(_, pk)| SheetRow {
                            pk: pk.clone(),
                            cells: BTreeMap::new(),
                        }),
                );
        }
        // The grid's order is worked out again, so a row added or written shows.
        self.reads = self.reads.wrapping_add(1);
    }

    /// Whether the cell at `sheet`, `pk`, `column` is a row's lifecycle the pane stages: an
    /// existing row's, in a sheet with a lifecycle whose grid types it.
    pub(super) fn stages_life(&self, sheet: &str, pk: &str, column: &str) -> bool {
        column == aterm_db::typed::LIFECYCLE
            && !is_new(pk)
            && self.sheet(sheet).is_some_and(|s| {
                !s.owned && s.life.is_some() && s.base.header.iter().any(|h| h == column)
            })
    }

    /// The row's word for a change to its lifecycle not written: `new`, `retire` (`end` where
    /// its rows end) or `restore`.
    pub(crate) fn row_word(&self, sheet: &Sheet, pk: &str) -> Option<&'static str> {
        if is_new(pk) {
            return Some("new");
        }
        let life = sheet.life.as_ref()?;
        let staged = self.staged.get(&(
            sheet.name.to_owned(),
            pk.to_owned(),
            aterm_db::typed::LIFECYCLE.to_owned(),
        ))?;
        if staged == aterm_db::typed::grid::LIVE {
            Some("restore")
        } else if life.ends == "ended" {
            Some("end")
        } else {
            Some("retire")
        }
    }

    /// Add a new row to `sheet`, where the grid adds one and the page takes edits: its key.
    fn add_row(&mut self, sheet: &'static str) -> Option<String> {
        if self.fixed()
            || !self
                .sheet(sheet)
                .and_then(|s| s.life.as_ref())
                .is_some_and(|life| life.born_elsewhere.is_none())
        {
            return None;
        }
        self.born_count += 1;
        let pk = format!("{NEW}{}", self.born_count);
        self.born.push((sheet, pk.clone()));
        self.note = None;
        self.show_born();
        Some(pk)
    }

    /// Take back the new row `pk` of `sheet` and every cell staged in it: its pane's Remove,
    /// and the Undo of its line in the history drawer.
    pub(super) fn drop_row(&mut self, sheet: &str, pk: &str) {
        let staged: Vec<At> = self
            .staged
            .keys()
            .filter(|(s, p, _)| s == sheet && p == pk)
            .cloned()
            .collect();
        for at in &staged {
            self.unstage(at);
        }
        self.born.retain(|(s, p)| !(*s == sheet && p == pk));
        self.show_born();
    }

    /// Stage `word` in row `pk`'s lifecycle: the word a removed row takes, or live. A retire
    /// is not staged while a live row names the row ([`Self::blocking`]), which the save would
    /// refuse; a restore is not staged while the row names a row that is not live
    /// ([`Self::named_ended`]), which the save would refuse too.
    fn stage_life(&mut self, sheet: &str, pk: &str, word: &str) {
        if self.fixed() || !self.stages_life(sheet, pk, aterm_db::typed::LIFECYCLE) {
            return;
        }
        let Some(s) = self.sheet(sheet) else {
            return;
        };
        let held_back = if word == aterm_db::typed::grid::LIVE {
            s.rows
                .iter()
                .find(|r| r.pk == pk)
                .is_some_and(|row| self.named_ended(s, row).is_some())
        } else {
            !self.blocking(s, pk).is_empty()
        };
        if held_back {
            return;
        }
        let at = (
            sheet.to_owned(),
            pk.to_owned(),
            aterm_db::typed::LIFECYCLE.to_owned(),
        );
        let held = s
            .rows
            .iter()
            .find(|r| r.pk == pk)
            .and_then(|r| r.cells.get(aterm_db::typed::LIFECYCLE))
            .is_some_and(|c| c.values.is_empty() && c.text == word);
        self.refused.remove(&at);
        self.note = None;
        if held {
            self.unstage(&at);
        } else {
            self.put(at, word.to_owned(), None);
        }
    }

    /// The key a new row would be born under, as the save's reading of its staged cells
    /// derives it now; `None` where its key is not made from its cells (a phone's is random,
    /// made only as it is written).
    fn born_key(&self, sheet: &'static str, pk: &str) -> Option<&str> {
        self.born_keys
            .get(&(sheet, pk.to_owned()))
            .map(String::as_str)
    }

    /// The lifecycle word staged in row `pk` of `sheet`, where one is.
    pub(crate) fn staged_life(&self, sheet: &str, pk: &str) -> Option<&str> {
        self.staged
            .get(&(
                sheet.to_owned(),
                pk.to_owned(),
                aterm_db::typed::LIFECYCLE.to_owned(),
            ))
            .map(String::as_str)
    }

    /// Whether a change staged ends row `pk` of `sheet`: its lifecycle staged to a word that is
    /// not live.
    pub(crate) fn ends_staged(&self, sheet: &str, pk: &str) -> bool {
        self.staged_life(sheet, pk).is_some_and(|w| !is_live(w))
    }

    /// The live rows that name row `pk` of `sheet`, as the read found them
    /// ([`Sheet::referrers`]), less each a change staged here ends or points elsewhere: what a
    /// retire of it waits for. A Write saves the tables of those changes before the retire's
    /// ([`order`]), so the save's own check finds them ended.
    pub(crate) fn blocking<'a>(&self, sheet: &'a Sheet, pk: &str) -> Vec<&'a Referrer> {
        sheet
            .referrers
            .get(pk)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter(|by| {
                !self.ends_staged(by.sheet, &by.pk) && self.still_names(by, sheet.name, pk)
            })
            .collect()
    }

    /// Whether the live row `by` still names row `pk` of `to` once its staged cells are written:
    /// unless every one-row reference of it that named `pk` in the read is staged to another
    /// row, and nothing else of it (a set of references, a conflicted cell) may name it.
    fn still_names(&self, by: &Referrer, to: &str, pk: &str) -> bool {
        let Some(sheet) = self.sheet(by.sheet) else {
            return true;
        };
        let Some(row) = sheet.rows.iter().find(|r| r.pk == by.pk) else {
            return true;
        };
        let mut named = false;
        for column in &sheet.columns {
            match column.compiled.map(|c| c.ty) {
                Some(Ty::Ref(target)) if target == to => {
                    let cell = row.cells.get(&column.name);
                    if cell.is_some_and(|c| !c.values.is_empty()) {
                        return true;
                    }
                    if cell.and_then(|c| c.refers.as_deref()) == Some(pk) {
                        named = true;
                        if named_key(self, sheet, row, &column.name).as_deref() == Some(pk) {
                            return true;
                        }
                    }
                }
                Some(Ty::Set(Ty::Ref(target)) | Ty::List(Ty::Ref(target), _)) if *target == to => {
                    return true;
                }
                _ => {}
            }
        }
        !named
    }

    /// Why row `pk` of `from`, live once written, may not name row `key` of `to`, where it may
    /// not: the read holds no such row, or that row is not live once the Write lands (a retire
    /// staged here; a restore staged in another table, whose save may come after). The save's
    /// own rules refuse both (`aterm_db::typed::check::missing_row`, `names_ended`). `None` too
    /// where the read lists no rows of `to`.
    fn names(&self, from: &str, to: &str, key: &str) -> Option<String> {
        let named = self.snapshot.as_ref()?.targets.get(to)?;
        let title = self.title_of(to).unwrap_or(to);
        let Some(target) = named.rows.get(key) else {
            return Some(format!("names no row of {title}; choose one from its list"));
        };
        match self.staged_life(to, key) {
            Some(word) if !is_live(word) => Some(format!(
                "{title} row {} is {word} once written; a live row names only live rows",
                target.short
            )),
            Some(_) if target.live || to == from => None,
            Some(_) => Some(format!(
                "{title} row {} is {} until its restore is written; write that first",
                target.short, target.word
            )),
            None if target.live => None,
            None => Some(format!(
                "{title} row {} is {}; a live row names only live rows",
                target.short, target.word
            )),
        }
    }

    /// Why `row` of `sheet` may not be made live again, where it may not: the first row its
    /// one-row references name that is not live ([`Self::names`]).
    pub(crate) fn named_ended(&self, sheet: &Sheet, row: &SheetRow) -> Option<String> {
        sheet.columns.iter().find_map(|column| {
            let to = refers_to(column)?;
            let key = named_key(self, sheet, row, &column.name)?;
            self.names(sheet.name, to, &key)
        })
    }

    /// What the save's rules for references would refuse of the staged cells, by cell: each
    /// one-row reference staged in a row live once written that names no row, or a row that is
    /// not live then ([`Self::names`]); and a restore staged in a row that names one, said at
    /// its lifecycle. The check as each cell is staged adds these to the save's reading of the
    /// grid, which holds no other table's rows.
    pub(crate) fn reference_refusals(&self) -> BTreeMap<At, String> {
        let mut found = BTreeMap::new();
        let Some(snapshot) = &self.snapshot else {
            return found;
        };
        for sheet in snapshot.sheets.iter().filter(|s| s.life.is_some()) {
            let staged: BTreeSet<&str> = self
                .staged
                .keys()
                .filter(|(c, ..)| c == sheet.name)
                .map(|(_, pk, _)| pk.as_str())
                .collect();
            for pk in staged {
                let row = sheet.rows.iter().find(|r| r.pk == pk);
                let life = self.staged_life(sheet.name, pk);
                let live = match life {
                    Some(word) => is_live(word),
                    None => is_new(pk) || row.is_some_and(|r| Filter::Live.admits(r)),
                };
                if !live {
                    continue;
                }
                let restored = life.is_some() && !is_new(pk);
                for column in &sheet.columns {
                    let Some(to) = refers_to(column) else {
                        continue;
                    };
                    let at = (sheet.name.to_owned(), pk.to_owned(), column.name.clone());
                    let (key, place) = match self.staged.get(&at) {
                        Some(key) => (key.trim().to_owned(), at),
                        None if restored => {
                            let Some(key) = row
                                .and_then(|r| r.cells.get(&column.name))
                                .and_then(|c| c.refers.clone())
                            else {
                                continue;
                            };
                            let at = (
                                sheet.name.to_owned(),
                                pk.to_owned(),
                                aterm_db::typed::LIFECYCLE.to_owned(),
                            );
                            (key, at)
                        }
                        None => continue,
                    };
                    if key.is_empty() {
                        continue;
                    }
                    if let Some(rule) = self.names(sheet.name, to, &key) {
                        found.entry(place).or_insert(rule);
                    }
                }
            }
        }
        found
    }
}

/// Put `staged`, each table's save of one Write, in the order the save's own rules need: a
/// table whose rows a change ends (a link ended, a reference pointed elsewhere) before the table
/// of a row they named that a change retires, so the retire's save finds them ended
/// (`aterm_db::typed::check::retired_referenced`). Otherwise, and where two tables would each
/// wait on the other, by the tables' names.
pub(crate) fn order(app: &TablesApp, staged: &mut Vec<super::Staged>) {
    let names: Vec<String> = staged.iter().map(|s| s.base.collection.clone()).collect();
    // `(first, then)`: the table `first` is saved before `then`.
    let mut before: BTreeSet<(String, String)> = BTreeSet::new();
    for (sheet, pk, _) in app
        .staged
        .keys()
        .filter(|(s, p, c)| c == aterm_db::typed::LIFECYCLE && app.ends_staged(s, p))
    {
        let Some(s) = app.sheet(sheet) else {
            continue;
        };
        for by in s.referrers.get(pk).map_or(&[][..], Vec::as_slice) {
            if by.sheet != sheet && names.iter().any(|n| n == by.sheet) {
                before.insert((by.sheet.to_owned(), sheet.clone()));
            }
        }
    }
    let mut left: Vec<String> = names;
    let mut ordered = Vec::with_capacity(left.len());
    while !left.is_empty() {
        let at = left
            .iter()
            .position(|then| {
                !left
                    .iter()
                    .any(|first| before.contains(&(first.clone(), then.clone())))
            })
            .unwrap_or(0);
        ordered.push(left.remove(at));
    }
    staged.sort_by_key(|s| {
        ordered
            .iter()
            .position(|n| *n == s.base.collection)
            .unwrap_or(usize::MAX)
    });
}

/// What a press on a key of this module does; `None` for a key not its own.
pub(crate) fn act(app: &mut TablesApp, view: &mut TablesViewState, key: &str) -> Option<bool> {
    let sheet = app.current(view)?;
    match key {
        ADD => {
            view.close_edit(app, false);
            if let Some(pk) = app.add_row(sheet) {
                let column = app
                    .sheet(sheet)
                    .and_then(|s| s.life.as_ref())
                    .and_then(|life| life.born_columns.first().cloned());
                view.show_row(app, sheet, &pk, column.as_deref());
            }
            Some(true)
        }
        FILTER => {
            let next = app
                .sheet(sheet)
                .filter(|s| s.life.is_some())
                .map(|s| view.filter(s).next(s))?;
            view.life.insert(sheet, next);
            view.scroll.set(0.0);
            Some(true)
        }
        _ if key.starts_with(PANE) => {
            let pk = view.row.clone()?;
            match key {
                RETIRE => {
                    let ends = app.sheet(sheet)?.life.as_ref()?.ends;
                    app.stage_life(sheet, &pk, ends);
                }
                RESTORE => app.stage_life(sheet, &pk, aterm_db::typed::grid::LIVE),
                DROP => {
                    app.drop_row(sheet, &pk);
                    view.row = None;
                    view.column = None;
                }
                _ if key.starts_with(TO) => {
                    let column = key.strip_prefix(TO)?;
                    let s = app.sheet(sheet)?;
                    let to = s
                        .columns
                        .iter()
                        .find(|c| c.name == column)
                        .and_then(refers_to)?;
                    let row = s.rows.iter().find(|r| r.pk == pk)?;
                    let named = named_key(app, s, row, column)?;
                    view.show_row(app, to, &named, None);
                }
                _ => {
                    let n: usize = key.strip_prefix(REFERRER)?.parse().ok()?;
                    let s = app.sheet(sheet)?;
                    let by = (*app.blocking(s, &pk).get(n)?).clone();
                    view.show_row(app, by.sheet, &by.pk, None);
                }
            }
            Some(true)
        }
        _ => None,
    }
}

/// A tools line's button in a seat `w` wide, the button as wide as the seat: one given less
/// room than its label cuts the label, and never runs over the next tool.
fn seat(key: &str, mut node: UiNode, w: f32) -> UiNode {
    node.layout.width = Length::Fixed(w);
    group(format!("{key}/seat"))
        .layout(
            Layout::row()
                .width(Length::Fixed(w))
                .padding(Insets::symmetric(0.0, 3.0)),
        )
        .children(vec![node])
}

/// The tools line's lifecycle filter's label for `sheet`: the rows it shows. `None` where the
/// sheet has no lifecycle.
fn filter_label(view: &TablesViewState, sheet: &Sheet) -> Option<String> {
    sheet.life.as_ref()?;
    Some(format!("{} rows", view.filter(sheet).label()))
}

/// How wide the tools line's lifecycle filter for `sheet` is whole.
pub(crate) fn filter_width(view: &TablesViewState, sheet: &Sheet) -> Option<f32> {
    filter_label(view, sheet)
        .map(|label| crate::native_ui::native_button_label_width(&label) + 24.0)
}

/// The tools line's lifecycle filter for `sheet`, `w` wide: the rows it shows, and what a press
/// shows next. A past point shows it too, since it picks that point's rows as well.
pub(crate) fn filter_tool(view: &TablesViewState, sheet: &Sheet, w: f32) -> Option<UiNode> {
    let label = filter_label(view, sheet)?;
    Some(seat(
        FILTER,
        button(
            FILTER,
            &label,
            &format!(
                "{label} shown; press for {} rows",
                view.filter(sheet).next(sheet).label().to_lowercase()
            ),
            StyleRef::Secondary,
            true,
        ),
        w,
    ))
}

/// What follows the filter on the tools line of a table as it is: the Add, or where the grid adds
/// no row, the words that say who adds one.
enum Adds {
    Button(String),
    Words(String),
}

/// What follows the filter on `sheet`'s tools line: nothing where it has no lifecycle, and none
/// where this Mac takes no edit (a past point, a registry it cannot write) and the grid adds rows.
fn adds(app: &TablesApp, sheet: &Sheet) -> Option<Adds> {
    match sheet.life.as_ref()?.born_elsewhere {
        None if !app.fixed() => Some(Adds::Button(format!(
            "Add {}",
            sheet.singular.to_lowercase()
        ))),
        None => None,
        Some(elsewhere) => Some(Adds::Words(format!("Not added here: {elsewhere}"))),
    }
}

/// How wide what follows the filter on `sheet`'s tools line is whole ([`add_tool`]).
pub(crate) fn add_width(app: &TablesApp, sheet: &Sheet) -> Option<f32> {
    Some(match adds(app, sheet)? {
        Adds::Button(label) => crate::native_ui::native_button_label_width(&label) + 24.0,
        Adds::Words(words) => {
            crate::native_workers::text_width(&words, SemanticRole::Text, StyleRef::Note)
        }
    })
}

/// What follows the filter on `sheet`'s tools line, `w` wide: the Add, or where the grid adds no
/// row, who adds one.
pub(crate) fn add_tool(app: &TablesApp, sheet: &Sheet, w: f32) -> Option<UiNode> {
    Some(match adds(app, sheet)? {
        Adds::Button(label) => seat(
            ADD,
            button(
                ADD,
                &label,
                &format!("{label}: a new row of {}", sheet.title),
                StyleRef::Secondary,
                true,
            ),
            w,
        ),
        Adds::Words(words) => fitted("tables/rows/elsewhere", &words, StyleRef::Note, w),
    })
}

/// The pane's lines for `row`'s lifecycle, each with its height and the key of the button on
/// it: the Retire, End or Restore it takes, what still names it where a retire waits, and for a
/// new row the verbs that set what no grid types, and the way to take it back.
pub(crate) fn pane_lines(
    app: &TablesApp,
    sheet: &Sheet,
    row: &SheetRow,
    inner: f32,
    line: f32,
) -> Vec<(UiNode, f32, Option<String>)> {
    let Some(life) = &sheet.life else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let note = |key: &str, text: &str, style: StyleRef| {
        let node = crate::native_about::wrapped(key, text, inner, SemanticRole::Text, style);
        let h = match node.layout.height {
            Length::Fixed(h) => h,
            _ => line,
        };
        (node, h, None)
    };
    // A button on its line, and under it what it does, wrapped to the pane.
    let with_button = |key: &str, label: &str, spoken: &str, words: &str, enabled: bool| {
        let b_w = crate::native_ui::native_button_label_width(label) + 24.0;
        let node = group(format!("{key}/line"))
            .layout(Layout::row())
            .children(vec![
                button(key, label, spoken, StyleRef::Secondary, enabled),
                group(format!("{key}/rest"))
                    .layout(Layout::default().width(Length::Fixed((inner - b_w).max(0.0)))),
            ]);
        [
            (node, line + 4.0, enabled.then(|| key.to_owned())),
            note(&format!("{key}/words"), words, StyleRef::Note),
        ]
    };
    let singular = sheet.singular.to_lowercase();
    let fixed = app.fixed();
    if is_new(&row.pk) {
        lines.push(note(
            "tables/pane/row/new",
            &format!("A new {singular}, not written: Write adds it to the table"),
            StyleRef::Unwritten,
        ));
        let key = app.born_key(sheet.name, &row.pk);
        for (column, verb) in &life.set_by {
            let label = sheet
                .columns
                .iter()
                .find(|c| c.name == *column)
                .map_or_else(|| column.clone(), super::field_words);
            let verb = match key {
                Some(key) => verb.replace("<row>", key),
                None => (*verb).to_owned(),
            };
            // Only the verb makes the secret or the number, so the page names no store entry:
            // which one it fills (a tagged entry, or none where the generation seals it into its
            // cell) is the verb's to say.
            let words =
                format!("{label} is not typed here: once the row is written, {verb} sets it");
            lines.push(note(
                &format!("tables/pane/row/set/{column}"),
                &words,
                StyleRef::Note,
            ));
        }
        lines.extend(with_button(
            DROP,
            "Remove",
            &format!("Remove this new {singular}"),
            "Takes this row back; nothing of it is written",
            !app.writing,
        ));
        return lines;
    }
    let at = (
        sheet.name.to_owned(),
        row.pk.clone(),
        aterm_db::typed::LIFECYCLE.to_owned(),
    );
    let cell = row.cells.get(aterm_db::typed::LIFECYCLE);
    let word = cell.map_or("", |c| c.text.as_str());
    let conflicted = cell.is_some_and(|c| !c.values.is_empty());
    if let Some(staged) = app.staged.get(&at) {
        let words = if staged == aterm_db::typed::grid::LIVE {
            format!("Restored when written: the {singular} is live again")
        } else {
            format!("{} when written; Undo it under the fields", capital(staged))
        };
        lines.push(note("tables/pane/row/staged", &words, StyleRef::Unwritten));
        return lines;
    }
    let stages = !fixed && app.stages_life(sheet.name, &row.pk, aterm_db::typed::LIFECYCLE);
    if conflicted {
        lines.push(note(
            "tables/pane/row/state",
            &format!("This {singular}'s lifecycle is conflicted: keep one of its values below"),
            StyleRef::Note,
        ));
    } else if is_live(word) {
        let (label, verb) = if life.ends == "ended" {
            ("End", "ended")
        } else {
            ("Retire", "retired")
        };
        let referrers = app.blocking(sheet, &row.pk);
        if referrers.is_empty() {
            if stages {
                lines.extend(with_button(
                    RETIRE,
                    label,
                    &format!("{label} this {singular}"),
                    &format!("Its lifecycle becomes {verb}; its row and history stay"),
                    true,
                ));
            }
        } else {
            let n = referrers.len();
            let (rows, them) = if n == 1 {
                ("1 live row names".to_owned(), "that row")
            } else {
                (format!("{n} live rows name"), "those rows")
            };
            lines.extend(with_button(
                RETIRE,
                label,
                &format!("{label} this {singular}: {rows} it"),
                &format!("{rows} this {singular}: end or change {them} before it is {verb}"),
                false,
            ));
            for (i, by) in referrers.iter().enumerate() {
                let title = app.title_of(by.sheet).unwrap_or(by.sheet);
                let key = format!("{REFERRER}{i}");
                // The row by its words, never its key: what names this row is left out.
                let named = app
                    .sheet(by.sheet)
                    .and_then(|s| Some((s, s.rows.iter().find(|r| r.pk == by.pk)?)))
                    .and_then(|(s, r)| row_name(app, s, r, Some((sheet.name, &row.pk))))
                    .unwrap_or_else(|| by.pk.clone());
                let words = format!("{title} \u{b7} {named}");
                // A press under the words, as Check's findings are laid out.
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
                    .children(vec![super::press(&key, format!("Show {words}")), face]);
                lines.push((node, line, Some(key)));
            }
        }
    } else if word == "retired" || word == "ended" {
        if stages {
            // A row that names a row that is not live is not made live, which the save refuses.
            let held_back = app.named_ended(sheet, row);
            let words = match &held_back {
                Some(why) => format!("This {singular} is {word}. Not restored: {why}"),
                None => format!("This {singular} is {word}; Restore makes it live again"),
            };
            lines.extend(with_button(
                RESTORE,
                "Restore",
                &format!("Restore this {singular}"),
                &words,
                held_back.is_none(),
            ));
        } else {
            lines.push(note(
                "tables/pane/row/state",
                &format!("This {singular} is {word}"),
                StyleRef::Note,
            ));
        }
    } else if !word.is_empty() {
        lines.push(note(
            "tables/pane/row/state",
            &format!("This {singular} is {word}"),
            StyleRef::Note,
        ));
    }
    lines
}

/// `word` with its first letter set as a capital.
fn capital(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

/// One row a reference may name, as a read found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// The words it is known by: a machine's label, an account's provider word and address, else
    /// what its sheet names its row by.
    pub(crate) words: String,
    /// How a reference to it reads: its words, and where another row of its table is known by the
    /// same words, its key's ending, its status and whether a Mac holds it.
    pub(crate) said: String,
    /// How it is named where its status may since have changed (a reference's history, a pane's
    /// title): its words, and where another row shares them, its key's ending.
    pub(crate) short: String,
    /// Whether it is live: what a reference is chosen from.
    pub(crate) live: bool,
    /// Its lifecycle as the read found it: `live`, its word (`retired`), or `lifecycle
    /// conflicted`.
    pub(crate) word: String,
}

/// The rows of one table a reference may name, by key, and their keys in the order a choice
/// lists them: by their words, as the grid orders words (`m9` before `m55`), then by key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Named {
    pub(crate) rows: BTreeMap<String, Target>,
    pub(crate) order: Vec<String>,
}

/// Every cells table's rows a reference may name, by the table's name.
pub(crate) type Targets = BTreeMap<&'static str, Named>;

/// The shortest ending of `pk`, four characters at least, that none of `others` ends with.
pub(crate) fn ending<'a>(pk: &'a str, others: &[&str]) -> &'a str {
    let chars: Vec<(usize, char)> = pk.char_indices().collect();
    for n in 4..chars.len() {
        let end = &pk[chars[chars.len() - n].0..];
        if others
            .iter()
            .all(|other| *other == pk || !other.ends_with(end))
        {
            return end;
        }
    }
    pk
}

/// The rows of every cells sheet of a read as a reference may name them. `words` gives the words
/// the registry knows a row by (`aterm_db::alab::registry::Names`), where it knows them;
/// `written` the label a row was written under, where its table's labels are claimed (two rows
/// written under one label read as two: the claim's winner by it, the other by its clash label,
/// `m22-3f9a`), so rows written under one label are told apart whatever they read as; `held`
/// names each row a Mac holds, by table and key. Every row is listed, a retired one too, since a
/// reference outlives its row's life; a new row not written is not.
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "only the registry read of macOS and Linux lists the rows"
    )
)]
pub(crate) fn targets(
    sheets: &[Sheet],
    words: impl Fn(&str, &str) -> Option<String>,
    written: impl Fn(&str, &str) -> Option<String>,
    held: &BTreeSet<(&'static str, String)>,
) -> Targets {
    let mut targets = Targets::new();
    for sheet in sheets.iter().filter(|s| !s.owned) {
        let has_status = sheet.columns.iter().any(|c| c.name == "status");
        // Each row's key, words, status, whether a Mac holds it, whether it is live and its
        // lifecycle, and the label it was written under.
        let mut found: Vec<(String, String, String, bool, (bool, String), String)> = Vec::new();
        for row in sheet.rows.iter().filter(|r| !is_new(&r.pk)) {
            let text = |column: &str| {
                row.cells
                    .get(column)
                    .map(|c| c.text.trim())
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned)
            };
            let named = words(sheet.name, &row.pk)
                .or_else(|| {
                    (sheet.key != crate::tables_store::KEY)
                        .then(|| text(&sheet.key))
                        .flatten()
                })
                .unwrap_or_else(|| row.pk.clone());
            let live = sheet.life.is_none() || Filter::Live.admits(row);
            let life = row.cells.get(aterm_db::typed::LIFECYCLE);
            let word = if live {
                aterm_db::typed::grid::LIVE.to_owned()
            } else if life.is_some_and(|c| !c.values.is_empty()) {
                "lifecycle conflicted".to_owned()
            } else {
                life.map(|c| c.text.clone()).unwrap_or_default()
            };
            let status = if live {
                has_status
                    .then(|| text("status"))
                    .flatten()
                    .unwrap_or_else(|| word.clone())
            } else {
                word.clone()
            };
            let holds = held.contains(&(sheet.name, row.pk.clone()));
            let label = written(sheet.name, &row.pk)
                .unwrap_or_else(|| named.clone())
                .to_lowercase();
            found.push((row.pk.clone(), named, status, holds, (live, word), label));
        }
        let mut by_label: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (pk, .., label) in &found {
            by_label.entry(label).or_default().push(pk);
        }
        let mut named = Named::default();
        for (pk, words, status, holds, (live, word), label) in &found {
            let shared = &by_label[label.as_str()];
            let (said, short) = if shared.len() > 1 {
                let short = format!("{words} \u{b7} \u{2026}{}", ending(pk, shared));
                let said = format!(
                    "{short} \u{b7} {status} \u{b7} {}",
                    if *holds {
                        "a machine holds it"
                    } else {
                        "no machine holds it"
                    }
                );
                (said, short)
            } else {
                (words.clone(), words.clone())
            };
            named.rows.insert(
                pk.clone(),
                Target {
                    words: words.clone(),
                    said,
                    short,
                    live: *live,
                    word: word.clone(),
                },
            );
        }
        let mut order: Vec<&String> = named.rows.keys().collect();
        order.sort_by(|a, b| {
            let (wa, wb) = (&named.rows[*a].words, &named.rows[*b].words);
            super::natural_lowered(&wa.to_lowercase(), &wb.to_lowercase())
                .then_with(|| wa.cmp(wb))
                .then_with(|| a.cmp(b))
        });
        named.order = order.into_iter().cloned().collect();
        targets.insert(sheet.name, named);
    }
    targets
}

/// The table a one-row reference column names.
fn refers_to(column: &SheetColumn) -> Option<&'static str> {
    match column.compiled.map(|c| c.ty) {
        Some(Ty::Ref(to)) => Some(to),
        _ => None,
    }
}

/// Show each one-row reference of `sheets` as its row reads ([`Target::said`]), its key kept in
/// [`super::Shown::refers`]; a conflicted one's values each as their row reads. A key no row
/// answers shows as it is.
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "only the registry read of macOS and Linux lists the rows"
    )
)]
pub(crate) fn name_references(sheets: &mut [Sheet], targets: &Targets) {
    for sheet in sheets.iter_mut() {
        let columns: Vec<(String, &'static str)> = sheet
            .columns
            .iter()
            .filter_map(|c| refers_to(c).map(|to| (c.name.clone(), to)))
            .collect();
        for (column, to) in columns {
            let Some(named) = targets.get(to) else {
                continue;
            };
            let said = |key: &str| {
                named
                    .rows
                    .get(key)
                    .map_or_else(|| key.to_owned(), |t| t.said.clone())
            };
            // A change of the past names its row by what stays of it, not its status now.
            let short = |key: &str| {
                named
                    .rows
                    .get(key)
                    .map_or_else(|| key.to_owned(), |t| t.short.clone())
            };
            for row in &mut sheet.rows {
                let Some(cell) = row.cells.get_mut(&column) else {
                    continue;
                };
                if !cell.values.is_empty() {
                    cell.text = cell
                        .values
                        .iter()
                        .map(|(key, _)| said(key))
                        .collect::<Vec<_>>()
                        .join(" / ");
                } else if !cell.text.is_empty() {
                    let key = std::mem::take(&mut cell.text);
                    cell.text = said(&key);
                    cell.refers = Some(key);
                }
            }
            // Its history names each row by what stays of it; what it holds now in a past read
            // reads as the reference does.
            for cell in sheet
                .written
                .iter_mut()
                .flat_map(|w| &mut w.cells)
                .filter(|c| c.column == column && !c.now.is_empty())
            {
                cell.now = short(&cell.now);
            }
            for ((_, _), since) in sheet
                .since
                .iter_mut()
                .filter(|((_, c), s)| *c == column && !s.members && !s.now.is_empty())
            {
                since.now = since
                    .now
                    .split(" / ")
                    .map(said)
                    .collect::<Vec<_>>()
                    .join(" / ");
            }
        }
    }
}

/// How `value`, held or staged in `column`, reads: where the column is a reference, as the row
/// it names reads; else as it is.
pub(crate) fn reads(app: &TablesApp, column: &SheetColumn, value: &str) -> String {
    refers_to(column)
        .and_then(|to| app.snapshot.as_ref()?.targets.get(to)?.rows.get(value))
        .map_or_else(|| value.to_owned(), |t| t.said.clone())
}

/// The list a reference to table `to` opens on, `held` (a row's key) marked: the table's live
/// rows, each staged by its key and read by its words, the row `held` names whatever its life,
/// and for a reference that may be empty, empty last. `None` where the read lists no row of `to`.
pub(crate) fn choice(app: &TablesApp, to: &str, null: bool, held: &str) -> Option<Choice> {
    let named = app.snapshot.as_ref()?.targets.get(to)?;
    let held = held.trim();
    let mut words = Vec::new();
    let mut said = Vec::new();
    for pk in &named.order {
        let target = &named.rows[pk];
        // A row a change staged here ends is no longer one to name.
        if (target.live && !app.ends_staged(to, pk)) || pk == held {
            words.push(pk.clone());
            said.push(target.said.clone());
        }
    }
    if null {
        words.push(String::new());
        said.push(String::new());
    }
    if words.is_empty() {
        return None;
    }
    let at = words.iter().position(|w| w == held);
    Some(Choice {
        words,
        said,
        many: false,
        whole: true,
        at,
        wrote: held.to_owned(),
    })
}

/// How `row` of `sheet` is named (the pane's title, a row that names the one a retire waits on):
/// its first column in the pane's order, passing over a reference to row `not` (table and key),
/// as it reads with what is staged in it; a reference by the row it names ([`Target::short`]),
/// since its status may change. `None` where that column holds nothing.
pub(crate) fn row_name(
    app: &TablesApp,
    sheet: &Sheet,
    row: &SheetRow,
    not: Option<(&str, &str)>,
) -> Option<String> {
    let column = super::ordered(sheet).into_iter().find(|column| {
        let key = named_key(app, sheet, row, &column.name);
        not.is_none_or(|(to, pk)| refers_to(column) != Some(to) || key.as_deref() != Some(pk))
    })?;
    if let Some(to) = refers_to(column)
        && let Some(key) = named_key(app, sheet, row, &column.name)
    {
        let short = app
            .snapshot
            .as_ref()
            .and_then(|s| s.targets.get(to)?.rows.get(&key))
            .map(|t| t.short.clone());
        return Some(short.unwrap_or(key));
    }
    app.staged
        .get(&(sheet.name.to_owned(), row.pk.clone(), column.name.clone()))
        .cloned()
        .or_else(|| row.cells.get(&column.name).map(|c| c.text.clone()))
        .filter(|t| !t.trim().is_empty())
}

/// The key `row`'s reference in `column` names now: what is staged in it, else what it holds.
fn named_key(app: &TablesApp, sheet: &Sheet, row: &SheetRow, column: &str) -> Option<String> {
    app.staged
        .get(&(sheet.name.to_owned(), row.pk.clone(), column.to_owned()))
        .cloned()
        .or_else(|| row.cells.get(column).and_then(|c| c.refers.clone()))
        .filter(|key| !key.trim().is_empty())
}

/// The pane's lines for the rows `row`'s references name, each with its height and the key of
/// its press: `Machine → m22`, which shows that row.
pub(crate) fn reference_lines(
    app: &TablesApp,
    sheet: &Sheet,
    row: &SheetRow,
    inner: f32,
    line: f32,
) -> Vec<(UiNode, f32, Option<String>)> {
    let Some(snapshot) = app.snapshot.as_ref() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for column in super::ordered(sheet) {
        let Some(to) = refers_to(column) else {
            continue;
        };
        let Some(key) = named_key(app, sheet, row, &column.name) else {
            continue;
        };
        let (Some(target), Some(title)) = (
            snapshot.targets.get(to).and_then(|n| n.rows.get(&key)),
            app.title_of(to),
        ) else {
            continue;
        };
        let press_key = format!("{TO}{}", column.name);
        let words = format!("{} \u{2192} {}", super::field_words(column), target.said);
        let face = group(format!("{press_key}/face"))
            .layout(Layout::row().height(Length::Fixed(line)))
            .children(vec![fitted(
                format!("{press_key}/words"),
                &words,
                StyleRef::Accent,
                inner,
            )])
            .paint_only_tree();
        let node = group(format!("{press_key}/seat"))
            .layout(Layout::default().height(Length::Fixed(line)))
            .children(vec![
                super::press(&press_key, format!("Show {} in {title}", target.said)),
                face,
            ]);
        lines.push((node, line, Some(press_key)));
    }
    lines
}

/// The Write bar's place where this Mac cannot write the registry, in `width` and `h`: the one
/// sentence that says why, and a Discard where changes from before are still staged.
pub(crate) fn cannot_write(app: &TablesApp, why: &str, width: f32, h: f32) -> UiNode {
    let sentence = format!("This machine cannot write the registry: {why}");
    let mut children = Vec::new();
    let staged = !app.staged.is_empty() || !app.born.is_empty();
    let discard_w = if staged {
        crate::native_ui::native_button_label_width("Discard") + 24.0
    } else {
        0.0
    };
    children.push(fitted(
        "tables/bar/cannot",
        &sentence,
        StyleRef::Note,
        (width - 28.0 - discard_w - 12.0).max(1.0),
    ));
    if staged {
        children.push(
            group("tables/bar/buttons")
                .layout(
                    Layout::row()
                        .width(Length::Fixed(discard_w))
                        .padding(Insets::symmetric(0.0, 5.0)),
                )
                .children(vec![button(
                    super::DISCARD,
                    "Discard",
                    "Discard every change not written",
                    StyleRef::Secondary,
                    !app.writing,
                )]),
        );
    }
    group("tables/bar")
        .layout(
            Layout::row()
                .gap(12.0)
                .height(Length::Fixed(h))
                .padding(Insets::symmetric(14.0, 0.0)),
        )
        .children(children)
}

#[cfg(all(test, unix))]
#[path = "native_tables_rows_tests.rs"]
mod native_tables_rows_tests;
