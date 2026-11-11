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

    /// The first (or, `backward`, last) match of `needle` within
    /// `start..end`, widened to the model characters it touches. With
    /// `ignore_case`, the needle and the text compare by one per-character
    /// [`fold`].
    pub(crate) fn find(
        &self,
        start: usize,
        end: usize,
        needle: &str,
        backward: bool,
        ignore_case: bool,
    ) -> Option<(usize, usize)> {
        let key = |ch: char| if ignore_case { fold(ch) } else { ch };
        let needle = needle.chars().map(key).collect::<String>();
        // `spans[byte]` is the UTF-16 extent of the source character whose
        // key holds the haystack's `byte`.
        let mut haystack = String::new();
        let mut spans = Vec::new();
        let mut offset = 0;
        for ch in self.value.chars() {
            let here = offset;
            offset += ch.len_utf16();
            if here < start || offset > end {
                continue;
            }
            let ch = key(ch);
            haystack.push(ch);
            spans.extend(std::iter::repeat_n((here, offset), ch.len_utf8()));
        }
        let byte = if backward {
            haystack.rfind(&needle)
        } else {
            haystack.find(&needle)
        }?;
        let first = spans.get(byte)?.0;
        let last = spans.get(byte + needle.len().checked_sub(1)?)?.1;
        Some((
            self.floor(first),
            self.characters
                .iter()
                .copied()
                .find(|&point| point >= last)
                .unwrap_or(self.len()),
        ))
    }
}

/// A character's case-blind key: the lowercase of its uppercase, taken per
/// character so no context (such as Greek's word-final sigma) splits a letter's
/// cases: 'Σ', 'σ' and 'ς' all key as 'σ', and 'ß' and 'ẞ' as 'ß'. Where a
/// mapping is more than one character the character stands for itself, so a
/// key is always one character. The dotless 'ı' keys with 'i', as its
/// uppercase 'I' does.
fn fold(ch: char) -> char {
    let mut upper = ch.to_uppercase();
    let upper = match (upper.next(), upper.next()) {
        (Some(one), None) => one,
        _ => ch,
    };
    let mut lower = upper.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(one), None) => one,
        _ => upper,
    }
}

#[cfg(test)]
#[path = "uia_text_tests.rs"]
mod tests;
