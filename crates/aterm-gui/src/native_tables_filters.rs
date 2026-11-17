// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's filters (docs/PLAN.md LT-41): which rows the grid shows by the values their
//! columns hold. A filter is the view's: it writes nothing.
//!
//! - **The mark.** Each column's header carries a mark at its right end (a drawn funnel,
//!   [`StyleRef::Filter`], in the primary ink while a filter stands; never a triangle, which is
//!   the sort's mark), spoken `Filter Role`: a press of its own that opens the column's list
//!   under it, the mark's seat washed while it is open; the header's own press still sorts. The
//!   mark is gone with its column where the column is hidden or folded.
//! - **The list.** Laid over the page as the Columns list is ([`UiNode::surface`]: a press on its
//!   words or its frame, and the wheel over it, reach nothing under it), and it stays where it
//!   opened while it is open, though the chips' line under the tools comes or goes. Two
//!   one-press choices, `Only rows with a value` and `Only rows that are empty`; for a number or
//!   a time column the `At least` and `At most` fields: a whole number, or a date (`2026-11-16`,
//!   the whole day) or a date and its minute (`2026-11-16 14:05`). A bound that does not read so
//!   is marked invalid and leaves no row out; a value that does not read so does not meet a
//!   bound; a linked count's empty cell is its 0. Then the values the rows hold, each with how
//!   many rows hold it (`worker 24`), the most held first and then in the sort's order, and the
//!   empty rows (`(empty) 123`) last. A set's or a list's row holds each of its members, as its
//!   cell shows them (`a, b`, or a JSON list of words where a member holds a comma); a list of
//!   records, or of anything else its cell shows as JSON, holds its cell's words as one value; a
//!   reference's holds the words of the row it names, as its cell shows them; a conflicted cell
//!   holds its values as its cell shows them, as one (`a / b`). The 200 most held values are
//!   offered, and a line says how many more there are (a value chosen is offered wherever it
//!   stands, and a value pressed stays offered while the list is open, so no line leaves under
//!   the keys or the pointer). The counts are of the written rows the lifecycle filter shows, the
//!   find and every filter aside, as the Columns list's fills are: a count does not move while
//!   other filters do. Choosing values keeps the rows that hold one of them. A one-press choice
//!   stands alone: choosing it clears the values chosen, and choosing a value clears it; `Only
//!   rows that are empty` clears the bounds too, which no empty row meets. The arrows move among
//!   the list's lines, Page Up and Page Down by its room, Command-Home and Command-End to its
//!   ends; Space or Return presses one, a field takes typing, and Escape closes the list with the
//!   keys back on the control it opened from (its mark, or its chip). The list is one stop of
//!   Tab's: Tab and Shift-Tab close it and go on from that control.
//! - **Chips.** Each filter that stands is a chip, in a line of its own under the tools row shown
//!   only while one stands, that says it in words: `ROLE is worker, desk`, `ALIASES includes m2`
//!   (a set or a list holds its values among others), `RAM GB at least 64 and at most 128`,
//!   `CHIP has a value`, `CHIP is empty`; past three values, `and 4 more`. A press on a chip
//!   opens its column's list under it; its close mark (`Remove filter`) takes the filter away.
//!   Where two or more stand, `Clear filters` ends the line. Chips that do not fit wrap onto a
//!   second and a third line; on the last the chips give way to share it, so every chip's close
//!   mark and `Clear filters` stay on the page.
//! - **Which rows.** One filter a column, its parts joined with and; filters join with and, after
//!   the lifecycle filter and beside the find. A row with a staged cell and a new row stay shown,
//!   as under the find. A filter judges what the registry holds, never staged words, and the rows
//!   are worked out again on a read as the sort's order is ([`TablesViewState::put_in_order`]): no
//!   row leaves while it is typed in. Where no row passes, the grid says so with `Clear filters`.
//! - **Hidden columns.** A filter on a column that is then hidden or folded stands, and its chip
//!   with it: the chip is how the filter is seen and changed while the column's mark is gone.
//! - **The page's height.** The chips' lines and the gap under them come off the grid's room
//!   once, where the tools row's do, and count in the window's judgment of whether the cell bar
//!   and the history drawer stand: a filter is the view the person set, never a selection, so a
//!   filter added in a short window can take the bar and then the drawer away, and taking it off
//!   brings them back.
//! - **A row asked for.** A press that asks to see a row the filters alone leave out, Check's
//!   Show row or a line of the history drawer, clears them, all of them, so the row shows
//!   ([`TablesViewState::clear_filters_for`]). A row they pass keeps them, and so does a row the
//!   find or the lifecycle filter leaves out as well, where clearing them would show no row:
//!   Show row empties the find and widens the lifecycle filter first where they leave it out,
//!   and a line of the drawer keeps both and names the row in its head. The drawer lists the
//!   table's records whatever the filters show: a filter is a view of rows, never of history.
//!
//! The filters are kept per table while the tab lives: a read keeps them, and another table
//! chosen keeps its own.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use aterm_db::typed::Ty;
use aterm_types::rfc3339::{civil_from_days, days_from_civil};

use super::native_tables_columns::{LINE_GAP, line_press};
use super::{
    ColumnKind, FIND, Group, Rank, Sheet, SheetColumn, SheetRow, TablesApp, TablesViewState,
    button, cell_key, edit, field_words, fitted, group, ordered, row_height, rows, scale, styled,
};
use crate::native_app::{AppEvent, SemanticInput, TextInputEvent};
use crate::native_text_input::TextInputState;
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    SemanticValue, StyleRef, TextFieldSpec, TextSpec, UiContent, UiNode,
};
use crate::native_workers::text_width;

/// A column's mark in its header: a press opens the column's list.
pub(crate) const MARK: &str = "tables/mark/";
/// Every key of the open list starts so.
const LIST: &str = "tables/filter/";
/// The open list's surface: a press on it that no line takes.
const PANEL: &str = "tables/filter/panel";
/// `Only rows with a value`.
pub(crate) const HAS: &str = "tables/filter/has";
/// `Only rows that are empty`.
pub(crate) const EMPTY: &str = "tables/filter/empty";
/// The `At least` and `At most` fields.
pub(crate) const LEAST: &str = "tables/filter/least";
pub(crate) const MOST: &str = "tables/filter/most";
/// A value's line: the value ends its key, as [`value_key`] writes it.
pub(crate) const VALUE: &str = "tables/filter/value/";
/// The empty rows' line.
pub(crate) const BLANK: &str = "tables/filter/blank";
/// A chip under the tools row: a press opens its column's list.
pub(crate) const CHIP: &str = "tables/chips/chip/";
/// A chip's close mark.
pub(crate) const REMOVE: &str = "tables/chips/remove/";
/// The chips' line's `Clear filters`.
pub(crate) const CLEAR: &str = "tables/chips/clear";
/// The grid's `Clear filters`, where no row passes them.
pub(crate) const CLEAR_NONE: &str = "tables/none/clear";
/// The most values the list offers: past them it says how many more there are.
pub(crate) const SHOWN: usize = 200;
/// How the empty value reads in the list and on a chip.
const EMPTY_WORDS: &str = "(empty)";
/// How wide the open list is, at the text scale of 1.
const LIST_WIDTH: f32 = 300.0;
/// How wide a header's mark is, and the room between it and the header's right end, where the
/// column's edge is held.
pub(crate) const FILTER_MARK: f32 = 16.0;
const MARK_SPACE: f32 = 10.0;
/// The room a header's name and its sort's mark leave at its end for the filter's mark and a
/// gap before it, so the two marks do not run together.
pub(crate) const HEAD_MARK: f32 = FILTER_MARK + 4.0;
/// How wide a chip's close mark is.
const CLOSE_MARK: f32 = 22.0;
/// The narrowest a chip gives way to, where the line is short of room for them all.
const CHIP_LEAST: f32 = 72.0;
/// The most values a chip names: past them it says how many more.
const CHIP_VALUES: usize = 3;
/// The most lines the chips wrap onto: on the last, the chips left give way to share it.
const CHIP_LINES: usize = 3;
/// The room between two chips on their line, and before `Clear filters`.
const CHIP_GAP: f32 = 8.0;

/// One column's filter: what a row's value in it must be for the grid to show the row. Its
/// parts join with and, as its list shows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ColumnFilter {
    /// Only rows with a value (`Some(true)`), or only those that are empty (`Some(false)`).
    has: Option<bool>,
    /// The values chosen: a row passes where it holds one of them; `""` is the empty value.
    values: BTreeSet<String>,
    /// The least and the most value a row may hold, as typed: a number or a time column's, each
    /// one that reads as the column's kind ([`Point::of`]); a bound that does not read is none.
    least: Option<String>,
    most: Option<String>,
}

impl ColumnFilter {
    /// Whether it leaves any row out: a part of it is chosen.
    pub(crate) fn stands(&self) -> bool {
        self.has.is_some() || !self.values.is_empty() || self.least.is_some() || self.most.is_some()
    }

