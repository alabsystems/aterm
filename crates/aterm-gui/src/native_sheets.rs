// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A set of sheets and an hourly chart, each drawn from a computed relation's schema
//! (docs/PLAN.md TK-35, on LT-C8's [`aterm_db::relation`]): a file of no one page, which a page
//! hands relations and the words that name them ([`Relation::of`]). It names no product and no
//! table.
//!
//! - **Columns from the schema.** A sheet's columns are its schema's, in order: each head says
//!   its column's name ([`crate::tables_store::label_of`], the Tables page's spelling) over its
//!   type's words ([`crate::native_tables_choices::ty_words`]: `whole number`, `one of 3`,
//!   `time`, `key · text` for a column of the relation's key), and is heard with what the
//!   column holds, its schema's words. A column is as wide as its words and a sample of its
//!   type need, never as its values do, so a new read moves no column.
//! - **Rows in the relation's order.** Nothing sorts them: the relation's order is the order
//!   its producer states (`Schema::order`).
//! - **A cell by its column's type** ([`cell_face`]): a cell nobody read is drawn `?` and heard
//!   "not read", never 0 and never blank; a cell known empty is drawn blank and heard "empty";
//!   a word of an enum column is the schema's word; a time is its date and minute on this Mac's
//!   clock; any other value is drawn as [`Cell::shown`] draws it (a sealed value masked, a
//!   credential refused).
//! - **Read only.** Nothing here writes or opens a field. A press selects a cell; a press on the
//!   selected cell, or an edit of it ([`SheetsView::edit`]), is refused in words under the sheet
//!   until another cell is selected.
//! - **Bounded.** A sheet lays out only the rows and columns its room shows, from where it is
//!   scrolled to ([`SheetsView::scroll_to`]), so a relation of any length builds a tree its
//!   viewport bounds.
//! - **The hourly chart** ([`chart`], [`chart_node`]): one used and capacity pair of an hourly
//!   relation, a bar an hour, its height the share `used ÷ (capacity × per)` the relation's
//!   figures give (the share is derived where it is drawn; the face sums and averages nothing).
//!   An hour with no row, or whose use or capacity was not read or is no capacity at all, is a
//!   gap, drawn as dashes along the chart's rails and never as an empty bar; a use of zero is a
//!   solid bar one point tall, never a gap. Bars and rails are in the readable secondary ink:
//!   grey is normal, and a use above its capacity is a full bar, no hue.
//!
//! CX-72's Table node has not landed: the grid is lowered onto today's nodes in one function
//! ([`SheetsView::grid`]) for that node to replace, and [`cell_face`] and [`Head`] are the one
//! place a cell's and a column's drawing is decided, for it to take as its own.

#![cfg_attr(
    not(all(test, unix)),
    expect(
        dead_code,
        reason = "TK-35's scaffold: no page draws a relation on it until FP-05 or TK-20; its \
                  test and the chart's paint arm reach it"
    )
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use aterm_db::relation::{Cell, Col, Row, Schema};
use aterm_db::typed::Ty;
use aterm_json::Value;

use crate::native_tables::{
    fitted, group, head_line, press, row_height, rows_words, scale, stamp_words, styled,
};
use crate::native_ui::{
    ActionId, AuditedCustomNode, ButtonSpec, Control, GroupSpec, Insets, Layout, Length,
    LogicalRect, SemanticRole, SemanticValue, StyleRef, TextSpec, UiContent, UiNode, key_word,
};
use crate::native_workers::{one_line, text_width};
use crate::settings::Roles;
use crate::widget::{DrawPrim, rgba};

/// One relation as a sheet draws it: the words that name it, its schema, and its rows' cells in
/// the relation's order.
pub(crate) struct Relation {
    pub(crate) title: String,
    pub(crate) schema: &'static Schema,
    pub(crate) rows: Vec<Vec<Cell>>,
}

impl Relation {
    /// `rows` of one relation, named `title`: the product's word for it, which its schema's name
    /// is not.
    pub(crate) fn of<R: Row>(title: impl Into<String>, rows: &[R]) -> Self {
        Self {
            title: title.into(),
            schema: R::SCHEMA,
            rows: rows.iter().map(Row::cells).collect(),
        }
    }
}

