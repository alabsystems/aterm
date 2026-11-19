// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's typed cells (docs/PLAN.md SF-37): what a column's header says of its type,
//! and what an open cell offers by it. An enum's cell opens on a list of its words, a set of
//! enums' on the same list with each member marked, and a bool's switches to its other value
//! (a conflicted one is settled by the pane's Keep, never by a switch). A day reads, and is
//! typed, as its date (`2026-11-14`), the spelling the registry's grid gives it
//! ([`aterm_db::typed::grid::day_date`]).
//!
//! - **Keys.** In an open list Up and Down move the marked word, Return stages it (in a set's
//!   list, the members marked) and moves down as any open cell does, and Escape leaves the cell
//!   as it was. Leaving an enum's cell any other way (Tab, a press elsewhere) stages the marked
//!   word as Return does. A typed word marks the word it is, else the first that starts with it;
//!   with none marked what was typed is staged, which the check then refuses where it is no word
//!   of the column's. In a set's list Space puts the marked word in or takes it out while the
//!   field holds what the list last wrote; once the field is typed in, Space is a space. Space
//!   on a selected cell opens its list on what it holds.
//! - **Pointer.** A press on a word stages it, or in a set's list puts it in or takes it out;
//!   the wheel over the list moves its mark. The list stands under the cell, or over it where
//!   the sheet has more room there, and shows as many words as that room holds.
//!
//! The words come from the compiled column ([`aterm_db::typed::Ty::Enum`]): the page keeps no
//! list of them. A reference's cell opens on the same list, of the rows of its target, each read
//! by its words and staged by its key ([`crate::native_tables::rows::choice`], docs/PLAN.md
//! LT-30). The list is a list box (`ListBox`) of options (`ListBoxOption`), each spoken by its
//! words, the marked one selected.

use aterm_db::typed::{Column, Ty};

use crate::native_tables::{Group, SheetColumn, fitted, group, styled};
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length, SemanticRole,
    StyleRef, UiContent, UiNode,
};
use crate::native_workers::text_width;

/// The key of the word `n` of the open cell's list.
pub(crate) const CHOICE: &str = "tables/choice/";

/// How the header says `column`'s type, under its name: `key · text`, `one of 6`,
/// `any of 12`, `true or false`, `date`, `→ Providers`. `title` names a table by its rail title.
pub(crate) fn type_words(
    column: &SheetColumn,
    title: impl Fn(&str) -> Option<&'static str>,
) -> String {
    // A reference's table by the page's name for its sheet, the column's link, where it has one.
    let named = |target: &str| {
        let target = column.links.as_deref().unwrap_or(target);
        title(target).unwrap_or(target).to_owned()
    };
    let words = match (column.compiled.map(|c| c.ty), &column.links) {
        (Some(ty), _) => ty_words(&ty, &named),
        // A count of the rows of another table that name this row.
        (None, Some(other)) if column.group == Group::Linked => {
            format!("count \u{b7} {}", named(other))
        }
        // The row's own key, or the machine that states a row of its own.
        (None, Some(to)) => format!("\u{2192} {}", named(to)),
        (None, None) => String::new(),
    };
    match (column.key, words.is_empty()) {
        (true, true) => "key".to_owned(),
        (true, false) => format!("key \u{b7} {words}"),
        (false, _) => words,
    }
}

/// A type's words: what a value of it is.
fn ty_words(ty: &Ty, named: &dyn Fn(&str) -> String) -> String {
    match ty {
        Ty::Bool => "true or false".into(),
        Ty::Int { .. } => "number".into(),
        Ty::Uint => "whole number".into(),
        Ty::Text(_) | Ty::Prose(_) | Ty::Handle(_) => "text".into(),
        Ty::Command(_) => "command".into(),
        Ty::Commit => "commit".into(),
        Ty::Digest => "digest".into(),
        Ty::Path => "path".into(),
        Ty::Decimal => "decimal".into(),
        Ty::Principal => "writer".into(),
        Ty::Evidence => "evidence".into(),
        Ty::Enum(words) => format!("one of {}", words.len()),
        Ty::Time => "time".into(),
        Ty::Day => "date".into(),
        Ty::Label => "machine label".into(),
        Ty::Hex(n) => format!("{n} hex digits"),
        Ty::Digits(n) => format!("{n} digits"),
        Ty::Email => "address".into(),
        Ty::Phone => "phone number".into(),
        Ty::Url => "URL".into(),
        Ty::SecretRef | Ty::Secret => "secret".into(),
        Ty::Key => "key".into(),
        Ty::Ref(target) => format!("\u{2192} {}", named(target)),
        Ty::Link(link) => format!(
            "\u{2192} {} in {}/{}",
            link.collection, link.application, link.role
        ),
        Ty::Set(Ty::Enum(words)) => format!("any of {}", words.len()),
        Ty::Set(Ty::Ref(target)) | Ty::List(Ty::Ref(target), _) => {
            format!("\u{2192} {}, several", named(target))
        }
        Ty::Set(inner) => several("set", inner, named),
        Ty::List(inner, _) => several("list", inner, named),
        Ty::Struct(_) | Ty::Json(_) => "record".into(),
        Ty::SrcRef => "source".into(),
        Ty::Sealed(_) => "sealed record".into(),
    }
}

