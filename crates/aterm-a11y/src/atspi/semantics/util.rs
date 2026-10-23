// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_atspi_common 0.19.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/util.rs.

//! AT-SPI window coordinate conversions and text offset utilities.

use crate::model::{Point, Rect, ScrollHint};
use crate::text::{Position as TextPosition, Range as TextRange};
use crate::tree::Node;

use super::{CoordType, Error, Granularity, ScrollType};

/// Where a window's client area is on the screen.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct WindowBounds {
    pub(crate) inner: Rect,
}

impl WindowBounds {
    /// A window's bounds from its inner rectangle; the outer one is unused.
    pub(crate) fn new(_outer: Rect, inner: Rect) -> Self {
        Self { inner }
    }

    pub(crate) fn accesskit_point_to_atspi_point(
        &self,
        point: Point,
        parent: Option<Node>,
        coord_type: CoordType,
    ) -> Point {
        let origin = self.origin(parent, coord_type);
        Point::new(origin.x + point.x, origin.y + point.y)
    }

    pub(crate) fn atspi_point_to_accesskit_point(
        &self,
        point: Point,
        parent: Option<Node>,
        coord_type: CoordType,
    ) -> Point {
        let origin = self.origin(parent, coord_type);
        Point::new(point.x - origin.x, point.y - origin.y)
    }

    fn origin(&self, parent: Option<Node>, coord_type: CoordType) -> Point {
        match coord_type {
            CoordType::Screen => self.inner.origin(),
            CoordType::Window => Point::ZERO,
            CoordType::Parent => {
                if let Some(parent) = parent {
                    let parent_origin = parent.bounding_box().unwrap_or_default().origin();
                    Point::new(-parent_origin.x, -parent_origin.y)
                } else {
                    self.inner.origin()
                }
            }
        }
    }
}

pub(crate) fn text_position_from_offset<'a>(
    node: &Node<'a>,
    offset: i32,
) -> Option<TextPosition<'a>> {
    let index = offset.try_into().ok()?;
    node.text_position_from_global_usv_index(index)
}

pub(crate) fn text_range_from_offset<'a>(
    node: &Node<'a>,
    offset: i32,
    granularity: Granularity,
) -> Result<TextRange<'a>, Error> {
    let start_offset = text_position_from_offset(node, offset).ok_or(Error::IndexOutOfRange)?;
    let start = match granularity {
        Granularity::Char => start_offset,
        Granularity::Line if start_offset.is_line_start() => start_offset,
        Granularity::Line => start_offset.backward_to_line_start(),
        Granularity::Paragraph if start_offset.is_paragraph_start() => start_offset,
        Granularity::Paragraph => start_offset.backward_to_paragraph_start(),
        Granularity::Sentence => return Err(Error::UnsupportedTextGranularity),
        Granularity::Word if start_offset.is_word_start() => start_offset,
        Granularity::Word => start_offset.backward_to_word_start(),
    };
    let end = match granularity {
        Granularity::Char if start_offset.is_document_end() => start_offset,
        Granularity::Char => start.forward_to_character_end(),
        Granularity::Line => start.forward_to_line_end(),
        Granularity::Paragraph => start.forward_to_paragraph_end(),
        Granularity::Sentence => return Err(Error::UnsupportedTextGranularity),
        Granularity::Word => start.forward_to_word_end(),
    };
    let mut range = start.to_degenerate_range();
    range.set_end(end);
    Ok(range)
}

pub(crate) fn text_range_from_offsets<'a>(
    node: &Node<'a>,
    start_offset: i32,
    end_offset: i32,
) -> Option<TextRange<'a>> {
    let start = text_position_from_offset(node, start_offset)?;
    let end = if end_offset == -1 {
        node.document_range().end()
    } else {
        text_position_from_offset(node, end_offset)?
    };

    let mut range = start.to_degenerate_range();
    range.set_end(end);
    Some(range)
}

pub(crate) fn text_range_bounds_from_offsets(
    node: &Node,
    start_offset: i32,
    end_offset: i32,
) -> Option<Rect> {
    text_range_from_offsets(node, start_offset, end_offset)?
        .bounding_boxes()
        .into_iter()
        .reduce(|rect1, rect2| rect1.union(rect2))
}