/// A sheet as it is drawn, decided without a window.
pub(crate) struct SheetFace {
    /// Its relation's name: the word its keys carry.
    pub(crate) name: &'static str,
    pub(crate) title: String,
    /// Its columns' heads, in the schema's order.
    pub(crate) heads: Vec<Head>,
    /// Its rows, in the relation's order.
    pub(crate) rows: Vec<RowFace>,
}

/// A column's head.
pub(crate) struct Head {
    pub(crate) name: &'static str,
    /// Its name as a header says it.
    pub(crate) label: String,
    /// Its type's words, `key · ` before them for a column of the relation's key.
    pub(crate) words: String,
    /// What the column holds, in its schema's words.
    pub(crate) doc: &'static str,
    /// Whether its values are numbers, which stand at the right of their cells.
    pub(crate) number: bool,
    /// How wide its cells are: from its words and a sample of its type, never its values.
    pub(crate) width: f32,
}

/// A row of a sheet.
pub(crate) struct RowFace {
    /// Its key cells' words, joined ` · `: what names it when it is heard.
    pub(crate) key: String,
    /// Its key cells' words as one word of a key, unique in its sheet: what its nodes' keys
    /// carry, so a press finds its row by identity.
    pub(crate) word: String,
    /// Its cells, one per column.
    pub(crate) cells: Vec<CellFace>,
}

/// How a cell is drawn and heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CellFace {
    pub(crate) form: Form,
    /// What its face paints.
    pub(crate) drawn: String,
    /// What it is heard as, after its column and row.
    pub(crate) spoken: String,
}

/// What a cell holds, which decides how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Form {
    /// A value of its column's type.
    Value,
    /// One of an enum column's words.
    Word,
    /// Nobody read it: drawn `?`, never 0 and never blank.
    Unread,
    /// It is known to hold nothing: drawn blank.
    Empty,
}

/// How a cell nobody read is heard: apart from the word `unread` an enum column may hold.
pub(crate) const UNREAD: &str = "not read";

/// How a cell known to hold nothing is heard, as the Tables page hears one.
const EMPTY: &str = "empty";

/// What the cells of a column keep beside their words: 10 points each side.
const CELL_PAD: f32 = 20.0;
/// How narrow a column may be at a text scale of 1, as on the Tables page.
const NARROWEST: f32 = 56.0;
/// How wide a column may be at a text scale of 1.
const WIDEST: f32 = 320.0;
/// The most characters a sample of a text type holds.
const SAMPLE_MOST: usize = 24;

/// The sheet of `relation`.
pub(crate) fn face(relation: &Relation) -> SheetFace {
    let schema = relation.schema;
    let keys: Vec<usize> = schema
        .key
        .iter()
        .filter_map(|name| schema.position(name))
        .collect();
    let heads = schema
        .columns
        .iter()
        .map(|col| head(col, schema.key.contains(&col.column.name)))
        .collect();
    let mut words = BTreeSet::new();
    // A cell a row's producer did not state, past the end of its cells, nobody read.
    let unread = Cell::Unread;
    let rows = relation
        .rows
        .iter()
        .map(|cells| {
            let cells: Vec<CellFace> = schema
                .columns
                .iter()
                .enumerate()
                .map(|(at, col)| cell_face(cells.get(at).unwrap_or(&unread), col))
                .collect();
            // A key's position is a column's, and a row has a cell for every column.
            let drawn = |at: &usize| cells[*at].drawn.as_str();
            let key = keys.iter().map(drawn).collect::<Vec<_>>().join(" \u{b7} ");
            let base = keys
                .iter()
                .map(|at| key_word(drawn(at)))
                .collect::<Vec<_>>()
                .join(".");
            // Two rows a relation names alike still get keys of their own.
            let mut word = base.clone();
            let mut n = 1;
            while !words.insert(word.clone()) {
                n += 1;
                word = format!("{base}~{n}");
            }
            RowFace { key, word, cells }
        })
        .collect();
    SheetFace {
        name: schema.name,
        title: relation.title.clone(),
        heads,
        rows,
    }
}