/// A set's or a list's words: `set of machine labels`, `list of whole numbers`; where its values
/// have no one noun, what each is (`list, each one of 4`).
fn several(kind: &str, inner: &Ty, named: &dyn Fn(&str) -> String) -> String {
    match plural(inner) {
        Some(nouns) => format!("{kind} of {nouns}"),
        None => format!("{kind}, each {}", ty_words(inner, named)),
    }
}

/// The noun for several values of `ty`, where it has one.
fn plural(ty: &Ty) -> Option<&'static str> {
    Some(match ty {
        Ty::Int { .. } => "numbers",
        Ty::Uint => "whole numbers",
        Ty::Text(_) | Ty::Prose(_) | Ty::Handle(_) => "texts",
        Ty::Command(_) => "commands",
        Ty::Commit => "commits",
        Ty::Digest => "digests",
        Ty::Path => "paths",
        Ty::Decimal => "decimals",
        Ty::Principal => "writers",
        Ty::Evidence => "evidence",
        Ty::Time => "times",
        Ty::Day => "dates",
        Ty::Label => "machine labels",
        Ty::Email => "addresses",
        Ty::Phone => "phone numbers",
        Ty::Url => "URLs",
        Ty::SecretRef | Ty::Secret => "secrets",
        Ty::Key => "keys",
        Ty::Struct(_) | Ty::Json(_) => "records",
        Ty::SrcRef => "sources",
        Ty::Sealed(_) => "sealed records",
        Ty::Bool
        | Ty::Enum(_)
        | Ty::Hex(_)
        | Ty::Digits(_)
        | Ty::Ref(_)
        | Ty::Link(_)
        | Ty::Set(_)
        | Ty::List(..) => return None,
    })
}

/// What opening a cell of `column` does by its type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opens {
    /// A text field, as any cell.
    Field,
    /// A list of the column's words, or of the rows a reference names ([`Choice`]).
    List,
    /// The other value of a bool, staged at once.
    Toggle,
}

/// What opening a cell of `column` does.
pub(crate) fn opens(column: &SheetColumn) -> Opens {
    match column.compiled.map(|c| c.ty) {
        Some(Ty::Enum(_) | Ty::Set(Ty::Enum(_)) | Ty::Ref(_)) => Opens::List,
        Some(Ty::Bool) => Opens::Toggle,
        _ => Opens::Field,
    }
}

/// An open cell's list of words: an enum's, a set of enums' with each member marked, or the rows
/// a reference may name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    /// What each choice stages: the column's words, in its order, or a reference's row keys;
    /// for a column that may be empty, `""` last, which reads `empty`.
    pub(crate) words: Vec<String>,
    /// How each choice reads, in the list and in the open field once marked: an enum's word
    /// itself; a row by the words it is known by, never its key alone.
    pub(crate) said: Vec<String>,
    /// Several of the words at once: a set.
    pub(crate) many: bool,
    /// Whether only a choice typed whole is marked: a reference's rows, where the first row
    /// whose words start with what was typed (one of two rows under one label) is no row the
    /// person named.
    pub(crate) whole: bool,
    /// The word the keys have marked.
    pub(crate) at: Option<usize>,
    /// What a set's list last wrote into the field (what it opened with, at first): while the
    /// field holds it, Space puts the marked word in or takes it out; once it is typed in,
    /// Space is typed.
    pub(crate) wrote: String,
}

