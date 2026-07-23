// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Persistent balanced UTF-8 text storage for native documents.
//!
//! Leaves are bounded UTF-8 chunks; branches cache byte/newline counts and AVL height.
//! Cloning a snapshot is O(1), byte split/replace and line lookup are logarithmic in the
//! number of chunks, and untouched subtrees are structurally shared. `DocumentStore`
//! lowers the resulting text through its authoritative `aterm_buffer::Surface` commit.

#![allow(
    dead_code,
    reason = "native document renderer integration lands in stages"
)]

use std::ops::Range;
use std::sync::Arc;

const CHUNK_TARGET: usize = 4 * 1024;
const CHUNK_MERGE_MAX: usize = CHUNK_TARGET * 2;

#[derive(Clone, Debug)]
pub(crate) struct TextRope {
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
pub(crate) enum TextRopeError {
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
                    .map_or(text.len(), |(relative, _)| start + relative);
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
    pub(crate) fn len(&self) -> usize {
        bytes(&self.root)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn line_count(&self) -> usize {
        newlines(&self.root).saturating_add(1)
    }

    pub(crate) fn chunk_count(&self) -> usize {
        chunk_count(&self.root)
    }

    pub(crate) fn height(&self) -> u8 {
        height(&self.root)
    }

    pub(crate) fn is_char_boundary(&self, offset: usize) -> bool {
        if offset > self.len() {
            return false;
        }
        is_boundary(&self.root, offset)
    }

    pub(crate) fn to_flat_string(&self) -> String {
        let mut output = String::with_capacity(self.len());
        append_to(&self.root, &mut output);
        output
    }

    pub(crate) fn slice(&self, range: Range<usize>) -> Result<String, TextRopeError> {
        self.validate(&range)?;
        let mut output = String::with_capacity(range.end.saturating_sub(range.start));
        append_range(&self.root, 0, &range, &mut output);
        Ok(output)
    }

    pub(crate) fn replace(&self, range: Range<usize>, insert: &str) -> Result<Self, TextRopeError> {
        self.validate(&range)?;
        let (before, tail) = split(self.root.clone(), range.start);
        let (_, after) = split(tail, range.end.saturating_sub(range.start));
        let inserted = TextRope::from(insert).root;
        Ok(Self {
            root: concat(concat(before, inserted), after),
        })
    }

    /// Apply non-overlapping ranges expressed in this snapshot's coordinates.
    pub(crate) fn replace_many(
        &self,
        edits: &[(Range<usize>, &str)],
    ) -> Result<Self, TextRopeError> {
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

    /// Zero-based logical line containing `offset`.
    pub(crate) fn line_of_byte(&self, offset: usize) -> Result<usize, TextRopeError> {
        if offset > self.len() || !self.is_char_boundary(offset) {
            return Err(TextRopeError::InvalidRange);
        }
        Ok(count_newlines_before(&self.root, offset))
    }

    /// Byte start of a zero-based logical line; the one-past-final line returns `len`.
    pub(crate) fn byte_of_line(&self, line: usize) -> Option<usize> {
        if line == 0 {
            return Some(0);
        }
        if line >= self.line_count() {
            return None;
        }
        find_nth_newline(&self.root, line - 1).map(|offset| offset.saturating_add(1))
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
        height: height(&left).max(height(&right)).saturating_add(1),
        left,
        right,
    })
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

fn bytes(node: &Node) -> usize {
    match node {
        Node::Empty => 0,
        Node::Leaf { text, .. } => text.len(),
        Node::Branch { bytes, .. } => *bytes,
    }
}

fn newlines(node: &Node) -> usize {
    match node {
        Node::Empty => 0,
        Node::Leaf { newlines, .. } | Node::Branch { newlines, .. } => *newlines,
    }
}

fn height(node: &Node) -> u8 {
    match node {
        Node::Empty => 0,
        Node::Leaf { .. } => 1,
        Node::Branch { height, .. } => *height,
    }
}

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
        Node::Leaf { text, .. } => text.is_char_boundary(offset),
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

fn append_range(node: &Node, base: usize, range: &Range<usize>, output: &mut String) {
    let end = base.saturating_add(bytes(node));
    if range.end <= base || range.start >= end {
        return;
    }
    match node {
        Node::Empty => {}
        Node::Leaf { text, .. } => {
            let start = range.start.saturating_sub(base).min(text.len());
            let end = range.end.saturating_sub(base).min(text.len());
            output.push_str(&text[start..end]);
        }
        Node::Branch { left, right, .. } => {
            append_range(left, base, range, output);
            append_range(right, base.saturating_add(bytes(left)), range, output);
        }
    }
}

fn count_newlines_before(node: &Node, offset: usize) -> usize {
    match node {
        Node::Empty => 0,
        Node::Leaf { text, .. } => text[..offset].bytes().filter(|byte| *byte == b'\n').count(),
        Node::Branch { left, right, .. } => {
            let left_bytes = bytes(left);
            if offset <= left_bytes {
                count_newlines_before(left, offset)
            } else {
                newlines(left).saturating_add(count_newlines_before(right, offset - left_bytes))
            }
        }
    }
}

fn find_nth_newline(node: &Node, target: usize) -> Option<usize> {
    match node {
        Node::Empty => None,
        Node::Leaf { text, .. } => text
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(index))
            .nth(target),
        Node::Branch { left, right, .. } => {
            let left_lines = newlines(left);
            if target < left_lines {
                find_nth_newline(left, target)
            } else {
                find_nth_newline(right, target - left_lines)
                    .map(|offset| bytes(left).saturating_add(offset))
            }
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
        assert_eq!(changed.slice(6..13).unwrap(), "BETA!!!");
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
    fn line_lookup_crosses_chunk_boundaries() {
        let source = (0..2_000).map(|_| "abc\n").collect::<String>();
        let rope = TextRope::from(source.as_str());
        for line in [0, 1, 999, 1_999] {
            let byte = rope.byte_of_line(line).unwrap();
            assert_eq!(byte, line * 4);
            assert_eq!(rope.line_of_byte(byte).unwrap(), line);
        }
    }

    #[test]
    fn invalid_utf8_boundary_is_rejected_without_mutation() {
        let rope = TextRope::from("aé🦀z");
        assert_eq!(rope.replace(2..3, "x"), Err(TextRopeError::InvalidRange));
        assert_eq!(rope.to_flat_string(), "aé🦀z");
    }
}
