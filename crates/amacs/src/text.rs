// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Persistent balanced UTF-8 text storage for native documents.

use std::ops::Range;
use std::sync::Arc;

const CHUNK_TARGET: usize = 4 * 1024;
const CHUNK_MERGE_MAX: usize = CHUNK_TARGET * 2;

#[derive(Clone, Debug)]
pub struct TextRope {
    root: Arc<Node>,
}

#[derive(Debug)]
enum Node {
    Empty,
    Leaf {
        text: Arc<str>,
        newlines: usize,
    },
    Branch {
        left: Arc<Node>,
        right: Arc<Node>,
        bytes: usize,
        newlines: usize,
        height: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRopeError {
    InvalidRange,
}

impl Default for TextRope {
    fn default() -> Self {
        Self {
            root: Arc::new(Node::Empty),
        }
    }
}

impl From<&str> for TextRope {
    fn from(text: &str) -> Self {
        if text.is_empty() {
            return Self::default();
        }
        let mut leaves = Vec::new();
        let mut start = 0usize;
        while start < text.len() {
            let mut end = start.saturating_add(CHUNK_TARGET).min(text.len());
            while end > start && !text.is_char_boundary(end) {
                end -= 1;
            }
            if end == start {
                end = text[start..]
                    .char_indices()
                    .nth(1)
                    .map_or(text.len(), |(relative, _)| start.saturating_add(relative));
            }
            leaves.push(leaf(&text[start..end]));
            start = end;
        }
        while leaves.len() > 1 {
            let mut next = Vec::with_capacity(leaves.len().div_ceil(2));
            let mut iter = leaves.into_iter();
            while let Some(left) = iter.next() {
                next.push(match iter.next() {
                    Some(right) => branch(left, right),
                    None => left,
                });
            }
            leaves = next;
        }
        Self {
            root: leaves.pop().unwrap_or_else(empty),
        }
    }
}

impl TextRope {
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        bytes(&self.root)
    }

    /// Whether the rope holds no bytes.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg(any(test, debug_assertions))]
    pub fn line_count(&self) -> usize {
        newlines(&self.root).saturating_add(1)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn chunk_count(&self) -> usize {
        chunk_count(&self.root)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn height(&self) -> u8 {
        height(&self.root)
    }

    pub fn is_char_boundary(&self, offset: usize) -> bool {
        if offset > self.len() {
            return false;
        }
        is_boundary(&self.root, offset)
    }

    pub fn to_flat_string(&self) -> String {
        let mut output = String::with_capacity(self.len());
        append_to(&self.root, &mut output);
        output
    }

    pub fn replace(&self, range: Range<usize>, insert: &str) -> Result<Self, TextRopeError> {
        self.validate(&range)?;
        let (before, tail) = split(self.root.clone(), range.start);
        let (_, after) = split(tail, range.end.saturating_sub(range.start));
        let inserted = TextRope::from(insert).root;
        Ok(Self {
            root: concat(concat(before, inserted), after),
        })
    }

    /// Apply non-overlapping ranges expressed in this snapshot's coordinates.
    pub fn replace_many(&self, edits: &[(Range<usize>, &str)]) -> Result<Self, TextRopeError> {
        let mut previous_end = 0usize;
        for (index, (range, _)) in edits.iter().enumerate() {
            self.validate(range)?;
            if index > 0 && range.start < previous_end {
                return Err(TextRopeError::InvalidRange);
            }
            previous_end = range.end;
        }
        let mut result = self.clone();
        for (range, insert) in edits.iter().rev() {
            result = result.replace(range.clone(), insert)?;
        }
        Ok(result)
    }

    fn validate(&self, range: &Range<usize>) -> Result<(), TextRopeError> {
        if range.start > range.end
            || range.end > self.len()
            || !self.is_char_boundary(range.start)
            || !self.is_char_boundary(range.end)
        {
            Err(TextRopeError::InvalidRange)
        } else {
            Ok(())
        }
    }
}

fn empty() -> Arc<Node> {
    Arc::new(Node::Empty)
}

fn leaf(text: &str) -> Arc<Node> {
    if text.is_empty() {
        return empty();
    }
    Arc::new(Node::Leaf {
        text: Arc::from(text),
        newlines: text.bytes().filter(|byte| *byte == b'\n').count(),
    })
}

fn branch(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    if bytes(&left) == 0 {
        return right;
    }
    if bytes(&right) == 0 {
        return left;
    }
    Arc::new(Node::Branch {
        bytes: bytes(&left).saturating_add(bytes(&right)),
        newlines: newlines(&left).saturating_add(newlines(&right)),
        height: taller_child(height(&left), height(&right)).saturating_add(1),
        left,
        right,
    })
}

trust_contract_fn! {
    fn taller_child(left: u8, right: u8) -> u8
    clauses [ensures result >= left && result >= right by taller_child_covers_both
             ensures result == left || result == right by taller_child_is_a_child]
    {
        if left < right { right } else { left }
    }
}

verify_island! {
    clean {
        theorem taller_child_covers_both (left right : UInt8) :
            And (Nat.le (UInt8.toNat left) (UInt8.toNat (trust_import_amacs__text__taller_child left right)))
                (Nat.le (UInt8.toNat right) (UInt8.toNat (trust_import_amacs__text__taller_child left right))) :=
          @Decidable.casesOn (LT.lt left right)
            (fun (d : Decidable (LT.lt left right)) =>
              And (Nat.le (UInt8.toNat left) (UInt8.toNat (@ite UInt8 (LT.lt left right) d right left)))
                  (Nat.le (UInt8.toNat right) (UInt8.toNat (@ite UInt8 (LT.lt left right) d right left))))
            (instDecidableUInt8Lt left right)
            (fun (h : Not (LT.lt left right)) =>
              And.intro (Nat.le_refl (UInt8.toNat left)) (Nat.ge_of_not_lt h))
            (fun (h : LT.lt left right) =>
              And.intro (Nat.le_of_lt h) (Nat.le_refl (UInt8.toNat right)))

        theorem taller_child_is_a_child (left right : UInt8) :
            Or (Eq (trust_import_amacs__text__taller_child left right) left)
               (Eq (trust_import_amacs__text__taller_child left right) right) :=
          @Decidable.casesOn (LT.lt left right)
            (fun (d : Decidable (LT.lt left right)) =>
              Or (Eq (@ite UInt8 (LT.lt left right) d right left) left)
                 (Eq (@ite UInt8 (LT.lt left right) d right left) right))
            (instDecidableUInt8Lt left right)
            (fun (_ : Not (LT.lt left right)) => Or.inl (Eq.refl left))
            (fun (_ : LT.lt left right) => Or.inr (Eq.refl right))
    }
}

fn concat(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    if bytes(&left) == 0 {
        return right;
    }
    if bytes(&right) == 0 {
        return left;
    }
    if let (Node::Leaf { text: a, .. }, Node::Leaf { text: b, .. }) = (&*left, &*right)
        && a.len().saturating_add(b.len()) <= CHUNK_MERGE_MAX
    {
        let mut joined = String::with_capacity(a.len() + b.len());
        joined.push_str(a);
        joined.push_str(b);
        return leaf(&joined);
    }
    balance(branch(left, right))
}

fn balance(node: Arc<Node>) -> Arc<Node> {
    let Node::Branch { left, right, .. } = &*node else {
        return node;
    };
    let factor = i16::from(height(left)) - i16::from(height(right));
    if factor > 1 {
        let Node::Branch {
            left: left_left,
            right: left_right,
            ..
        } = &**left
        else {
            return node;
        };
        if height(left_left) >= height(left_right) {
            return branch(left_left.clone(), branch(left_right.clone(), right.clone()));
        }
        let Node::Branch {
            left: middle_left,
            right: middle_right,
            ..
        } = &**left_right
        else {
            return node;
        };
        return branch(
            branch(left_left.clone(), middle_left.clone()),
            branch(middle_right.clone(), right.clone()),
        );
    }
    if factor < -1 {
        let Node::Branch {
            left: right_left,
            right: right_right,
            ..
        } = &**right
        else {
            return node;
        };
        if height(right_right) >= height(right_left) {
            return branch(
                branch(left.clone(), right_left.clone()),
                right_right.clone(),
            );
        }
        let Node::Branch {
            left: middle_left,
            right: middle_right,
            ..
        } = &**right_left
        else {
            return node;
        };
        return branch(
            branch(left.clone(), middle_left.clone()),
            branch(middle_right.clone(), right_right.clone()),
        );
    }
    node
}

fn split(node: Arc<Node>, offset: usize) -> (Arc<Node>, Arc<Node>) {
    if offset == 0 {
        return (empty(), node);
    }
    if offset >= bytes(&node) {
        return (node, empty());
    }
    match &*node {
        Node::Empty => (empty(), empty()),
        Node::Leaf { text, .. } => (leaf(&text[..offset]), leaf(&text[offset..])),
        Node::Branch { left, right, .. } => {
            let left_bytes = bytes(left);
            if offset < left_bytes {
                let (before, after_left) = split(left.clone(), offset);
                (before, concat(after_left, right.clone()))
            } else if offset == left_bytes {
                (left.clone(), right.clone())
            } else {
                let (before_right, after) = split(right.clone(), offset - left_bytes);
                (concat(left.clone(), before_right), after)
            }
        }
    }
}

#[inline]
fn bytes(node: &Node) -> usize {
    match node {
        Node::Empty => 0,
        Node::Leaf { text, .. } => text.len(),
        Node::Branch { bytes, .. } => *bytes,
    }
}

#[inline]
const fn newlines(node: &Node) -> usize {
    match node {
        Node::Empty => 0,
        Node::Leaf { newlines, .. } | Node::Branch { newlines, .. } => *newlines,
    }
}

#[inline]
const fn height(node: &Node) -> u8 {
    match node {
        Node::Empty => 0,
        Node::Leaf { .. } => 1,
        Node::Branch { height, .. } => *height,
    }
}

#[cfg(any(test, debug_assertions))]
fn chunk_count(node: &Node) -> usize {
    match node {
        Node::Empty => 0,
        Node::Leaf { .. } => 1,
        Node::Branch { left, right, .. } => chunk_count(left).saturating_add(chunk_count(right)),
    }
}

fn is_boundary(node: &Node, offset: usize) -> bool {
    match node {
        Node::Empty => offset == 0,
        Node::Leaf { text, .. } => text
            .as_bytes()
            .get(offset)
            .map_or(offset == text.len(), |&byte| utf8_char_start(byte)),
        Node::Branch { left, right, .. } => {
            let left_bytes = bytes(left);
            if offset < left_bytes {
                is_boundary(left, offset)
            } else if offset == left_bytes {
                true
            } else {
                is_boundary(right, offset - left_bytes)
            }
        }
    }
}

trust_contract_fn! {
    #[expect(
        clippy::manual_range_contains,
        reason = "Trust kernel-imports a bool tree of comparisons, not a call to `Range::contains` (E6 refuses calls)"
    )]
    fn utf8_char_start(byte: u8) -> bool
    clauses [ensures result == (byte < 128 || byte >= 192) by utf8_char_start_skips_continuations]
    {
        byte < 0x80 || byte >= 0xC0
    }
}

verify_island! {
    clean {
        theorem utf8_char_start_skips_continuations (byte : UInt8) :
            trust_import_amacs__text__utf8_char_start byte = (decide (byte < 128) || decide (192 <= byte)) := by
          simp only [trust_import_amacs__text__utf8_char_start]
    }
}

fn append_to(node: &Node, output: &mut String) {
    match node {
        Node::Empty => {}
        Node::Leaf { text, .. } => output.push_str(text),
        Node::Branch { left, right, .. } => {
            append_to(left, output);
            append_to(right, output);
        }
    }
}

impl PartialEq for TextRope {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.root, &other.root) || self.to_flat_string() == other.to_flat_string()
    }
}

