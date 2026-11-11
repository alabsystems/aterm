// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_atspi_common 0.19.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/util.rs, and changed by the aterm project in 2026.

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
#[path = "util_tests.rs"]
mod tests;