    /// Whether a row that holds `held` in `column` passes.
    fn passes(&self, column: &SheetColumn, held: &[Cow<'_, str>]) -> bool {
        let has = match self.has {
            Some(true) => !held.is_empty(),
            Some(false) => held.is_empty(),
            None => true,
        };
        let chosen = self.values.is_empty()
            || if held.is_empty() {
                self.values.contains("")
            } else {
                held.iter()
                    .any(|value| self.values.contains(value.as_ref()))
            };
        let bounded = self.least.is_none() && self.most.is_none()
            || if held.is_empty() {
                // A linked count shows none as an empty cell: to a bound it is the 0 it counts.
                counts(column) && self.within(column.kind, "0")
            } else {
                held.iter().any(|value| self.within(column.kind, value))
            };
        has && chosen && bounded
    }

    /// Whether `value` stands between the bounds as a column of `kind` reads them
    /// ([`Point::of`]): a value that does not read so meets no bound.
    fn within(&self, kind: ColumnKind, value: &str) -> bool {
        let Some(at) = Point::of(kind, value) else {
            return false;
        };
        let meets = |bound: Option<&str>, past: Ordering| {
            bound
                .and_then(|bound| Point::of(kind, bound))
                .is_none_or(|bound| at.cmp(bound) != Some(past))
        };
        meets(self.least.as_deref(), Ordering::Less)
            && meets(self.most.as_deref(), Ordering::Greater)
    }
}

/// A value or a bound as `At least` and `At most` read it: a whole number in a number's column;
/// in a time's, a date and, where it names one, its minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    Number(i128),
    /// Days since 1970-01-01, and the minute of the day where one is named.
    Date(i64, Option<u16>),
}

impl Point {
    /// What `text` reads as in a column of `kind`: a whole number (`64`, `-3`); a date
    /// (`2026-11-16`, a day its month has) or a date and its minute (`2026-11-16 14:05`), as the
    /// page writes a time; `None` for anything else, and for any text in a column of words.
    fn of(kind: ColumnKind, text: &str) -> Option<Self> {
        let text = text.trim();
        match kind {
            ColumnKind::Number => text.parse::<i128>().ok().map(Self::Number),
            ColumnKind::Time => {
                let (date, minute) = match text.split_once(' ') {
                    Some((date, minute)) => (date, Some(minute.trim())),
                    None => (text, None),
                };
                let digits = |part: Option<&str>, len: usize| {
                    part.filter(|p| p.len() == len && p.bytes().all(|b| b.is_ascii_digit()))?
                        .parse::<i64>()
                        .ok()
                };
                let mut parts = date.split('-');
                let (year, month, day) = (
                    digits(parts.next(), 4)?,
                    digits(parts.next(), 2)?,
                    digits(parts.next(), 2)?,
                );
                if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day)
                {
                    return None;
                }
                let days = days_from_civil(year, month, day);
                // A day past its month's end (`2026-02-30`) names another date: no date.
                if civil_from_days(days) != (year, month, day) {
                    return None;
                }
                let minute = match minute {
                    None => None,
                    Some(minute) => {
                        let (hour, of_hour) = minute.split_once(':')?;
                        let (hour, of_hour) = (digits(Some(hour), 2)?, digits(Some(of_hour), 2)?);
                        if hour > 23 || of_hour > 59 {
                            return None;
                        }
                        Some(u16::try_from(hour * 60 + of_hour).ok()?)
                    }
                };
                Some(Self::Date(days, minute))
            }
            ColumnKind::Text => None,
        }
    }

    /// How `self` stands to `other`: numbers by their value; dates by their day, and by their
    /// minute where both name one, so a date alone is its whole day (`At most 2026-11-14` holds
    /// `2026-11-14 23:59`); `None` for a number beside a date.
    fn cmp(self, other: Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => Some(a.cmp(&b)),
            (Self::Date(a, Some(at)), Self::Date(b, Some(bt))) => Some((a, at).cmp(&(b, bt))),
            (Self::Date(a, _), Self::Date(b, _)) => Some(a.cmp(&b)),
            _ => None,
        }
    }
}

/// Whether `column` is a count of linked rows, which shows none as an empty cell.
fn counts(column: &SheetColumn) -> bool {
    column.group == Group::Linked && column.compiled.is_none() && column.kind == ColumnKind::Number
}

/// Each table's filters, by column.
pub(crate) type Filters = BTreeMap<String, ColumnFilter>;

/// The list open, and the control it opened from: the column's mark or its chip.
struct Open {
    column: String,
    from: String,
    /// The values offered whatever the rows hold while the list is open: those chosen as it
    /// opened and each pressed since (`""` the empty rows'), so a line a press unchooses stays
    /// under the keys and the pointer, unchosen, until the list closes.
    kept: BTreeSet<String>,
    /// Where it was first laid out, its left and its top, with the main column's right edge
    /// then: it stays there while it is open and that edge does not move, though the chips'
    /// line comes or goes over the grid.
    at: Cell<Option<(f32, f32, f32)>>,
}

/// The filters' view state ([`TablesViewState::filters`]).
pub(crate) struct FiltersView {
    /// Each table's filters, by its name.
    tables: BTreeMap<String, Filters>,
    /// The open list.
    open: Option<Open>,
    /// How far the list's lines are scrolled, how far they can be, and how tall their room is.
    scroll: f32,
    limit: Cell<f32>,
    room: Cell<f32>,
    /// Where each of the list's lines that takes a press stood as it was last laid out: its key,
    /// its top and its bottom. The arrows scroll the one they reach into view.
    lines: RefCell<Vec<(String, f32, f32)>>,
    /// The line the keys were last put on: a press on the list's surface leaves them there.
    line: Option<String>,
    /// The open list's `At least` and `At most`, as typed.
    least: TextInputState,
    most: TextInputState,
    /// Where the grid last laid each column's mark: its right edge, across the grid.
    marks: RefCell<BTreeMap<String, f32>>,
    /// Where the chips' lines last laid each chip: its column, its left and the foot of its
    /// line, down from the top of the first.
    chips: RefCell<Vec<(String, f32, f32)>>,
}

impl Default for FiltersView {
    fn default() -> Self {
        Self {
            tables: BTreeMap::new(),
            open: None,
            scroll: 0.0,
            limit: Cell::new(0.0),
            room: Cell::new(0.0),
            lines: RefCell::new(Vec::new()),
            line: None,
            least: TextInputState::new(String::new()),
            most: TextInputState::new(String::new()),
            marks: RefCell::new(BTreeMap::new()),
            chips: RefCell::new(Vec::new()),
        }
    }
}

impl FiltersView {
    /// Whether a column's list is open.
    pub(crate) const fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Note where the grid laid each column's mark: its right edge, across the grid.
    pub(crate) fn laid_marks(&self, marks: BTreeMap<String, f32>) {
        *self.marks.borrow_mut() = marks;
    }

    /// Forget table `sheet`'s filters.
    pub(crate) fn clear(&mut self, sheet: &str) {
        self.tables.remove(sheet);
    }
}

/// Whether `column` holds several values a row: a set or a list.
fn several(column: &SheetColumn) -> bool {
    matches!(
        column.compiled.map(|c| &c.ty),
        Some(Ty::Set(_) | Ty::List(..))
    )
}

/// What `row` holds in `column` that a filter judges, as the grid shows it
/// (`aterm_db::typed::grid::show`): each member of a set or a list, which its cell shows as `a,
/// b`, or as a JSON list of words where a member holds a comma (and a list of import
/// coordinates always: `["xlsx:A1","xlsx:B2"]`); a list its cell shows as JSON of anything but
/// words (a list of records), or in words of its own (a plan's limits), and a conflicted cell,
/// as one value, its cell's words; else its one value. Nothing where it is empty.
fn held<'a>(column: &SheetColumn, row: &'a SheetRow) -> Vec<Cow<'a, str>> {
    let Some(cell) = row.cells.get(&column.name) else {
        return Vec::new();
    };
    let text = cell.text.as_str();
    if text.is_empty() {
        return Vec::new();
    }
    let whole = || vec![Cow::Borrowed(text)];
    if !several(column) || !cell.values.is_empty() {
        return whole();
    }
    let mut members: Vec<Cow<'a, str>> = Vec::new();
    let mut hold = |member: Cow<'a, str>| {
        if !member.is_empty() && !members.contains(&member) {
            members.push(member);
        }
    };
    if text.starts_with('[') {
        let Ok(aterm_json::Value::Array(items)) = aterm_json::from_str::<aterm_json::Value>(text)
        else {
            return whole();
        };
        for item in items {
            let aterm_json::Value::String(word) = item else {
                return whole();
            };
            hold(Cow::Owned(word.trim().to_owned()));
        }
    } else if matches!(
        column.compiled.map(|c| &c.ty),
        Some(Ty::List(Ty::Struct(_), _))
    ) {
        return whole();
    } else {
        for member in text.split(',') {
            hold(Cow::Borrowed(member.trim()));
        }
    }
    if members.is_empty() {
        return whole();
    }
    members
}