/// How `cell`, in the column `col`, is drawn and heard.
pub(crate) fn cell_face(cell: &Cell, col: &Col) -> CellFace {
    let ty = &col.column.ty;
    let value = match cell {
        Cell::Unread => {
            return CellFace {
                form: Form::Unread,
                drawn: cell.shown(ty),
                spoken: UNREAD.to_owned(),
            };
        }
        Cell::Empty | Cell::Value(Value::Null) => {
            return CellFace {
                form: Form::Empty,
                drawn: String::new(),
                spoken: EMPTY.to_owned(),
            };
        }
        Cell::Value(value) => value,
    };
    let word = match ty {
        Ty::Enum(words) => value.as_str().filter(|word| words.contains(word)),
        _ => None,
    };
    let (form, drawn) = match (word, ty, value.as_u64()) {
        (Some(word), _, _) => (Form::Word, word.to_owned()),
        (None, Ty::Time, Some(at_ms)) => (Form::Value, stamp_words(at_ms)),
        _ => (Form::Value, cell.shown(ty)),
    };
    let spoken = if drawn.is_empty() {
        EMPTY.to_owned()
    } else {
        drawn.clone()
    };
    CellFace {
        form,
        drawn,
        spoken,
    }
}

/// The head of `col`, a column of the relation's key where `key`.
fn head(col: &Col, key: bool) -> Head {
    let name = col.column.name;
    let label = crate::tables_store::label_of(name);
    let words = head_words(col, key);
    let note = |text: &str| text_width(text, SemanticRole::Text, StyleRef::Note);
    let width = (note(&label)
        .max(note(&words))
        .max(sample_width(&col.column.ty))
        + CELL_PAD)
        .clamp(NARROWEST * scale(), WIDEST * scale());
    Head {
        name,
        label,
        words,
        doc: col.doc,
        number: matches!(col.column.ty, Ty::Uint | Ty::Int { .. }),
        width,
    }
}

/// What a head says of `col`'s type: the Tables page's words for it, `key · ` before them for a
/// column of the relation's key whatever its type (`key · key` too, as the Tables page says
/// one), so the key's head is told apart from another column of keys.
fn head_words(col: &Col, key: bool) -> String {
    let words =
        crate::native_tables_choices::ty_words(&col.column.ty, &|target: &str| target.to_owned());
    if key {
        format!("key \u{b7} {words}")
    } else {
        words
    }
}

/// How wide a value of `ty` is drawn at its widest likely spelling.
fn sample_width(ty: &Ty) -> f32 {
    let plain = |text: &str| text_width(text, SemanticRole::Text, StyleRef::Plain);
    match ty {
        Ty::Bool => plain("false"),
        Ty::Int { .. } | Ty::Uint => plain("0000000"),
        Ty::Time => plain(&stamp_words(0)),
        Ty::Day => plain("0000-00-00"),
        Ty::Enum(words) => words.iter().map(|word| plain(word)).fold(0.0, f32::max),
        Ty::Label => plain("m0000"),
        Ty::Text(n)
        | Ty::Handle(n)
        | Ty::Prose(n)
        | Ty::Command(n)
        | Ty::Hex(n)
        | Ty::Digits(n) => plain(&"0".repeat((*n).min(SAMPLE_MOST))),
        _ => plain(&"0".repeat(SAMPLE_MOST)),
    }
}

/// What a press on a sheet set's node did ([`SheetsView::press`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pressed {
    /// The key names no node of this set.
    NotMine,
    /// A sheet of the strip is shown.
    Shown,
    /// A cell is selected.
    Selected,
    /// The selected cell's edit is refused.
    Refused,
}