pub(crate) fn atspi_scroll_type_to_scroll_hint(scroll_type: ScrollType) -> Option<ScrollHint> {
    match scroll_type {
        ScrollType::TopLeft => Some(ScrollHint::TopLeft),
        ScrollType::BottomRight => Some(ScrollHint::BottomRight),
        ScrollType::TopEdge => Some(ScrollHint::TopEdge),
        ScrollType::BottomEdge => Some(ScrollHint::BottomEdge),
        ScrollType::LeftEdge => Some(ScrollHint::LeftEdge),
        ScrollType::RightEdge => Some(ScrollHint::RightEdge),
        ScrollType::Anywhere => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::model::{Node as NodeData, NodeId as LocalId, Role, Tree as TreeData, TreeUpdate};
    use crate::tree::Tree;

    /// A text viewport like aterm-gui's Settings editor.
    fn viewport(lines: &[&str]) -> TreeUpdate {
        let mut root = NodeData::new(Role::MultilineTextInput);
        let runs: Vec<LocalId> = (1..=lines.len() as u64).map(LocalId).collect();
        root.set_children(runs.clone());
        let mut nodes = vec![(LocalId(0), root)];
        for (id, line) in runs.into_iter().zip(lines) {
            let mut run = NodeData::new(Role::TextRun);
            run.set_value(*line);
            run.set_character_lengths(line.chars().map(|c| c.len_utf8() as u8).collect::<Vec<_>>());
            nodes.push((id, run));
        }
        TreeUpdate {
            nodes,
            tree: Some(TreeData::new(LocalId(0))),
            tree_id: crate::model::TreeId::ROOT,
            focus: LocalId(0),
        }
    }

    type Answers = Vec<(i32, Granularity, Option<(String, usize, usize)>)>;

    /// Every granularity at every offset, as `(text, start, end)`.
    fn answers(lines: &'static [&'static str]) -> Answers {
        let tree = Tree::new(viewport(lines), true);
        let node = tree.state().root();
        let count = lines.iter().map(|l| l.chars().count()).sum::<usize>() as i32;
        let mut out = Vec::new();
        for offset in 0..=count {
            for g in [
                Granularity::Char,
                Granularity::Word,
                Granularity::Line,
                Granularity::Paragraph,
            ] {
                let answer = text_range_from_offset(&node, offset, g).ok().map(|r| {
                    (
                        r.text(),
                        r.start().to_global_usv_index(),
                        r.end().to_global_usv_index(),
                    )
                });
                out.push((offset, g, answer));
            }
        }
        out
    }

    /// `answers`, on a thread.
    fn answered(lines: &'static [&'static str]) -> Answers {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(answers(lines));
        });
        rx.recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("every search over {lines:?} returns"))
    }

    fn at(got: &Answers, offset: i32, g: Granularity) -> Option<(&str, usize, usize)> {
        got.iter()
            .find(|(o, gg, _)| *o == offset && *gg == g)
            .and_then(|(_, _, a)| a.as_ref())
            .map(|(text, start, end)| (text.as_str(), *start, *end))
    }

    /// accesskit_consumer 0.37.0's paragraph search never returned here:
    /// walking back from "cd" to the blank line's start biased forward onto
    /// "cd" again (the semantics oracle's recorder hung on this shape).
    #[test]
    fn a_blank_line_ends_no_paragraph_search() {
        let got = answered(&["ab", "", "cd"]);
        assert_eq!(at(&got, 3, Granularity::Paragraph), Some(("abcd", 0, 4)));
        assert_eq!(at(&got, 0, Granularity::Paragraph), Some(("abcd", 0, 4)));
        assert_eq!(at(&got, 3, Granularity::Line), Some(("cd", 2, 4)));
        assert_eq!(at(&got, 1, Granularity::Char), Some(("b", 1, 2)));
        let control = answered(&["ab", "cd"]);
        assert_eq!(
            at(&control, 3, Granularity::Paragraph),
            Some(("abcd", 0, 4))
        );
    }

    /// A viewport whose top line is blank (Settings or the Manual scrolled
    /// to an empty line): accesskit_consumer's walk back from the first
    /// text found nothing before the blank line and answered where it
    /// began, at every offset, so its paragraph search never returned.
    #[test]
    fn a_blank_first_line_ends_no_paragraph_search() {
        let got = answered(&["", "cd"]);
        for offset in 0..=2 {
            assert_eq!(at(&got, offset, Granularity::Paragraph), Some(("cd", 0, 2)));
        }
        assert_eq!(at(&got, 1, Granularity::Line), Some(("cd", 0, 2)));
        let got = answered(&["", "", "ab"]);
        for offset in 0..=2 {
            assert_eq!(at(&got, offset, Granularity::Paragraph), Some(("ab", 0, 2)));
        }
        let got = answered(&["", ""]);
        assert_eq!(at(&got, 0, Granularity::Paragraph), Some(("", 0, 0)));
    }

    /// A line after a blank one that follows a line break starts a
    /// paragraph: the break, not the blank line, is the text before it.
    #[test]
    fn a_line_after_a_blank_one_starts_the_paragraph_a_break_began() {
        let got = answered(&["x\n", "", "ab"]);
        for offset in 2..=4 {
            assert_eq!(at(&got, offset, Granularity::Paragraph), Some(("ab", 2, 4)));
        }
        for offset in 0..=1 {
            assert_eq!(
                at(&got, offset, Granularity::Paragraph),
                Some(("x\n", 0, 2))
            );
        }
    }
}