/// The key of `value`'s line in a column's list: [`VALUE`], then the value with each whitespace
/// or control character and each `%` written as its bytes, `%XX`, so the key is one word a
/// driver can name (`aterm ctl act`, whose keys hold no whitespace): `rack a` is
/// `tables/filter/value/rack%20a`.
pub(crate) fn value_key(value: &str) -> String {
    let mut key = String::from(VALUE);
    for ch in value.chars() {
        if ch.is_whitespace() || ch.is_control() || ch == '%' {
            for byte in ch.encode_utf8(&mut [0; 4]).bytes() {
                key.push_str(&format!("%{byte:02X}"));
            }
        } else {
            key.push(ch);
        }
    }
    key
}

/// The value whose line's key is `key` ([`value_key`]); `None` for any other key.
fn value_of_key(key: &str) -> Option<String> {
    let written = key.strip_prefix(VALUE)?.as_bytes();
    let mut bytes = Vec::with_capacity(written.len());
    let mut at = 0;
    while let Some(&byte) = written.get(at) {
        if byte == b'%' {
            let hex = std::str::from_utf8(written.get(at + 1..at + 3)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            bytes.push(byte);
            at += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

/// Whether `column` takes `At least` and `At most`: a number or a time.
fn bounded(column: &SheetColumn) -> bool {
    column.kind != ColumnKind::Text
}

/// Whether `row` of `sheet` passes every one of `filters`.
pub(crate) fn passes(sheet: &Sheet, filters: &Filters, row: &SheetRow) -> bool {
    filters.iter().all(|(name, filter)| {
        sheet
            .columns
            .iter()
            .find(|c| c.name == *name)
            .is_none_or(|column| filter.passes(column, &held(column, row)))
    })
}

/// Whether `filters` leave `row` of `sheet` out of the grid: a new row and a row with a staged cell
/// show whatever they say, so no row leaves while it is typed in; any other row shows only where it
/// passes every one of them. The one rule the grid's rows ([`TablesViewState::put_in_order`]) and a
/// press that asks for a row ([`TablesViewState::clear_filters_for`]) both read.
pub(crate) fn leave_out(app: &TablesApp, sheet: &Sheet, filters: &Filters, row: &SheetRow) -> bool {
    !filters.is_empty()
        && !rows::is_new(&row.pk)
        && !super::staged(app, sheet, row)
        && !passes(sheet, filters, row)
}

/// What a column's list offers: each value the rows hold with how many hold it, in the list's
/// order, and how many it leaves unoffered past [`SHOWN`]; how many rows hold a value and how
/// many are empty, and whether it offers the empty rows' line.
struct Offered {
    values: Vec<(String, usize)>,
    more: usize,
    with: usize,
    empty: usize,
    blank: bool,
}

impl TablesViewState {
    /// The filters that stand on `sheet`, each on a column it has: what the rows it shows are
    /// worked out from.
    pub(crate) fn filters_of(&self, sheet: &Sheet) -> Filters {
        self.filters
            .tables
            .get(sheet.name)
            .map(|filters| {
                filters
                    .iter()
                    .filter(|(name, filter)| {
                        filter.stands() && sheet.columns.iter().any(|c| c.name == **name)
                    })
                    .map(|(name, filter)| (name.clone(), filter.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The one rule for the filters on the columns' values that a press asking to see row `pk`
    /// of `sheet` follows, Check's Show row ([`Self::show_row`]) and a history line's
    /// ([`TablesApp::history_select`]) alike: where they alone leave the row out ([`leave_out`],
    /// the find and the lifecycle filter showing it) they are cleared, all of them, as `Clear
    /// filters` clears them, so the row shows. A row they pass keeps them, and so does a row the
    /// find or the lifecycle filter leaves out as well: clearing them would show no row, and the
    /// person would lose them for nothing. Show row lets the find and the lifecycle filter give
    /// way first; a history line lets neither. Whether they were cleared.
    pub(crate) fn clear_filters_for(&mut self, app: &TablesApp, sheet: &Sheet, pk: &str) -> bool {
        let filters = self.filters_of(sheet);
        let find = self.find.value().trim().to_lowercase();
        if !sheet.rows.iter().any(|row| {
            row.pk == pk
                && leave_out(app, sheet, &filters, row)
                && self.life_shows(app, sheet, row)
                && super::found(app, sheet, row, &find)
        }) {
            return false;
        }
        self.filters.clear(sheet.name);
        true
    }

    /// The filter on `column` of table `sheet`, as it stands or not.
    fn filter_on(&self, sheet: &str, column: &str) -> ColumnFilter {
        self.filters
            .tables
            .get(sheet)
            .and_then(|filters| filters.get(column))
            .cloned()
            .unwrap_or_default()
    }

    fn filter_mut(&mut self, sheet: &str, column: &str) -> &mut ColumnFilter {
        self.filters
            .tables
            .entry(sheet.to_owned())
            .or_default()
            .entry(column.to_owned())
            .or_default()
    }

    /// The columns of `sheet` a filter stands on, in the grid's order, with their filters: the
    /// chips, left to right.
    fn standing<'a>(&self, sheet: &'a Sheet) -> Vec<(&'a SheetColumn, ColumnFilter)> {
        let filters = self.filters_of(sheet);
        ordered(sheet)
            .into_iter()
            .filter_map(|column| filters.get(&column.name).map(|f| (column, f.clone())))
            .collect()
    }

    /// What `column`'s list offers, counted over the written rows the lifecycle filter shows:
    /// beside the values they hold, each value chosen and, while the list of `column` is open,
    /// each it keeps ([`Open::kept`]), however few rows hold it.
    fn offered(&self, app: &TablesApp, sheet: &Sheet, column: &SheetColumn) -> Offered {
        let mut by: BTreeMap<Cow<'_, str>, usize> = BTreeMap::new();
        let (mut with, mut empty) = (0, 0);
        for row in sheet
            .rows
            .iter()
            .filter(|row| !rows::is_new(&row.pk) && self.life_shows(app, sheet, row))
        {
            let held = held(column, row);
            if held.is_empty() {
                empty += 1;
            }
            with += usize::from(!held.is_empty());
            for value in held {
                *by.entry(value).or_insert(0) += 1;
            }
        }
        let mut keep = self.filter_on(sheet.name, &column.name).values;
        if let Some(open) = self.filters.open.as_ref()
            && open.column == column.name
        {
            keep.extend(open.kept.iter().cloned());
        }
        let mut values: Vec<(String, usize)> =
            by.into_iter().map(|(v, n)| (v.into_owned(), n)).collect();
        for value in &keep {
            if !value.is_empty() && !values.iter().any(|(v, _)| v == value) {
                values.push((value.clone(), 0));
            }
        }
        values.sort_by(|(a, n), (b, m)| {
            m.cmp(n)
                .then_with(|| Rank::of(column.kind, a).cmp(&Rank::of(column.kind, b)))
        });
        // The most held, and every value chosen or kept wherever it stands.
        let mut more = 0;
        let mut at = 0;
        values.retain(|(value, _)| {
            at += 1;
            let offer = at <= SHOWN || keep.contains(value);
            more += usize::from(!offer);
            offer
        });
        Offered {
            values,
            more,
            with,
            empty,
            blank: empty > 0 || keep.contains(""),
        }
    }

    /// `column`'s filter in words, its column named as its header names it: `ROLE is worker,
    /// desk`, `RAM GB at least 64 and at most 128`, `CHIP has a value`, `CHIP is empty`. Its
    /// values are named in the order its list shows them, three at most, then how many more; a
    /// set or a list `includes` them, as its row holds them among others; the empty value chosen
    /// beside others reads `or empty` (`or is empty` after `includes`), and alone `is empty`, as
    /// `Only rows that are empty` does.
    fn chip_words(
        &self,
        app: &TablesApp,
        sheet: &Sheet,
        column: &SheetColumn,
        filter: &ColumnFilter,
    ) -> String {
        let mut parts = Vec::new();
        match filter.has {
            Some(true) => parts.push("has a value".to_owned()),
            Some(false) => parts.push("is empty".to_owned()),
            None => {}
        }
        if !filter.values.is_empty() {
            let offered = self.offered(app, sheet, column);
            let named: Vec<&str> = offered
                .values
                .iter()
                .map(|(value, _)| value.as_str())
                .filter(|value| filter.values.contains(*value))
                .collect();
            let empty = filter.values.contains("");
            let includes = several(column);
            if named.is_empty() {
                parts.push("is empty".to_owned());
            } else {
                let more = named.len().saturating_sub(CHIP_VALUES);
                let mut words = format!(
                    "{} {}",
                    if includes { "includes" } else { "is" },
                    named[..named.len().min(CHIP_VALUES)].join(", ")
                );
                if more > 0 {
                    words.push_str(&format!(" and {more} more"));
                }
                if empty {
                    words.push_str(if more > 0 { "," } else { "" });
                    words.push_str(if includes {
                        " or is empty"
                    } else {
                        " or empty"
                    });
                }
                parts.push(words);
            }
        }
        if let Some(least) = &filter.least {
            parts.push(format!("at least {least}"));
        }
        if let Some(most) = &filter.most {
            parts.push(format!("at most {most}"));
        }
        format!("{} {}", column.label, parts.join(" and "))
    }

    /// Open `column`'s list, from the control `from` (its mark or its chip): an open cell
    /// closes, and the Columns list; the keys go to the list's first line.
    fn open_filter(&mut self, app: &mut TablesApp, name: &str, column: &str, from: &str) {
        self.close_edit(app, false);
        self.columns.open = false;
        let filter = self.filter_on(name, column);
        self.filters.least = TextInputState::new(filter.least.unwrap_or_default());
        self.filters.most = TextInputState::new(filter.most.unwrap_or_default());
        self.filters.open = Some(Open {
            column: column.to_owned(),
            from: from.to_owned(),
            kept: filter.values,
            at: Cell::new(None),
        });
        self.filters.scroll = 0.0;
        self.focus_filter_line(HAS);
    }

    /// Whether a column's list is open with the keys on it.
    pub(crate) fn filters_keyed(&self) -> bool {
        self.filters.open.is_some()
            && self
                .common
                .last_focus
                .as_ref()
                .is_some_and(|key| key.as_str().starts_with(LIST))
    }

    /// Put the keys on the list's line `key`.
    fn focus_filter_line(&mut self, key: &str) {
        self.filters.line = Some(key.to_owned());
        self.focus(key);
    }

    /// Close the open list; with `keys`, put them back on the control it opened from where the
    /// page still draws it, else on its column's mark, else on its chip, else in the find.
    fn close_filter(&mut self, app: &TablesApp, keys: bool) {
        let Some(open) = self.filters.open.take() else {
            return;
        };
        if !keys {
            return;
        }
        let mark = format!("{MARK}{}", open.column);
        let chip = format!("{CHIP}{}", open.column);
        let to = app
            .current(self)
            .and_then(|name| app.sheet(name))
            .and_then(|sheet| {
                let drawn = |key: &str| {
                    key == mark
                        && self
                            .shown_columns(app, sheet)
                            .iter()
                            .any(|c| c.name == open.column)
                        || key == chip && self.filters_of(sheet).contains_key(&open.column)
                };
                [open.from.as_str(), mark.as_str(), chip.as_str()]
                    .into_iter()
                    .find(|key| drawn(key))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| FIND.to_owned());
        self.focus(&to);
    }

    /// After a filter of table `name` changed: one that no longer leaves a row out goes, and the
    /// selected row is kept in view where it still shows, else the rows show from their top.
    fn refiltered(&mut self, app: &TablesApp, name: &str) {
        if let Some(filters) = self.filters.tables.get_mut(name) {
            filters.retain(|_, filter| filter.stands());
        }
        let Some(sheet) = app.sheet(name) else {
            return;
        };
        let at = self.row.as_ref().and_then(|pk| {
            self.shown_rows(app, sheet)
                .iter()
                .position(|row| row.pk == *pk)
        });
        match at {
            Some(at) => self.keep_in_view(at),
            None => self.scroll.set(0.0),
        }
    }

    /// Where the keys go once a press on `key`, a control the press takes away (a chip's close
    /// mark, `Clear filters`), was made: from the keyboard, to `next` where there is one, else
    /// the find; after a pointer's press, to the selected cell where the grid still shows it,
    /// else the find.
    fn rest_after(&mut self, app: &TablesApp, keyboard: bool, next: Option<String>) {
        let cell = self.row.clone().zip(self.column.clone()).filter(|(pk, _)| {
            app.current(self)
                .and_then(|name| app.sheet(name))
                .is_some_and(|sheet| self.shown_rows(app, sheet).iter().any(|r| r.pk == *pk))
        });
        let to = match (keyboard, next, cell) {
            (true, Some(next), _) => next,
            (false, _, Some((pk, column))) => cell_key(&pk, &column),
            _ => FIND.to_owned(),
        };
        self.focus(&to);
    }

    /// The open list's lines that take a press, top down: the two choices, the bounds where the
    /// column takes them, then each value offered and the empty rows' where any row is empty or
    /// the empty value is chosen or kept.
    fn filter_keys(&self, app: &TablesApp, sheet: &Sheet, column: &SheetColumn) -> Vec<String> {
        let mut keys = vec![HAS.to_owned(), EMPTY.to_owned()];
        if bounded(column) {
            keys.extend([LEAST.to_owned(), MOST.to_owned()]);
        }
        let offered = self.offered(app, sheet, column);
        keys.extend(offered.values.iter().map(|(value, _)| value_key(value)));
        if offered.blank {
            keys.push(BLANK.to_owned());
        }
        keys
    }

    /// After a read or a press: the open list goes where its column has gone, and keys left on
    /// a line of it the list no longer offers (a read took the last row holding its value) go to
    /// the line now standing where it stood, so Return presses a line the page draws, never its
    /// default button, Write. Whether anything moved.
    pub(crate) fn settle_filters(&mut self, app: &TablesApp) -> bool {
        let Some(column) = self.filters.open.as_ref().map(|open| open.column.clone()) else {
            return false;
        };
        let Some(sheet) = app.current(self).and_then(|name| app.sheet(name)) else {
            return false;
        };
        let focus = self
            .common
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_owned())
            .filter(|key| key.starts_with(LIST));
        let Some(found) = sheet.columns.iter().find(|c| c.name == column) else {
            self.close_filter(app, focus.is_some());
            return true;
        };
        let Some(focus) = focus.filter(|key| key != PANEL) else {
            return false;
        };
        let keys = self.filter_keys(app, sheet, found);
        if keys.contains(&focus) {
            return false;
        }
        let was = self
            .filters
            .lines
            .borrow()
            .iter()
            .position(|line| line.0 == focus)
            .unwrap_or(0);
        let to = keys
            .get(was.min(keys.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_else(|| HAS.to_owned());
        self.focus_filter_line(&to);
        true
    }

    /// From the list's line with the keys `by` lines down (up where negative), held at its
    /// first and its last line, scrolled into view where the list was laid out: the arrows step
    /// one, Page Up and Page Down a room's worth, Command-Home and Command-End to its ends.
    fn step_filter(&mut self, app: &TablesApp, by: i32) {
        let Some(sheet) = app.current(self).and_then(|name| app.sheet(name)) else {
            return;
        };
        let Some(column) = self
            .filters
            .open
            .as_ref()
            .and_then(|open| sheet.columns.iter().find(|c| c.name == open.column))
        else {
            return;
        };
        let keys = self.filter_keys(app, sheet, column);
        let at = self
            .common
            .last_focus
            .as_ref()
            .and_then(|focus| keys.iter().position(|key| key == focus.as_str()));
        let last = keys.len().saturating_sub(1);
        let steps = usize::try_from(by.unsigned_abs()).unwrap_or(usize::MAX);
        let to = match at {
            None => 0,
            Some(at) if by < 0 => at.saturating_sub(steps),
            Some(at) => at.saturating_add(steps).min(last),
        };
        let Some(key) = keys.get(to).cloned() else {
            return;
        };
        let line = self
            .filters
            .lines
            .borrow()
            .iter()
            .find(|line| line.0 == key)
            .map(|line| (line.1, line.2));
        if let Some((top, bottom)) = line {
            self.reveal_filter_line(top, bottom);
        }
        self.focus_filter_line(&key);
        self.common.focus_visible = true;
    }

    /// Scroll a line of the open list, or a column's mark, named by `key` into view: whether
    /// anything moved; `None` for a key of neither.
    pub(crate) fn reveal_filters_control(&mut self, key: &str) -> Option<bool> {
        if let Some(column) = key.strip_prefix(MARK) {
            let across = self.across;
            self.reveal(column);
            return Some(across != self.across);
        }
        if !key.starts_with(LIST) {
            return None;
        }
        let line = self
            .filters
            .lines
            .borrow()
            .iter()
            .find(|line| line.0 == key)
            .map(|line| (line.1, line.2));
        let was = self.filters.scroll;
        if let Some((top, bottom)) = line {
            self.reveal_filter_line(top, bottom);
        }
        Some(was != self.filters.scroll)
    }

    /// Scroll the list so the line from `top` to `bottom` shows whole.
    fn reveal_filter_line(&mut self, top: f32, bottom: f32) {
        let room = self.filters.room.get();
        let scroll = &mut self.filters.scroll;
        if top < *scroll {
            *scroll = top;
        } else if room > 0.0 && bottom > *scroll + room {
            *scroll = bottom - room;
        }
        *scroll = scroll.clamp(0.0, self.filters.limit.get());
    }

    /// The open list's bound `key` ([`LEAST`] or [`MOST`]) set from its field: empty, or words
    /// that do not read as the column's kind ([`Point::of`]: the field is then marked invalid),
    /// none. A bound set ends `Only rows that are empty`, which no row it bounds can meet.
    fn bound(&mut self, app: &TablesApp, name: &str, key: &str) {
        let Some(column) = self.filters.open.as_ref().map(|open| open.column.clone()) else {
            return;
        };
        let Some(kind) = app
            .sheet(name)
            .and_then(|sheet| sheet.columns.iter().find(|c| c.name == column))
            .map(|c| c.kind)
        else {
            return;
        };
        let typed = if key == LEAST {
            &self.filters.least
        } else {
            &self.filters.most
        };
        let typed = typed.value().trim();
        let bound = Point::of(kind, typed).map(|_| typed.to_owned());
        let filter = self.filter_mut(name, &column);
        if bound.is_some() && filter.has == Some(false) {
            filter.has = None;
        }
        if key == LEAST {
            filter.least = bound;
        } else {
            filter.most = bound;
        }
        self.refiltered(app, name);
    }
}

/// The filters' share of the page's reducer ([`TablesApp::act`]), asked before the Columns
/// list's: `Some` where it took the event whole, with whether anything changed; `None` where the
/// page goes on with it. A press of anything else, or the keys leaving for another control,
/// closes the open list first; a press of the filters' closes the Columns list.
#[expect(
    clippy::too_many_lines,
    reason = "one reducer, each arm a part of the filters"
)]
pub(crate) fn act(
    app: &mut TablesApp,
    view: &mut TablesViewState,
    event: &AppEvent,
) -> Option<bool> {
    let on_list = |view: &TablesViewState| {
        view.common
            .last_focus
            .as_ref()
            .is_some_and(|key| key.as_str().starts_with(LIST))
    };
    match event {
        AppEvent::Action(action) => {
            let key = action.id.as_str();
            let ours = key.starts_with(MARK)
                || key.starts_with(LIST)
                || key.starts_with(CHIP)
                || key.starts_with(REMOVE)
                || key == CLEAR
                || key == CLEAR_NONE;
            if !ours {
                // A press anywhere else closes the list, and the page takes the press. Where the
                // keys were on the list (an action no press made: a paste, an assistive
                // technology's) they go back where it opened from: keys on a line the page no
                // longer draws would reach its default button, Write, at Return.
                if view.filters.open.is_some() {
                    let keys = on_list(view);
                    view.close_filter(app, keys);
                }
                return None;
            }
            let name = app.current(view)?;
            let sheet = app.sheet(name)?;
            view.columns.open = false;
            if let Some(column) = key.strip_prefix(MARK).or_else(|| key.strip_prefix(CHIP)) {
                sheet.columns.iter().find(|c| c.name == column)?;
                let again = view
                    .filters
                    .open
                    .as_ref()
                    .is_some_and(|open| open.column == column && open.from == key);
                if again {
                    view.close_filter(app, true);
                } else {
                    view.open_filter(app, name, column, key);
                }
                return Some(true);
            }
            if let Some(column) = key.strip_prefix(REMOVE) {
                let keyboard = view.keyboard_on(key);
                let chips: Vec<String> = view
                    .standing(sheet)
                    .iter()
                    .map(|(c, _)| c.name.clone())
                    .collect();
                let at = chips.iter().position(|c| c == column)?;
                let next = chips
                    .get(at + 1)
                    .or_else(|| at.checked_sub(1).and_then(|before| chips.get(before)))
                    .map(|next| format!("{REMOVE}{next}"));
                if let Some(filters) = view.filters.tables.get_mut(name) {
                    filters.remove(column);
                }
                if view
                    .filters
                    .open
                    .as_ref()
                    .is_some_and(|open| open.column == column)
                {
                    view.filters.open = None;
                }
                view.refiltered(app, name);
                view.rest_after(app, keyboard, next);
                return Some(true);
            }
            if key == CLEAR || key == CLEAR_NONE {
                let keyboard = view.keyboard_on(key);
                view.filters.clear(name);
                view.filters.open = None;
                view.refiltered(app, name);
                view.rest_after(app, keyboard, None);
                return Some(true);
            }
            // The list's own lines.
            let column = view.filters.open.as_ref()?.column.clone();
            let found = sheet.columns.iter().find(|c| c.name == column)?;
            match key {
                // A press on the list's words or its frame is the list's, and leaves the keys on
                // the line they were on.
                PANEL => {
                    let line = view.filters.line.clone();
                    view.focus(line.as_deref().unwrap_or(HAS));
                    return Some(true);
                }
                LEAST | MOST if bounded(found) => {
                    let input = if key == LEAST {
                        &mut view.filters.least
                    } else {
                        &mut view.filters.most
                    };
                    match action.value.as_ref() {
                        Some(SemanticInput::Text(value)) => {
                            *input = TextInputState::new(value.clone());
                            view.bound(app, name, key);
                        }
                        Some(SemanticInput::TextPosition { byte, extend }) => {
                            let anchor = if *extend {
                                input.selection().anchor
                            } else {
                                *byte
                            };
                            input.set_selection(anchor, *byte);
                        }
                        _ => {}
                    }
                    view.focus_filter_line(key);
                    return Some(true);
                }
                HAS => {
                    let filter = view.filter_mut(name, &column);
                    filter.has = (filter.has != Some(true)).then_some(true);
                    filter.values.clear();
                }
                EMPTY => {
                    let filter = view.filter_mut(name, &column);
                    filter.has = (filter.has != Some(false)).then_some(false);
                    if filter.has.is_some() {
                        filter.values.clear();
                        filter.least = None;
                        filter.most = None;
                        view.filters.least = TextInputState::new(String::new());
                        view.filters.most = TextInputState::new(String::new());
                    }
                }
                _ => {
                    let value = if key == BLANK {
                        String::new()
                    } else {
                        value_of_key(key)?
                    };
                    let filter = view.filter_mut(name, &column);
                    if !filter.values.remove(&value) {
                        filter.values.insert(value.clone());
                    }
                    filter.has = None;
                    // Unchosen, its line stays while the list is open, the keys on it.
                    if let Some(open) = view.filters.open.as_mut() {
                        open.kept.insert(value);
                    }
                }
            }
            view.refiltered(app, name);
            view.focus_filter_line(key);
            Some(true)
        }
        AppEvent::FocusChanged(focus) if view.filters.open.is_some() => {
            let from = view.filters.open.as_ref().map(|open| open.from.as_str());
            let stays = focus
                .as_ref()
                .is_some_and(|key| key.as_str().starts_with(LIST) || Some(key.as_str()) == from);
            if !stays {
                view.filters.open = None;
            }
            None
        }
        AppEvent::TextInput(TextInputEvent::Cancel)
            if view.filters.open.is_some() && on_list(view) =>
        {
            view.close_filter(app, true);
            Some(true)
        }
        // A bound's field takes typing; the list's other lines keep the keys.
        AppEvent::TextInput(event) if view.filters.open.is_some() && on_list(view) => {
            let focus = view
                .common
                .last_focus
                .as_ref()
                .map(|key| key.as_str().to_owned());
            let name = app.current(view)?;
            match focus.as_deref() {
                Some(key @ (LEAST | MOST)) if !matches!(event, TextInputEvent::Submit) => {
                    let input = if key == LEAST {
                        &mut view.filters.least
                    } else {
                        &mut view.filters.most
                    };
                    edit(input, event.clone());
                    view.bound(app, name, key);
                }
                _ => {}
            }
            Some(true)
        }
        AppEvent::StepFocus(by) if view.filters.open.is_some() && on_list(view) => {
            view.step_filter(app, *by);
            Some(true)
        }
        // Page Up and Page Down move the keys a room's worth of lines, Command-Home and
        // Command-End to its ends: the list's, never the grid under it.
        AppEvent::ScrollPage(pages) if view.filters.open.is_some() && on_list(view) => {
            let room = view.filters.room.get();
            let page = (room / row_height()).floor().max(1.0) as i32;
            view.step_filter(app, pages.saturating_mul(page));
            Some(true)
        }
        AppEvent::ScrollLines(lines) if view.filters.open.is_some() && on_list(view) => {
            view.step_filter(app, *lines);
            Some(true)
        }
        // Shift and an arrow grow the grid's range, which is not the list's: the list keeps the
        // keys and the selection stays.
        AppEvent::ExtendFocus(_) if view.filters.open.is_some() && on_list(view) => Some(true),
        // Tab leaves the list, one stop of Tab's: it closes with the keys back on the control it
        // opened from, and the window goes on from there (`App::native_input_event`).
        AppEvent::StepAcross(_) if view.filters.open.is_some() && on_list(view) => {
            view.close_filter(app, true);
            Some(true)
        }
        AppEvent::ScrollAt { lines, key } if key.as_str().starts_with(LIST) => {
            view.filters.scroll = (view.filters.scroll + *lines as f32 * row_height())
                .clamp(0.0, view.filters.limit.get());
            Some(true)
        }
        _ => None,
    }
}

/// A press the size of its parent under painted words, `spoken` its name, that opens a list and
/// says whether it is `open`: a column's mark, a chip.
fn opener(key: &str, spoken: String, open: bool) -> UiNode {
    UiNode::new(
        key.to_owned(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(spoken).visual_label(String::new()),
                ActionId::new(key.to_owned()),
            )
            .state(ControlState {
                enabled: true,
                expanded: Some(open),
                ..ControlState::default()
            })
            .style(StyleRef::Navigation),
        ),
    )
}

/// A column's mark at the right end of its header, before the room where its edge is held: a
/// press of its own, spoken `Filter Role` (`, filtered` while a filter stands on the column),
/// that opens the column's list. Its face is a drawn funnel ([`StyleRef::Filter`], in the
/// primary ink while a filter stands, [`StyleRef::Filtered`]): a shape no sort's mark has. Open,
/// its seat is washed as a range is; the funnel stays as it is.
pub(crate) fn mark(view: &TablesViewState, sheet: &Sheet, column: &SheetColumn) -> UiNode {
    let key = format!("{MARK}{}", column.name);
    let filtered = view
        .filters
        .tables
        .get(sheet.name)
        .and_then(|filters| filters.get(&column.name))
        .is_some_and(ColumnFilter::stands);
    let open = view
        .filters
        .open
        .as_ref()
        .is_some_and(|open| open.column == column.name && open.from == key);
    let mut spoken = format!("Filter {}", field_words(column));
    if filtered {
        spoken.push_str(", filtered");
    }
    let mut seat = vec![opener(&key, spoken, open)];
    if open {
        seat.push(styled(format!("{key}/open"), StyleRef::CellRange).paint_only());
    }
    seat.push(
        styled(
            format!("{key}/glyph"),
            if filtered {
                StyleRef::Filtered
            } else {
                StyleRef::Filter
            },
        )
        .paint_only(),
    );
    group(format!("{key}/lane"))
        .layout(Layout::row())
        .children(vec![
            group(format!("{key}/lead")).layout(Layout::default().width(Length::Fill)),
            group(format!("{key}/seat"))
                .layout(Layout::default().width(Length::Fixed(FILTER_MARK)))
                .children(seat),
            group(format!("{key}/space"))
                .layout(Layout::default().width(Length::Fixed(MARK_SPACE))),
        ])
}

/// How far a header's mark stands in from the header's right end: the grid notes each mark's
/// right edge ([`FiltersView::laid_marks`]) as the header's less this.
pub(crate) const fn mark_inset() -> f32 {
    MARK_SPACE
}

/// A ruled seat `width` wide and `height` tall on the sheet's raised ground: its top and left
/// rules, then the ground, which rules its own right and bottom.
fn framed(key: &str, (width, height): (f32, f32), children: Vec<UiNode>) -> UiNode {
    group(format!("{key}/frame"))
        .layout(
            Layout::column()
                .width(Length::Fixed(width))
                .height(Length::Fixed(height)),
        )
        .children(vec![
            styled(format!("{key}/top"), StyleRef::Cell)
                .layout(Layout::default().height(Length::Fixed(1.0))),
            group(format!("{key}/sides"))
                .layout(Layout::row())
                .children(vec![
                    styled(format!("{key}/left"), StyleRef::Cell)
                        .layout(Layout::default().width(Length::Fixed(1.0))),
                    styled(format!("{key}/ground"), StyleRef::CellHead)
                        .layout(
                            Layout::default()
                                .width(Length::Fixed((width - 1.0).max(1.0)))
                                .height(Length::Fixed((height - 1.0).max(1.0))),
                        )
                        .children(children),
                ]),
        ])
}

/// A press `width` wide the height of its parent, under painted words, `spoken` its name.
fn sized_press(key: &str, spoken: String, width: f32) -> UiNode {
    line_press(key, spoken, true).layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fill),
    )
}

/// One chip `width` wide and `height` tall: the filter on `column` in `words`, a press that opens
/// the column's list (and says whether the list it opened is `open`), and at its right end the
/// close mark that takes the filter away.
fn chip(column: &SheetColumn, words: &str, (width, height): (f32, f32), open: bool) -> UiNode {
    let key = format!("{CHIP}{}", column.name);
    let remove = format!("{REMOVE}{}", column.name);
    let inner = (width - 1.0).max(1.0);
    let close_w = text_width("\u{d7}", SemanticRole::Text, StyleRef::Note);
    let words_w = (inner - 10.0 - LINE_GAP - CLOSE_MARK).max(1.0);
    framed(
        &key,
        (width, height),
        vec![
            opener(&key, format!("{words}; change this filter"), open),
            group(format!("{key}/face"))
                .layout(Layout::row().gap(LINE_GAP).padding(Insets {
                    left: 10.0,
                    right: (CLOSE_MARK - close_w) / 2.0,
                    ..Insets::default()
                }))
                .children(vec![
                    fitted(format!("{key}/words"), words, StyleRef::Primary, words_w),
                    group(format!("{key}/lead")).layout(Layout::default().width(Length::Fill)),
                    fitted(format!("{key}/close"), "\u{d7}", StyleRef::Note, close_w),
                ])
                .paint_only_tree(),
            group(format!("{key}/remove-lane"))
                .layout(Layout::row())
                .children(vec![
                    group(format!("{key}/remove-lead"))
                        .layout(Layout::default().width(Length::Fill)),
                    sized_press(&remove, format!("Remove filter, {words}"), CLOSE_MARK),
                ]),
        ],
    )
}

/// The widths `wants` fitted to `room`: where they fit, each its own; else each takes its share
/// of what is left as they are taken narrowest first, and none narrower than `least` (the line
/// then cuts what runs past its end).
fn shares(wants: &[f32], room: f32, least: f32) -> Vec<f32> {
    if wants.iter().sum::<f32>() <= room {
        return wants.to_vec();
    }
    let mut widths = wants.to_vec();
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by(|a, b| wants[*a].total_cmp(&wants[*b]));
    let mut left = room;
    for (taken, at) in order.iter().enumerate() {
        let share = left / (wants.len() - taken) as f32;
        widths[*at] = wants[*at].min(share).max(least);
        left -= widths[*at];
    }
    widths
}

/// The chips `wants` wide (each at most `width`), in their order, on lines `width` wide, `gap`
/// apart, with `Clear filters` `clear` wide (where it stands) at the last line's end after a
/// gap: each line takes the chips that fit after the one before, up to [`CHIP_LINES`] lines, and
/// the last takes every chip left. `Clear filters` takes a line of its own where the last has
/// no room for it and the lines are not all taken. Each line's chips, by their place in
/// `wants`.
fn wrap(wants: &[f32], clear: Option<f32>, width: f32, gap: f32) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = vec![Vec::new()];
    let mut x = 0.0_f32;
    for (n, want) in wants.iter().enumerate() {
        let w = want.min(width);
        let started = lines.last().is_some_and(|line| !line.is_empty());
        if started && x + gap + w > width && lines.len() < CHIP_LINES {
            lines.push(Vec::new());
            x = 0.0;
        }
        if lines.last().is_some_and(|line| !line.is_empty()) {
            x += gap;
        }
        x += w;
        if let Some(line) = lines.last_mut() {
            line.push(n);
        }
    }
    if let Some(clear) = clear
        && x + 2.0 * gap + clear > width
        && lines.len() < CHIP_LINES
    {
        lines.push(Vec::new());
    }
    lines
}

/// The lines under the tools row of the filters that stand on `sheet`, `width` wide, each
/// `line` tall: a chip for each, in the grid's order of their columns, and `Clear filters` at
/// the last line's end where two or more stand ([`wrap`]). On a line short of room its chips
/// give way, to [`CHIP_LEAST`] where it holds them and below where it does not, so no chip's
/// close mark and no `Clear filters` runs past the line's end. With how tall the lines are;
/// `None` where no filter stands, and then the page gives them no room.
pub(crate) fn chips(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    width: f32,
    line: f32,
) -> Option<(UiNode, f32)> {
    let standing = view.standing(sheet);
    if standing.is_empty() {
        view.filters.chips.borrow_mut().clear();
        return None;
    }
    let words: Vec<String> = standing
        .iter()
        .map(|(column, filter)| view.chip_words(app, sheet, column, filter))
        .collect();
    let clear_w = (standing.len() >= 2)
        .then(|| crate::native_ui::native_button_label_width("Clear filters") + 24.0);
    let wants: Vec<f32> = words
        .iter()
        .map(|words| {
            (1.0 + 10.0
                + text_width(words, SemanticRole::Text, StyleRef::Primary)
                + LINE_GAP
                + CLOSE_MARK)
                .min(width)
        })
        .collect();
    let lines = wrap(&wants, clear_w, width, CHIP_GAP);
    let chip_h = (line - 6.0).max(1.0);
    let open = view
        .filters
        .open
        .as_ref()
        .and_then(|open| open.from.strip_prefix(CHIP).map(str::to_owned));
    let mut placed = Vec::with_capacity(standing.len());
    let mut nodes = Vec::with_capacity(lines.len());
    for (n, members) in lines.iter().enumerate() {
        let clear_here = clear_w.filter(|_| n + 1 == lines.len());
        // Its chips, the gaps between them, and `Clear filters` with the gap before it and the
        // one before the lead that holds it at the line's end.
        let room = (width
            - CHIP_GAP * members.len().saturating_sub(1) as f32
            - clear_here.map_or(0.0, |w| {
                w + CHIP_GAP + if members.is_empty() { 0.0 } else { CHIP_GAP }
            }))
        .max(1.0);
        let line_wants: Vec<f32> = members.iter().map(|at| wants[*at]).collect();
        let least = (CHIP_LEAST * scale().max(1.0)).min(room / members.len().max(1) as f32);
        let widths = shares(&line_wants, room, least);
        let foot = (n + 1) as f32 * line;
        let mut children = Vec::with_capacity(members.len() + 2);
        let mut at = 0.0;
        for (member, w) in members.iter().zip(&widths) {
            let (column, _) = &standing[*member];
            placed.push((column.name.clone(), at, foot));
            at += w + CHIP_GAP;
            children.push(chip(
                column,
                &words[*member],
                (*w, chip_h),
                open.as_deref() == Some(column.name.as_str()),
            ));
        }
        if let Some(clear_w) = clear_here {
            children.push(group("tables/chips/lead").layout(Layout::default().width(Length::Fill)));
            let mut clear = button(
                CLEAR,
                "Clear filters",
                &format!("Clear filters, all {}", standing.len()),
                StyleRef::Secondary,
                true,
            );
            clear.layout.width = Length::Fixed(clear_w);
            children.push(clear);
        }
        nodes.push(
            group(format!("tables/chips/line/{n}"))
                .layout(
                    Layout::row()
                        .gap(CHIP_GAP)
                        .width(Length::Fixed(width))
                        .height(Length::Fixed(line))
                        .padding(Insets::symmetric(0.0, 3.0))
                        .clipped(),
                )
                .children(children),
        );
    }
    *view.filters.chips.borrow_mut() = placed;
    let tall = lines.len() as f32 * line;
    Some((
        group("tables/chips")
            .layout(
                Layout::column()
                    .width(Length::Fixed(width))
                    .height(Length::Fixed(tall)),
            )
            .children(nodes),
        tall,
    ))
}

/// What the grid says where no row passes the filters, and its `Clear filters`.
pub(crate) fn none_pass(width: f32, line: f32) -> UiNode {
    let words = "No row passes these filters";
    let clear_w = crate::native_ui::native_button_label_width("Clear filters") + 24.0;
    let words_w = text_width(words, SemanticRole::Text, StyleRef::Note)
        .min(width - 20.0 - 12.0 - clear_w)
        .max(1.0);
    let mut clear = button(
        CLEAR_NONE,
        "Clear filters",
        "Clear filters",
        StyleRef::Secondary,
        true,
    );
    clear.layout.width = Length::Fixed(clear_w);
    group("tables/rows/none/seat")
        .layout(
            Layout::row()
                .gap(12.0)
                .width(Length::Fixed(width))
                .height(Length::Fixed(line))
                .padding(Insets {
                    left: 10.0,
                    top: 2.0,
                    bottom: 2.0,
                    right: 0.0,
                }),
        )
        .children(vec![
            fitted("tables/rows/none", words, StyleRef::Note, words_w),
            clear,
        ])
}

/// One line of the open list `width` wide and `line` tall: its press over its painted words
/// `face`, the keys' ring where they are on it. `key` names the press, and `part` each of the
/// line's own nodes (`part("seat")`), apart from any value's key.
fn list_line(
    view: &TablesViewState,
    key: &str,
    part: &dyn Fn(&str) -> String,
    spoken: String,
    face: Vec<UiNode>,
    (width, line): (f32, f32),
) -> UiNode {
    let mut seat = vec![
        line_press(key, spoken, true),
        group(part("face"))
            .layout(
                Layout::row()
                    .gap(LINE_GAP)
                    .padding(Insets::symmetric(10.0, 0.0)),
            )
            .children(face)
            .paint_only_tree(),
    ];
    if view.common.focus_visible
        && view
            .common
            .last_focus
            .as_ref()
            .is_some_and(|focus| focus.as_str() == key)
    {
        seat.push(styled(part("ring"), StyleRef::CellSelected).paint_only());
    }
    group(part("seat"))
        .layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fixed(line)),
        )
        .children(seat)
}

/// `1 row`, `24 rows`.
fn rows_count(n: usize) -> String {
    if n == 1 {
        "1 row".to_owned()
    } else {
        format!("{n} rows")
    }
}

/// What a bound's field shows while it is empty: the form the column's values take.
fn bound_hint(column: &SheetColumn) -> &'static str {
    match (column.kind, column.compiled.map(|c| &c.ty)) {
        (ColumnKind::Number, _) => "a whole number",
        (_, Some(Ty::Day)) => "a date, as 2026-11-16",
        _ => "2026-11-16 or 2026-11-16 14:05",
    }
}

/// A bound's field, `label` its name, its value `input`: marked invalid where it holds words
/// that do not read as the column's kind, which then bound nothing.
fn bound_field(
    view: &TablesViewState,
    key: &str,
    label: &str,
    input: &TextInputState,
    column: &SheetColumn,
) -> UiNode {
    let typed = input.value().trim();
    UiNode::new(
        key.to_owned(),
        UiContent::TextField(
            Control::new(
                TextFieldSpec {
                    label: label.into(),
                    placeholder: Some(bound_hint(column).into()),
                    secret: false,
                    visual_value: None,
                    input: Some(input.projection()),
                    swatch: None,
                    swatch_line: false,
                    trailing_hint: None,
                    description: None,
                    end_button: false,
                },
                ActionId::new(key.to_owned()),
            )
            .value(SemanticValue::Text(input.value().into()))
            .state(ControlState {
                enabled: true,
                focused: view
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|k| k.as_str() == key),
                invalid: !typed.is_empty() && Point::of(column.kind, typed).is_none(),
                ..ControlState::default()
            })
            .style(StyleRef::Secondary),
        ),
    )
}