/// A sheet set's view: which sheet shows, where it is scrolled to, the selected cell and the
/// refusal that stands. The sheets themselves are passed in ([`SheetFace`]), so a new read is
/// new faces and nothing else.
pub(crate) struct SheetsView {
    /// The first word of every key the set's nodes carry.
    prefix: String,
    /// The sheet of the set that shows.
    shown: usize,
    first_row: usize,
    first_column: usize,
    /// The selected cell: its row's word and its column.
    selected: Option<(String, &'static str)>,
    /// The words of the edit refused, until another cell is selected.
    refused: Option<String>,
}

impl SheetsView {
    /// A view of a set's first sheet, scrolled to its start, its nodes keyed under `prefix`.
    pub(crate) fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            shown: 0,
            first_row: 0,
            first_column: 0,
            selected: None,
            refused: None,
        }
    }

    /// Scroll the sheet that shows to its row `row` and column `column`.
    pub(crate) fn scroll_to(&mut self, row: usize, column: usize) {
        self.first_row = row;
        self.first_column = column;
    }

    /// A press on the node `key`: another sheet of the strip shows it, scrolled to its start
    /// (the sheet that shows keeps where it is and what is selected); a cell is selected, and
    /// the selected one refuses its edit.
    pub(crate) fn press(&mut self, set: &[SheetFace], key: &str) -> Pressed {
        let Some(rest) = key
            .strip_prefix(self.prefix.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            return Pressed::NotMine;
        };
        if let Some(name) = rest.strip_prefix("strip/") {
            let Some(at) = set.iter().position(|sheet| sheet.name == name) else {
                return Pressed::NotMine;
            };
            if at != self.shown {
                *self = Self {
                    shown: at,
                    ..Self::new(std::mem::take(&mut self.prefix))
                };
            }
            return Pressed::Shown;
        }
        let Some(face) = set.get(self.shown) else {
            return Pressed::NotMine;
        };
        // `cell/{word}/{column}`: the column is the last word, since a row's word may hold a
        // `/`, and the row is found by its word, never by reading the key's words.
        let Some((word, column)) = rest
            .strip_prefix("sheet/")
            .and_then(|rest| rest.strip_prefix(face.name))
            .and_then(|rest| rest.strip_prefix("/cell/"))
            .and_then(|cell| cell.rsplit_once('/'))
        else {
            return Pressed::NotMine;
        };
        let (Some(row), Some(head)) = (
            face.rows.iter().find(|row| row.word == word),
            face.heads.iter().find(|head| head.name == column),
        ) else {
            return Pressed::NotMine;
        };
        if self
            .selected
            .as_ref()
            .is_some_and(|(word, column)| *word == row.word && *column == head.name)
        {
            self.edit(set);
            return Pressed::Refused;
        }
        self.selected = Some((row.word.clone(), head.name));
        self.refused = None;
        Pressed::Selected
    }

    /// An edit of the selected cell (Return, or typing on it): refused, since nothing here
    /// writes. The refusal's words, which the sheet shows until another cell is selected;
    /// `None` with no cell selected.
    pub(crate) fn edit(&mut self, set: &[SheetFace]) -> Option<&str> {
        let face = set.get(self.shown)?;
        let (_, column) = self.selected.as_ref()?;
        let head = face.heads.iter().find(|head| head.name == *column)?;
        self.refused = Some(format!(
            "{} cannot be edited here: {} is computed",
            head.label, face.title
        ));
        self.refused.as_deref()
    }

    /// The set `width` by `height`: its strip of sheets, the sheet that shows, and the refusal
    /// that stands under it.
    pub(crate) fn node(&self, set: &[SheetFace], width: f32, height: f32) -> UiNode {
        let line = row_height();
        let prefix = self.prefix.as_str();
        let layout = Layout::column()
            .width(Length::Fixed(width))
            .height(Length::Fixed(height));
        let Some(face) = set.get(self.shown) else {
            return group(prefix.to_owned()).layout(layout);
        };
        let strip = set
            .iter()
            .enumerate()
            .map(|(at, sheet)| self.tab(sheet, at == self.shown, line))
            .collect();
        let mut children = vec![
            UiNode::new(
                format!("{prefix}/strip"),
                UiContent::Group(GroupSpec::new("Sheets")),
            )
            .layout(
                Layout::row()
                    .gap(2.0)
                    .width(Length::Fixed(width))
                    .height(Length::Fixed(line)),
            )
            .children(strip)
            .segmented(),
            self.grid(face, width, (height - 2.0 * line).max(0.0)),
        ];
        if let Some(refused) = &self.refused {
            let inner = (width - CELL_PAD).max(0.0);
            children.push(
                group(format!("{prefix}/sheet/{}/refused/seat", face.name))
                    .layout(
                        Layout::row()
                            .height(Length::Fixed(line))
                            .padding(Insets::symmetric(CELL_PAD / 2.0, 0.0)),
                    )
                    .children(vec![
                        UiNode::new(
                            format!("{prefix}/sheet/{}/refused", face.name),
                            UiContent::Text(TextSpec {
                                text: one_line(refused, SemanticRole::Text, StyleRef::Note, inner),
                                role: SemanticRole::Status,
                                style: StyleRef::Note,
                            }),
                        )
                        .layout(Layout::default().width(Length::Fixed(inner))),
                    ]),
            );
        }
        UiNode::new(
            prefix.to_owned(),
            UiContent::Group(GroupSpec::new(format!(
                "{}, {}, read only",
                face.title,
                rows_words(face.rows.len(), face.rows.len())
            ))),
        )
        .layout(layout)
        .children(children)
    }

    /// The strip's line for `sheet`: its title, `on` where it shows.
    fn tab(&self, sheet: &SheetFace, on: bool, line: f32) -> UiNode {
        let key = format!("{}/strip/{}", self.prefix, sheet.name);
        let style = if on {
            StyleRef::Primary
        } else {
            StyleRef::Plain
        };
        let title_w = text_width(&sheet.title, SemanticRole::Text, style);
        let mut seat = Vec::with_capacity(3);
        if on {
            seat.push(styled(format!("{key}/band"), StyleRef::Code));
        }
        // A press under painted words, as the Tables page's rail line is: the sheet that shows
        // is the press's value, which assistive technology says.
        seat.push(UiNode::new(
            key.clone(),
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(format!(
                        "{}, {}",
                        sheet.title,
                        rows_words(sheet.rows.len(), sheet.rows.len())
                    ))
                    .visual_label(String::new()),
                    ActionId::new(key.clone()),
                )
                .value(SemanticValue::Bool(on))
                .style(StyleRef::Navigation),
            ),
        ));
        seat.push(
            group(format!("{key}/face"))
                .layout(Layout::row().padding(Insets::symmetric(CELL_PAD / 2.0, 0.0)))
                .children(vec![fitted(
                    format!("{key}/title"),
                    &sheet.title,
                    style,
                    title_w,
                )])
                .paint_only_tree(),
        );
        group(format!("{key}/seat"))
            .layout(
                Layout::default()
                    .width(Length::Fixed(title_w + CELL_PAD))
                    .height(Length::Fixed(line)),
            )
            .children(seat)
    }

    /// The grid of `face`, `width` by `height`: its heads, then the rows its room shows from
    /// the row and column it is scrolled to, each cell a seat that holds its press and its
    /// face. The one lowering CX-72's Table node replaces.
    fn grid(&self, face: &SheetFace, width: f32, height: f32) -> UiNode {
        let line = row_height();
        let words_line = head_line();
        let base = format!("{}/sheet/{}", self.prefix, face.name);
        // The columns that fit across from the first scrolled to, the first always.
        let mut columns: Vec<(usize, f32)> = Vec::new();
        let mut across = 0.0;
        let first = self.first_column.min(face.heads.len().saturating_sub(1));
        for (at, head) in face.heads.iter().enumerate().skip(first) {
            if !columns.is_empty() && across + head.width > width {
                break;
            }
            let w = head.width.min(width - across).max(0.0);
            columns.push((at, w));
            across += w;
        }
        let heads = columns
            .iter()
            .map(|&(at, w)| {
                let head = &face.heads[at];
                let key = format!("{base}/head/{}", head.name);
                UiNode::new(
                    key.clone(),
                    UiContent::Group(
                        GroupSpec::new(format!("{}, {}: {}", head.label, head.words, head.doc))
                            .style(StyleRef::CellHead),
                    ),
                )
                .layout(Layout::default().width(Length::Fixed(w)))
                .children(vec![
                    group(format!("{key}/face"))
                        .layout(Layout::column().padding(Insets::symmetric(CELL_PAD / 2.0, 0.0)))
                        .children(vec![
                            fitted(
                                format!("{key}/label"),
                                &head.label,
                                StyleRef::Note,
                                (w - CELL_PAD).max(0.0),
                            )
                            .layout(
                                Layout::default()
                                    .width(Length::Fixed((w - CELL_PAD).max(0.0)))
                                    .height(Length::Fixed(line)),
                            ),
                            fitted(
                                format!("{key}/words"),
                                &head.words,
                                StyleRef::Note,
                                (w - CELL_PAD).max(0.0),
                            )
                            .layout(
                                Layout::default()
                                    .width(Length::Fixed((w - CELL_PAD).max(0.0)))
                                    .height(Length::Fixed(words_line)),
                            ),
                        ])
                        .paint_only_tree(),
                ])
            })
            .collect();
        let room = (height - line - words_line).max(0.0);
        let fits = (room / line).floor() as usize;
        let first_row = self.first_row.min(face.rows.len().saturating_sub(1));
        let mut lines = vec![
            group(format!("{base}/head"))
                .layout(Layout::row().height(Length::Fixed(line + words_line)))
                .children(heads),
        ];
        for row in face.rows.iter().skip(first_row).take(fits) {
            let seats = columns
                .iter()
                .map(|&(at, w)| self.seat(&base, row, &face.heads[at], at, w))
                .collect();
            lines.push(
                group(format!("{base}/row/{}", row.word))
                    .layout(Layout::row().height(Length::Fixed(line)))
                    .children(seats),
            );
        }
        group(base)
            .layout(
                Layout::column()
                    .width(Length::Fixed(width))
                    .height(Length::Fixed(height)),
            )
            .children(lines)
    }

    /// The seat of `row`'s cell under `head`, the column `at`, `w` across: its press, heard as
    /// its column, its row and its cell (a cell the row lacks, unread); its face, paint only;
    /// and the ring of the selected cell.
    fn seat(&self, base: &str, row: &RowFace, head: &Head, at: usize, w: f32) -> UiNode {
        let key = format!("{base}/cell/{}/{}", row.word, head.name);
        let (drawn, spoken) = row.cells.get(at).map_or(("?", UNREAD), |cell| {
            (cell.drawn.as_str(), cell.spoken.as_str())
        });
        let selected = self
            .selected
            .as_ref()
            .is_some_and(|(word, column)| *word == row.word && *column == head.name);
        let inner = (w - CELL_PAD).max(0.0);
        let mut text = Vec::with_capacity(2);
        if head.number {
            text.push(group(format!("{key}/lead")).layout(Layout::default().width(Length::Fill)));
            let tw = text_width(drawn, SemanticRole::Text, StyleRef::Plain).min(inner);
            text.push(fitted(format!("{key}/text"), drawn, StyleRef::Plain, tw));
        } else {
            text.push(fitted(format!("{key}/text"), drawn, StyleRef::Plain, inner));
        }
        let mut seat = vec![
            press(
                &key,
                format!(
                    "{} of {}: {spoken}{}",
                    head.label,
                    row.key,
                    if selected { ", selected" } else { "" }
                ),
            ),
            group(format!("{key}/face"))
                .layout(Layout::row().padding(Insets::symmetric(CELL_PAD / 2.0, 0.0)))
                .children(text)
                .paint_only_tree(),
        ];
        if selected {
            // The selection's ring, over the cell: neutral ink, whatever the cell holds.
            seat.push(styled(format!("{key}/focus"), StyleRef::CellSelected).paint_only());
        }
        styled(format!("{key}/seat"), StyleRef::Cell)
            .layout(
                Layout::default()
                    .width(Length::Fixed(w))
                    .height(Length::Fixed(row_height())),
            )
            .children(seat)
    }
}