impl Choice {
    /// The list `column` opens on, the word `text` holds marked (a set's first word).
    pub(crate) fn of(column: &Column, text: &str) -> Option<Self> {
        let (words, many): (&'static [&'static str], bool) = match column.ty {
            Ty::Enum(words) => (words, false),
            Ty::Set(Ty::Enum(words)) => (words, true),
            _ => return None,
        };
        let mut words: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
        if !many && column.null {
            words.push(String::new());
        }
        let at = if many {
            Some(0)
        } else {
            words.iter().position(|word| word == text.trim())
        };
        Some(Self {
            said: words.clone(),
            words,
            many,
            whole: false,
            at,
            wrote: text.to_owned(),
        })
    }

    /// Move the mark `by` words, round the list's ends; from no mark, to the first word down or
    /// the last up.
    pub(crate) fn step(&mut self, by: i32) {
        let n = self.words.len();
        if n == 0 {
            return;
        }
        let len = i64::try_from(n).unwrap_or(i64::MAX);
        let next = match self.at {
            None if by < 0 => len - 1,
            None => 0,
            Some(at) => (i64::try_from(at).unwrap_or(0) + i64::from(by)).rem_euclid(len),
        };
        self.at = usize::try_from(next).ok();
    }

    /// Mark the word that was typed, else (except in a list of rows, [`Self::whole`]) the first
    /// that starts with it; else, in an enum's list, none. A choice is matched by how it reads,
    /// and a row's key typed whole marks that row. In a set's list the word typed is the one
    /// after its last comma, and a mark no word matches stays where it was.
    pub(crate) fn follow(&mut self, typed: &str) {
        let typed = if self.many {
            typed.rsplit(',').next().unwrap_or_default()
        } else {
            typed
        };
        let typed = typed.trim().to_lowercase();
        let found = (!typed.is_empty())
            .then(|| {
                let said = || self.said.iter().map(|w| w.to_lowercase());
                self.words
                    .iter()
                    .position(|w| w.to_lowercase() == typed)
                    .or_else(|| said().position(|w| w == typed))
                    .or_else(|| {
                        (!self.whole)
                            .then(|| said().position(|w| w.starts_with(&typed)))
                            .flatten()
                    })
            })
            .flatten();
        if !self.many || found.is_some() {
            self.at = found;
        }
    }

    /// The word an enum's list stages as its cell closes: the marked one (a row's key).
    pub(crate) fn marked(&self) -> Option<&str> {
        self.at
            .filter(|_| !self.many)
            .and_then(|at| self.words.get(at))
            .map(String::as_str)
    }

    /// How the marked choice reads: what the open field shows once it is marked.
    pub(crate) fn marked_said(&self) -> Option<&str> {
        self.at
            .filter(|_| !self.many)
            .and_then(|at| self.said.get(at))
            .map(String::as_str)
    }

    /// Whether Space in the field is the list's, not typed: a set's list whose field holds what
    /// it last wrote.
    pub(crate) fn spaces(&self, field: &str) -> bool {
        self.many && field == self.wrote
    }

    /// The members a set's text holds, in the column's order.
    pub(crate) fn members(&self, text: &str) -> Vec<&str> {
        let held: Vec<&str> = text
            .split(',')
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .collect();
        self.words
            .iter()
            .map(String::as_str)
            .filter(|w| held.contains(w))
            .collect()
    }

    /// A set's text with word `n` put in, or taken out where it is in: its members in the
    /// column's order, as the grid shows a set (`a, b`).
    pub(crate) fn toggled(&self, text: &str, n: usize) -> String {
        let Some(word) = self.words.get(n).map(String::as_str) else {
            return text.to_owned();
        };
        let mut members = self.members(text);
        if members.contains(&word) {
            members.retain(|w| *w != word);
        } else {
            members.push(word);
        }
        self.words
            .iter()
            .map(String::as_str)
            .filter(|w| members.contains(w))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A bool's other value: `true` for anything but `true`.
pub(crate) fn toggled(text: &str) -> String {
    if text.trim() == "true" {
        "false".into()
    } else {
        "true".into()
    }
}

/// The most words an open list shows at once; past them it shows the ones round the mark.
const SHOWN: usize = 12;

/// The open cell's list, laid over the sheet: under the cell whose left, top and width are
/// `cell`, or over it where the list would pass `foot`, the sheet's bottom, and fits there; one
/// `line` a word, the marked word ringed and, in a set's list, each member ticked. Where it fits
/// on neither side it takes the side with more room and shows as many words as that holds,
/// those round the mark, so every word it shows is on the sheet. `typed` is what the open cell
/// holds: a set's members. The list is a list box named `label`, each choice an option spoken by
/// how it reads, the marked one selected.
pub(crate) fn list(
    choice: &Choice,
    label: &str,
    typed: &str,
    (x, top, w): (f32, f32, f32),
    (foot, line): (f32, f32),
) -> UiNode {
    let members = if choice.many {
        choice.members(typed)
    } else {
        Vec::new()
    };
    let most = choice.words.len().min(SHOWN);
    let below = top + line;
    // How many words a room holds.
    let holds = |room: f32| {
        (0..=most)
            .take_while(|k| *k as f32 * line <= room + 0.5)
            .last()
            .unwrap_or(0)
    };
    let (under, over) = (holds(foot - below), holds(top));
    let (n, above) = if under >= most {
        (most, false)
    } else if over >= most {
        (most, true)
    } else if over > under {
        (over.max(1).min(most), true)
    } else {
        (under.max(1).min(most), false)
    };
    let first = choice
        .at
        .map_or(0, |at| at.saturating_sub(n.saturating_sub(1)))
        .min(choice.words.len().saturating_sub(n));
    let mark_w = text_width("\u{2713} ", SemanticRole::Text, StyleRef::Primary);
    let widest = choice
        .said
        .iter()
        .map(|word| text_width(said_word(word), SemanticRole::Text, StyleRef::Primary))
        .fold(0.0_f32, f32::max);
    let width = w.max(widest + mark_w + 28.0);
    let height = n as f32 * line;
    let y = if above { top - height } else { below };
    let mut items = Vec::with_capacity(n);
    for (at, (word, says)) in choice
        .words
        .iter()
        .zip(&choice.said)
        .enumerate()
        .skip(first)
        .take(n)
    {
        let key = format!("{CHOICE}{at}");
        let marked = choice.at == Some(at);
        let member = members.contains(&word.as_str());
        let mut spoken = said_word(says).to_owned();
        if choice.many {
            spoken.push_str(if member {
                ", in the set"
            } else {
                ", not in the set"
            });
        }
        if marked {
            spoken.push_str(", marked");
        }
        let style = if word.is_empty() {
            StyleRef::Note
        } else {
            StyleRef::Primary
        };
        let mut face = Vec::new();
        if choice.many {
            face.push(fitted(
                format!("{key}/tick"),
                if member { "\u{2713}" } else { "" },
                StyleRef::Primary,
                mark_w,
            ));
        }
        face.push(fitted(
            format!("{key}/word"),
            said_word(says),
            style,
            (width - 20.0 - if choice.many { mark_w } else { 0.0 }).max(1.0),
        ));
        let mut seat = vec![
            option(&key, spoken, marked),
            group(format!("{key}/face"))
                .layout(Layout::row().padding(Insets::symmetric(10.0, 0.0)))
                .children(face)
                .paint_only_tree(),
        ];
        if marked {
            seat.push(styled(format!("{key}/mark"), StyleRef::CellSelected).paint_only());
        }
        items.push(
            styled(format!("{key}/seat"), StyleRef::CellHead)
                .layout(
                    Layout::default()
                        .width(Length::Fixed(width))
                        .height(Length::Fixed(line)),
                )
                .children(seat),
        );
    }
    group("tables/choices")
        .layout(Layout::column())
        .children(vec![
            group("tables/choices/above")
                .layout(Layout::default().height(Length::Fixed(y.max(0.0)))),
            group("tables/choices/line")
                .layout(Layout::row().height(Length::Fixed(height)))
                .children(vec![
                    group("tables/choices/before")
                        .layout(Layout::default().width(Length::Fixed(x.max(0.0)))),
                    UiNode::new(
                        "tables/choices/list",
                        UiContent::Group(GroupSpec {
                            role: SemanticRole::ListBox,
                            ..GroupSpec::new(label)
                        }),
                    )
                    .layout(Layout::column().width(Length::Fixed(width)))
                    .children(items),
                ]),
        ])
}

/// One choice of the list: a press the size of its seat, under its painted words, spoken as
/// `spoken`; an option of the list box, selected where it is marked.
fn option(key: &str, spoken: String, marked: bool) -> UiNode {
    UiNode::new(
        key.to_owned(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(spoken).visual_label(String::new()),
                ActionId::new(key.to_owned()),
            )
            .state(ControlState {
                enabled: true,
                selected: marked,
                ..ControlState::default()
            })
            .style(StyleRef::Navigation)
            .role(SemanticRole::ListBoxOption),
        ),
    )
}

/// How a word of a list reads: an empty one as `empty`.
fn said_word(word: &str) -> &str {
    if word.is_empty() { "empty" } else { word }
}

#[cfg(test)]
#[path = "native_tables_choices_tests.rs"]
mod tests;