/// The open list of a column's values, laid over the page's main column between `left` and
/// `right`: under the column's mark, its right edge at the mark's, where it opened from the
/// mark (`head` the foot of the grid's header line, `grid_left` the grid's left); under its chip,
/// its left edge at the chip's, where it opened from the chip (`chips` the top of the chips'
/// lines); and no lower than `foot`. It stays where it was first laid out while it is open and
/// `right` does not move ([`Open::at`]). Where its lines are taller than its room they scroll
/// under its heading. `None` where no list is open or its column is gone.
#[expect(clippy::too_many_lines, reason = "one list, laid out in one pass")]
pub(crate) fn list(
    app: &TablesApp,
    view: &TablesViewState,
    sheet: &Sheet,
    (left, right, grid_left): (f32, f32, f32),
    (head, chips, foot): (f32, Option<f32>, f32),
) -> Option<UiNode> {
    let open = view.filters.open.as_ref()?;
    let column = sheet.columns.iter().find(|c| c.name == open.column)?;
    let line = row_height();
    let caption = 22.0 * scale().max(1.0);
    let width = (LIST_WIDTH * scale().max(1.0)).min(right - left).max(1.0);
    // Where it stands: where it was first laid out; else under the mark, its right edge the
    // mark's; under the chip, its left edge the chip's; else at the main column's right.
    let (x, top) = match open.at.get() {
        Some((x, top, was)) if was == right => (x, top),
        _ => {
            let mark = view.filters.marks.borrow().get(&column.name).copied();
            let chip = view
                .filters
                .chips
                .borrow()
                .iter()
                .find(|(name, ..)| *name == column.name)
                .map(|(_, at, foot)| (*at, *foot));
            let (x, top) = match (open.from.starts_with(CHIP), chip, chips, mark) {
                (true, Some((at, under)), Some(chips), _) => (left + at, chips + under + 2.0),
                (_, _, _, Some(mark)) => (grid_left + mark - width, head),
                _ => (right - width, head),
            };
            open.at.set(Some((x, top, right)));
            (x, top)
        }
    };
    let x = x.clamp(left, (right - width).max(left));
    // The words' room on a line: the ground's width (the frame's left rule off the list's) less
    // the line's padding at each end.
    let inner = width - 1.0 - 20.0;
    let gaps = |n: usize| LINE_GAP * n.saturating_sub(1) as f32;
    let filter = view.filter_on(sheet.name, &column.name);
    let offered = view.offered(app, sheet, column);
    let mark_w = text_width("\u{2713}", SemanticRole::Text, StyleRef::Primary);
    let tick = |key: String, on: bool| {
        fitted(
            key,
            if on { "\u{2713}" } else { "" },
            StyleRef::Primary,
            mark_w,
        )
    };
    let mut lines = Vec::new();
    let mut body = Vec::new();
    let mut tall = 0.0_f32;
    // The two one-press choices.
    for (key, words, on, n) in [
        (
            HAS,
            "Only rows with a value",
            filter.has == Some(true),
            offered.with,
        ),
        (
            EMPTY,
            "Only rows that are empty",
            filter.has == Some(false),
            offered.empty,
        ),
    ] {
        let count = n.to_string();
        let count_w = text_width(&count, SemanticRole::Text, StyleRef::Note);
        let part = |part: &str| format!("{key}/{part}");
        body.push(list_line(
            view,
            key,
            &part,
            format!(
                "{words}, {}, {}",
                if on { "on" } else { "off" },
                rows_count(n)
            ),
            vec![
                tick(part("tick"), on),
                fitted(
                    part("words"),
                    words,
                    StyleRef::Primary,
                    (inner - mark_w - count_w - gaps(4)).max(1.0),
                ),
                group(part("lead")).layout(Layout::default().width(Length::Fill)),
                fitted(part("count"), &count, StyleRef::Note, count_w),
            ],
            (width, line),
        ));
        lines.push((key.to_owned(), tall, tall + line));
        tall += line;
    }
    // A number's or a time's bounds.
    if bounded(column) {
        let words_w = ["At least", "At most"]
            .iter()
            .map(|w| text_width(w, SemanticRole::Text, StyleRef::Primary))
            .fold(0.0_f32, f32::max);
        for (key, words, input) in [
            (LEAST, "At least", &view.filters.least),
            (MOST, "At most", &view.filters.most),
        ] {
            body.push(
                group(format!("{key}/seat"))
                    .layout(
                        Layout::row()
                            .gap(LINE_GAP)
                            .width(Length::Fixed(width))
                            .height(Length::Fixed(line))
                            .padding(Insets::symmetric(10.0, 1.0)),
                    )
                    .children(vec![
                        fitted(format!("{key}/words"), words, StyleRef::Primary, words_w),
                        bound_field(view, key, words, input, column).layout(
                            Layout::default()
                                .width(Length::Fixed((inner - words_w - LINE_GAP).max(1.0)))
                                .height(Length::Fill),
                        ),
                    ]),
            );
            lines.push((key.to_owned(), tall, tall + line));
            tall += line;
        }
    }
    // The values, under their word.
    {
        let word_key = format!("{LIST}values");
        let note = "rows";
        let note_w = text_width(note, SemanticRole::Text, StyleRef::Note);
        body.push(
            group(word_key.clone())
                .layout(
                    Layout::row()
                        .width(Length::Fixed(width))
                        .height(Length::Fixed(caption))
                        .padding(Insets {
                            left: 10.0,
                            right: 10.0,
                            top: 6.0,
                            bottom: 0.0,
                        }),
                )
                .children(vec![
                    fitted(
                        format!("{word_key}/word"),
                        "VALUES",
                        StyleRef::Note,
                        (inner * 0.5).max(1.0),
                    ),
                    group(format!("{word_key}/lead")).layout(Layout::default().width(Length::Fill)),
                    fitted(format!("{word_key}/note"), note, StyleRef::Note, note_w),
                ]),
        );
        tall += caption;
    }
    let mut values: Vec<(String, String, usize)> = offered
        .values
        .iter()
        .map(|(value, n)| (value_key(value), value.clone(), *n))
        .collect();
    if offered.blank {
        values.push((BLANK.to_owned(), String::new(), offered.empty));
    }
    for (n, (key, value, count)) in values.iter().enumerate() {
        let on = filter.values.contains(value);
        let shown = if value.is_empty() {
            EMPTY_WORDS
        } else {
            value.as_str()
        };
        let count_words = count.to_string();
        let count_w = text_width(&count_words, SemanticRole::Text, StyleRef::Note);
        // Each part of a value's line is named by the line's place, never by the value, which
        // may hold any words.
        let part = |part: &str| format!("{LIST}line/{n}/{part}");
        body.push(list_line(
            view,
            key,
            &part,
            format!(
                "{shown}, {}, {}",
                if on { "chosen" } else { "not chosen" },
                rows_count(*count)
            ),
            vec![
                tick(part("tick"), on),
                fitted(
                    part("words"),
                    shown,
                    if value.is_empty() {
                        StyleRef::Note
                    } else {
                        StyleRef::Primary
                    },
                    (inner - mark_w - count_w - gaps(4)).max(1.0),
                ),
                group(part("lead")).layout(Layout::default().width(Length::Fill)),
                fitted(part("count"), &count_words, StyleRef::Note, count_w),
            ],
            (width, line),
        ));
        lines.push((key.clone(), tall, tall + line));
        tall += line;
    }
    // How many values it does not offer, and what it writes.
    let mut closing = Vec::new();
    if offered.more > 0 {
        closing.push(match offered.more {
            1 => "1 more value, in fewer rows, is not listed".to_owned(),
            n => format!("{n} more values, each in fewer rows, are not listed"),
        });
    }
    closing.push("What you filter here is this view's. It writes nothing.".to_owned());
    // Each on as many lines as its words take: the list is too narrow to cut them short.
    for (n, words) in closing.iter().enumerate() {
        let key = if n + 1 == closing.len() {
            format!("{LIST}note")
        } else {
            format!("{LIST}more")
        };
        let note =
            crate::native_about::wrapped(&key, words, inner, SemanticRole::Text, StyleRef::Note);
        let seat_h = match note.layout.height {
            Length::Fixed(h) => h,
            _ => caption,
        } + 6.0;
        body.push(
            group(format!("{key}/seat"))
                .layout(
                    Layout::row()
                        .width(Length::Fixed(width))
                        .height(Length::Fixed(seat_h))
                        .padding(Insets {
                            left: 10.0,
                            right: 10.0,
                            top: 4.0,
                            bottom: 2.0,
                        }),
                )
                .children(vec![note]),
        );
        tall += seat_h;
    }
    *view.filters.lines.borrow_mut() = lines;
    // The heading stays; the lines under it scroll where the room is short of them.
    let head_h = line;
    let room = (foot - top - head_h - 2.0).max(line);
    let body_h = tall.min(room);
    let limit = (tall - body_h).max(0.0);
    view.filters.room.set(body_h);
    view.filters.limit.set(limit);
    let scroll = view.filters.scroll.min(limit);
    let distinct = offered.values.len() + offered.more;
    let counted = match distinct {
        1 => "1 value".to_owned(),
        n => format!("{n} values"),
    };
    let counted_w = text_width(&counted, SemanticRole::Text, StyleRef::Note);
    let title = format!("Filter {}", field_words(column));
    let heading = group(format!("{LIST}head"))
        .layout(
            Layout::row()
                .gap(LINE_GAP)
                .width(Length::Fixed(width))
                .height(Length::Fixed(head_h))
                .padding(Insets::symmetric(10.0, 0.0)),
        )
        .children(vec![
            UiNode::new(
                format!("{LIST}head/title"),
                UiContent::Text(TextSpec {
                    text: title.clone(),
                    role: SemanticRole::Heading,
                    style: StyleRef::Primary,
                }),
            )
            .layout(Layout::default().width(Length::Fill)),
            fitted(
                format!("{LIST}head/count"),
                &counted,
                StyleRef::Note,
                counted_w,
            ),
        ]);
    let lines_node = group(format!("{LIST}lines"))
        .layout(
            Layout::column()
                .width(Length::Fixed(width))
                .height(Length::Fixed(body_h))
                .clipped()
                .scrolled(scroll),
        )
        .room(body_h, limit > 0.0)
        .lists_every_row()
        .children(body);
    let height = head_h + body_h + 2.0;
    // Ruled round, on the sheet's raised ground. The whole of it is a surface over the page: a
    // press on its words or its frame, and the wheel over it, are the list's.
    let panel = UiNode::new(
        PANEL,
        UiContent::Group(GroupSpec::new(format!("{title}: its values"))),
    )
    .surface(ActionId::new(PANEL))
    .layout(
        Layout::column()
            .width(Length::Fixed(width))
            .height(Length::Fixed(height)),
    )
    .children(vec![
        styled(format!("{LIST}panel/top"), StyleRef::Cell)
            .layout(Layout::default().height(Length::Fixed(1.0))),
        group(format!("{LIST}panel/sides"))
            .layout(Layout::row())
            .children(vec![
                styled(format!("{LIST}panel/left"), StyleRef::Cell)
                    .layout(Layout::default().width(Length::Fixed(1.0))),
                styled(format!("{LIST}panel/ground"), StyleRef::CellHead)
                    .layout(Layout::column())
                    .children(vec![heading, lines_node]),
            ]),
    ]);
    Some(
        group(format!("{LIST}over"))
            .layout(Layout::column())
            .children(vec![
                group(format!("{LIST}above"))
                    .layout(Layout::default().height(Length::Fixed(top.max(0.0)))),
                group(format!("{LIST}line"))
                    .layout(Layout::row().height(Length::Fixed(height)))
                    .children(vec![
                        group(format!("{LIST}before"))
                            .layout(Layout::default().width(Length::Fixed(x.max(0.0)))),
                        panel,
                    ]),
            ]),
    )
}