/// An hour, in milliseconds.
pub(crate) const HOUR_MS: u64 = 3_600_000;

/// The audit id of the hourly chart's node ([`chart_node`], painted by [`paint_chart`]).
pub(crate) const CHART_AUDIT: &str = "sheets.chart";

/// How long a gap's dashes are, and the room between them: a stale line's dash.
const GAP_DASH: f32 = crate::stats_live::STALE_DASH as f32;

/// Which columns of an hourly relation a chart draws, and over which hours.
pub(crate) struct ChartSpec {
    /// The hour a row is of: a time column.
    pub(crate) hour: &'static str,
    /// What was used in the hour: a whole-number column.
    pub(crate) used: &'static str,
    /// What there was to use: a whole-number column, in units of `per` of `used`'s.
    pub(crate) capacity: &'static str,
    /// How many of `used`'s units one of `capacity`'s holds (100 for hundredths of a core used
    /// of whole cores), at least 1.
    pub(crate) per: u64,
    /// The first hour drawn, on the hour.
    pub(crate) from_ms: u64,
    /// How many hours are drawn.
    pub(crate) hours: usize,
}

/// A chart as it is drawn: a slot an hour.
pub(crate) struct ChartFace {
    pub(crate) title: String,
    pub(crate) from_ms: u64,
    pub(crate) slots: Vec<Slot>,
}