impl Eq for TextRope {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracted_scalar_core_matches_the_std_forms_it_replaced() {
        for byte in u8::MIN..=u8::MAX {
            assert_eq!(utf8_char_start(byte), !(0x80..=0xBF).contains(&byte));
            for other in u8::MIN..=u8::MAX {
                assert_eq!(taller_child(byte, other), byte.max(other));
            }
        }
        let source = "a\u{e9}\u{20ac}\u{1f980}\n".repeat(400);
        let rope = TextRope::from(source.as_str());
        for offset in 0..=source.len().saturating_add(2) {
            assert_eq!(
                rope.is_char_boundary(offset),
                source.is_char_boundary(offset)
            );
        }
    }

    #[test]
    fn large_text_is_chunked_balanced_and_round_trips() {
        let source = (0..5_000)
            .map(|index| format!("line {index} — 🦀\n"))
            .collect::<String>();
        let rope = TextRope::from(source.as_str());
        assert!(rope.chunk_count() > 2);
        assert!(usize::from(rope.height()) <= rope.chunk_count().ilog2() as usize + 2);
        assert_eq!(rope.to_flat_string(), source);
        assert_eq!(rope.line_count(), 5_001);
    }

    #[test]
    fn persistent_replace_shares_old_snapshot_semantics() {
        let original = TextRope::from("alpha\nbeta\ngamma");
        let changed = original.replace(6..10, "BETA!!!").unwrap();
        assert_eq!(original.to_flat_string(), "alpha\nbeta\ngamma");
        assert_eq!(changed.to_flat_string(), "alpha\nBETA!!!\ngamma");
    }

    #[test]
    fn multi_edit_uses_original_coordinates() {
        let rope = TextRope::from("one two three");
        let changed = rope
            .replace_many(&[(0..3, "1"), (4..7, "2"), (8..13, "3")])
            .unwrap();
        assert_eq!(changed.to_flat_string(), "1 2 3");
    }

    #[test]
    fn invalid_utf8_boundary_is_rejected_without_mutation() {
        let rope = TextRope::from("aé🦀z");
        assert_eq!(rope.replace(2..3, "x"), Err(TextRopeError::InvalidRange));
        assert_eq!(rope.to_flat_string(), "aé🦀z");
    }
}
