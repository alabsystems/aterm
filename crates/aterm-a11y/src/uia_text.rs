// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! UIA text offsets and units, derived from the shared model's character runs.
//! Offsets count UTF-16 units; a model character, including a combining cluster,
//! remains indivisible. This module makes no Windows calls.

use crate::tree::Node;

#[derive(Clone, Copy)]
pub(crate) enum Unit {
    Character,
    Word,
    Line,
    Paragraph,
    Document,
}

pub(crate) struct Document {
    pub(crate) value: String,
    pub(crate) characters: Vec<usize>,
    words: Vec<usize>,
    lines: Vec<usize>,
    paragraphs: Vec<usize>,
}

impl Document {
    pub(crate) fn new(node: Node<'_>) -> Option<Self> {
        if !node.supports_text_ranges() {
            return None;
        }
        let mut doc = Self {
            value: String::new(),
            characters: vec![0],
            words: vec![0],
            lines: vec![0],
            paragraphs: vec![0],
        };
        let mut offset = 0;
        for run in node.text_runs() {
            let value = run.data().value().unwrap_or_default();
            doc.lines.push(offset);
            let mut byte = 0usize;
            for (index, &len) in run.data().character_lengths().iter().enumerate() {
                if u8::try_from(index)
                    .ok()
                    .is_some_and(|index| run.data().word_starts().contains(&index))
                {
                    doc.words.push(offset);
                }
                let next = byte.checked_add(usize::from(len))?;
                let character = value.get(byte..next)?;
                offset += character.encode_utf16().count();
                doc.characters.push(offset);
                byte = next;
            }
            if byte != value.len() {
                return None;
            }
            doc.value.push_str(value);
            if value.ends_with('\n') {
                doc.paragraphs.push(offset);
            }
        }
        for points in [&mut doc.words, &mut doc.lines, &mut doc.paragraphs] {
            points.push(offset);
            points.sort_unstable();
            points.dedup();
        }
        Some(doc)
    }

    pub(crate) fn len(&self) -> usize {
        *self.characters.last().unwrap_or(&0)
    }

    pub(crate) fn floor(&self, offset: usize) -> usize {
        self.characters[self
            .characters
            .partition_point(|&point| point <= offset)
            .saturating_sub(1)]
    }

    pub(crate) fn points(&self, unit: Unit) -> &[usize] {
        match unit {
            Unit::Character => &self.characters,
            Unit::Word => &self.words,
            Unit::Line => &self.lines,
            Unit::Paragraph => &self.paragraphs,
            Unit::Document => &[],
        }
    }

    pub(crate) fn enclosing(&self, offset: usize, unit: Unit) -> (usize, usize) {
        if matches!(unit, Unit::Document) {
            return (0, self.len());
        }
        let points = self.points(unit);
        let at = points
            .partition_point(|&point| point <= offset)
            .saturating_sub(1)
            .min(points.len().saturating_sub(2));
        (
            points[at],
            points.get(at + 1).copied().unwrap_or(points[at]),
        )
    }

    pub(crate) fn step(&self, offset: usize, unit: Unit, count: i32) -> (usize, i32) {
        if count == 0 {
            return (self.floor(offset), 0);
        }
        let document = [0, self.len()];
        let points = if matches!(unit, Unit::Document) {
            &document[..]
        } else {
            self.points(unit)
        };
        let at = if count > 0 {
            points
                .partition_point(|&point| point <= offset)
                .saturating_sub(1)
        } else {
            points.partition_point(|&point| point < offset)
        };
        let target = if count > 0 {
            at.saturating_add(count as usize).min(points.len() - 1)
        } else {
            at.saturating_sub(count.unsigned_abs() as usize)
        };
        (
            points[target],
            i32::try_from(target as i64 - at as i64).unwrap_or(if count < 0 {
                i32::MIN
            } else {
                i32::MAX
            }),
        )
    }

    pub(crate) fn slice(&self, start: usize, end: usize) -> String {
        let mut at = 0;
        self.value
            .chars()
            .filter(|ch| {
                let here = at;
                at += ch.len_utf16();
                here >= start && at <= end
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node as Data, NodeId, Role, Tree, TreeId, TreeUpdate};
    use crate::platform::Publication;

    fn document() -> Publication {
        let mut root = Data::new(Role::Document);
        root.set_children(vec![NodeId(2), NodeId(3), NodeId(4)]);
        let mut first = Data::new(Role::TextRun);
        first.set_value("A😀e\u{301}\n");
        first.set_character_lengths(vec![1, 4, 3, 1]);
        first.set_word_starts(vec![0, 2]);
        let mut blank = Data::new(Role::TextRun);
        blank.set_value("\n");
        blank.set_character_lengths(vec![1]);
        let mut last = Data::new(Role::TextRun);
        last.set_value("tail");
        last.set_character_lengths(vec![1; 4]);
        last.set_word_starts(vec![0]);
        let mut state = Publication::default();
        state.update(TreeUpdate {
            nodes: vec![
                (NodeId(1), root),
                (NodeId(2), first),
                (NodeId(3), blank),
                (NodeId(4), last),
            ],
            tree: Some(Tree::new(NodeId(1))),
            tree_id: TreeId::ROOT,
            focus: NodeId(1),
        });
        state
    }

    #[test]
    fn model_characters_and_blank_lines_define_utf16_navigation() {
        let state = document();
        let doc = Document::new(state.tree().unwrap().root()).unwrap();
        assert_eq!(doc.characters, [0, 1, 3, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(
            doc.enclosing(2, Unit::Character),
            (1, 3),
            "astral scalar is indivisible"
        );
        assert_eq!(
            doc.enclosing(4, Unit::Character),
            (3, 5),
            "combining model character is indivisible"
        );
        assert_eq!(
            doc.enclosing(6, Unit::Line),
            (6, 7),
            "blank line remains navigable"
        );
        assert_eq!(doc.enclosing(7, Unit::Paragraph), (7, 11));
        assert_eq!(doc.step(4, Unit::Character, -1), (3, -1));
        assert_eq!(doc.step(4, Unit::Character, 1), (5, 1));
        assert_eq!(doc.step(3, Unit::Word, -99), (0, -1));
        assert_eq!(doc.step(0, Unit::Document, i32::MAX), (11, 1));
        assert_eq!(doc.step(11, Unit::Character, 1), (11, 0));
        assert_eq!(doc.slice(1, 5), "😀e\u{301}");
    }
}