/// One hour of a chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// `used` of `whole` (capacity × per), both read.
    Share { used: u64, whole: u64 },
    /// No row, a use or capacity not read or empty, or no capacity: dashes, never a bar.
    Gap,
}

/// The chart of `relation`'s hours by `spec`.
///
/// # Errors
/// A column `spec` names is not the relation's or not of its type, `per` is 0, the first hour
/// is not on the hour, or two rows are of one hour: a face sums and chooses nothing.
pub(crate) fn chart(relation: &Relation, spec: &ChartSpec) -> Result<ChartFace, String> {
    let schema = relation.schema;
    let hour = column_of(schema, spec.hour, |ty| matches!(ty, Ty::Time), "time")?;
    let used = column_of(
        schema,
        spec.used,
        |ty| matches!(ty, Ty::Uint),
        "whole-number",
    )?;
    let capacity = column_of(
        schema,
        spec.capacity,
        |ty| matches!(ty, Ty::Uint),
        "whole-number",
    )?;
    if spec.per == 0 {
        return Err("a chart's capacity holds at least one unit of its use".to_owned());
    }
    if !spec.from_ms.is_multiple_of(HOUR_MS) {
        return Err(format!(
            "a chart starts on the hour, not at {}",
            stamp_words(spec.from_ms)
        ));
    }
    let figure = |cell: Option<&Cell>| match cell {
        Some(Cell::Value(value)) => value.as_u64(),
        _ => None,
    };
    let mut slots = vec![Slot::Gap; spec.hours];
    let mut filled = vec![false; spec.hours];
    for cells in &relation.rows {
        let Some(at) = figure(cells.get(hour))
            .and_then(|at_ms| at_ms.checked_sub(spec.from_ms))
            .and_then(|since| usize::try_from(since / HOUR_MS).ok())
            .filter(|at| *at < spec.hours)
        else {
            continue;
        };
        if std::mem::replace(&mut filled[at], true) {
            return Err(format!(
                "{}: two rows of the hour from {}",
                schema.name,
                stamp_words(spec.from_ms + at as u64 * HOUR_MS)
            ));
        }
        slots[at] = match (figure(cells.get(used)), figure(cells.get(capacity))) {
            (Some(used), Some(capacity)) if capacity > 0 => Slot::Share {
                used,
                whole: capacity.saturating_mul(spec.per),
            },
            _ => Slot::Gap,
        };
    }
    Ok(ChartFace {
        title: relation.title.clone(),
        from_ms: spec.from_ms,
        slots,
    })
}

/// The position of `schema`'s column `name`, whose type `takes` (a `kind` column).
fn column_of(
    schema: &Schema,
    name: &str,
    takes: fn(&Ty) -> bool,
    kind: &str,
) -> Result<usize, String> {
    let at = schema
        .position(name)
        .ok_or_else(|| format!("{}: no column {name}", schema.name))?;
    if takes(&schema.columns[at].column.ty) {
        Ok(at)
    } else {
        Err(format!("{}: {name} is no {kind} column", schema.name))
    }
}

/// `chart`'s node, `width` by `height`: heard as its title, its first hour and how many of its
/// hours were sampled; painted by [`paint_chart`] from its slots.
pub(crate) fn chart_node(
    key: impl Into<Arc<str>>,
    chart: &ChartFace,
    width: f32,
    height: f32,
) -> UiNode {
    let sampled = chart
        .slots
        .iter()
        .filter(|slot| matches!(slot, Slot::Share { .. }))
        .count();
    let paint = chart
        .slots
        .iter()
        .map(|slot| match slot {
            Slot::Share { used, whole } => format!("{used}/{whole}"),
            Slot::Gap => "-".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(",");
    UiNode::new(
        key,
        UiContent::Custom(AuditedCustomNode {
            audit_id: CHART_AUDIT,
            role: SemanticRole::Text,
            label: format!(
                "{}, by the hour from {}: {sampled} of {} hours sampled",
                chart.title,
                stamp_words(chart.from_ms),
                chart.slots.len()
            ),
            paint: Some(paint),
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
            progress: None,
            motion: None,
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fixed(height)),
    )
}

/// A [`CHART_AUDIT`] node's paint in `rect`: a bar a slot, as tall as its share of the rect
/// (at least a point, so a zero is a bar; at most the rect, so a use over its capacity is a full
/// bar), and each run of gaps a dashed rail along the rect's foot, and one along its head where
/// the rect is at least 4 tall, with no bar. A slot at least 2 wide keeps a point between its
/// bar or rail and the next; a narrower one keeps no gutter: its bar or rail fills it, so
/// nothing reaches past its slot. A value that names no slots paints nothing.
pub(crate) fn paint_chart(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    let Some(slots) = value.and_then(parse_slots) else {
        return;
    };
    if slots.is_empty() || rect.is_empty() {
        return;
    }
    let ink = crate::native_ui::readable_secondary(roles);
    let slot_w = rect.width / slots.len() as f32;
    let gutter = if slot_w >= 2.0 { 1.0 } else { 0.0 };
    let bar_w = slot_w - gutter;
    let mut gap_from = None;
    for (at, slot) in slots.iter().enumerate() {
        match *slot {
            Slot::Share { used, whole } => {
                if let Some(from) = gap_from.take() {
                    gap_rails(prims, rect, (from, at), (slot_w, gutter), ink);
                }
                let share = (used as f32 / whole as f32).min(1.0);
                let h = (rect.height * share)
                    .round()
                    .clamp(1.0, rect.height.max(1.0));
                prims.push(DrawPrim::Panel {
                    x: rect.x + at as f32 * slot_w,
                    y: rect.bottom() - h,
                    w: bar_w,
                    h,
                    radius: 0.0,
                    fill: rgba(ink, 255),
                });
            }
            Slot::Gap => {
                gap_from.get_or_insert(at);
            }
        }
    }
    if let Some(from) = gap_from {
        gap_rails(prims, rect, (from, slots.len()), (slot_w, gutter), ink);
    }
}

/// The slots a chart node's paint value names: `used/whole` or `-` each, joined by `,`.
fn parse_slots(value: &str) -> Option<Vec<Slot>> {
    value
        .split(',')
        .map(|slot| {
            if slot == "-" {
                return Some(Slot::Gap);
            }
            let (used, whole) = slot.split_once('/')?;
            let whole = whole.parse().ok().filter(|whole| *whole > 0)?;
            Some(Slot::Share {
                used: used.parse().ok()?,
                whole,
            })
        })
        .collect()
}

/// The gap of the slots `from..to`, each `slot_w` across and keeping `gutter` before the next:
/// a dashed rail along the chart's foot, where a zero's bar stands, and one along its head
/// where the chart is tall enough to hold both apart.
fn gap_rails(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    (from, to): (usize, usize),
    (slot_w, gutter): (f32, f32),
    ink: [u8; 3],
) {
    let rail = |y: f32| DrawPrim::Dashed {
        x: rect.x + from as f32 * slot_w,
        y,
        w: (to - from) as f32 * slot_w - gutter,
        h: 0.0,
        width: 1.0,
        on: GAP_DASH,
        off: GAP_DASH,
        color: rgba(ink, 0xCC),
    };
    prims.push(rail(rect.bottom() - 1.0));
    if rect.height >= 4.0 {
        prims.push(rail(rect.y));
    }
}

#[cfg(all(test, unix))]
#[path = "native_sheets_tests.rs"]
mod tests;
