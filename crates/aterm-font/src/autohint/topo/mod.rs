// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/topo/mod.rs and modified by the
// aterm project in 2026: the hint-plan accessors dropped.
// See ../../../LICENSE-FONTATIONS for the upstream license.

//! Topology analysis of segments and edges.

mod edges;
mod segments;

use super::{
    metrics::ScaledWidth,
    outline::{Direction, Orientation, Point},
};

pub(crate) use edges::{compute_blue_edges, compute_edges};
pub(crate) use segments::{compute_segments, link_segments};

/// The dimension of an axis.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub enum Dimension {
    /// Metrics and geometry in the horizontal direction.
    #[default]
    Horizontal = 0,
    /// Metrics and geometry in the vertical direction.
    Vertical = 1,
}

impl<T> core::ops::Index<Dimension> for [T] {
    type Output = T;

    fn index(&self, index: Dimension) -> &Self::Output {
        &self[index as usize]
    }
}

/// Segments and edges for one dimension of an outline.
#[derive(Clone, Default, Debug)]
pub struct Axis {
    /// Either horizontal or vertical.
    pub(crate) dim: Dimension,
    /// Depends on dimension and outline orientation.
    pub(crate) major_dir: Direction,
    /// Collection of segments for the axis.
    pub(crate) segments: Vec<Segment>,
    /// Collection of edges for the axis.
    pub(crate) edges: Vec<Edge>,
}

impl Axis {
    pub(crate) fn reset(&mut self, dim: Dimension, orientation: Option<Orientation>) {
        self.dim = dim;
        self.major_dir = match (dim, orientation) {
            (Dimension::Horizontal, Some(Orientation::Clockwise)) => Direction::Down,
            (Dimension::Vertical, Some(Orientation::Clockwise)) => Direction::Right,
            (Dimension::Horizontal, _) => Direction::Up,
            (Dimension::Vertical, _) => Direction::Left,
        };
        self.segments.clear();
        self.edges.clear();
    }

    /// Inserts the given edge into the sorted edge list.
    pub(crate) fn insert_edge(&mut self, edge: Edge, top_to_bottom_hinting: bool) {
        self.edges.push(edge);
        let edges = self.edges.as_mut_slice();
        if edges.len() == 1 {
            return;
        }
        let mut ix = edges.len() - 1;
        while ix > 0 {
            let prev_ix = ix - 1;
            let prev_fpos = edges[prev_ix].fpos;
            if (top_to_bottom_hinting && prev_fpos > edge.fpos)
                || (!top_to_bottom_hinting && prev_fpos < edge.fpos)
            {
                break;
            }
            // Edges with the same position and minor direction should appear
            // before those with the major direction
            if prev_fpos == edge.fpos && edge.dir == self.major_dir {
                break;
            }
            let prev_edge = edges[prev_ix];
            edges[ix] = prev_edge;
            ix -= 1;
        }
        edges[ix] = edge;
    }

    /// Links the given segment and edge.
    pub(crate) fn append_segment_to_edge(&mut self, segment_ix: usize, edge_ix: usize) {
        let edge = &mut self.edges[edge_ix];
        let first_ix = edge.first_ix;
        let last_ix = edge.last_ix;
        edge.last_ix = segment_ix as u16;
        let segment = &mut self.segments[segment_ix];
        segment.edge_next_ix = Some(first_ix);
        self.segments[last_ix as usize].edge_next_ix = Some(segment_ix as u16);
    }
}

/// Flags that define the properties of segments and edges.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct TopoFlags(pub(crate) u8);

impl TopoFlags {
    /// Regular segment or edge.
    pub const NORMAL: Self = Self(0);
    /// Segment or edge has rounded geometry.
    pub const ROUND: Self = Self(1);
    /// Segment or edge represents a serif.
    pub const SERIF: Self = Self(2);
    /// Segment or edge has been successfully processed.
    pub const DONE: Self = Self(4);
    /// Segment or edge aligns to a neutral blue zone.
    pub const NEUTRAL: Self = Self(8);
}

impl TopoFlags {
    /// Returns true if `self` contains all flags in `other`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::Not for TopoFlags {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

impl core::ops::BitOr for TopoFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for TopoFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::ops::BitAnd for TopoFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl core::ops::BitAndAssign for TopoFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

/// Sequence of points with a single dominant direction.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Segment {
    /// Flags describing the properties of the segment.
    pub(crate) flags: TopoFlags,
    /// Dominant direction of the segment.
    pub(crate) dir: Direction,
    /// Position of the segment.
    pub(crate) pos: i16,
    /// Deviation from segment position.
    pub(crate) delta: i16,
    /// Minimum coordinate of the segment.
    pub(crate) min_coord: i16,
    /// Maximum coordinate of the segment.
    pub(crate) max_coord: i16,
    /// Hinted segment height.
    pub(crate) height: i16,
    /// Used during stem matching.
    pub(crate) score: i32,
    /// Used during stem matching.
    pub(crate) len: i32,
    /// Index of best candidate for a stem link.
    pub(crate) link_ix: Option<u16>,
    /// Index of best candidate for a serif link.
    pub(crate) serif_ix: Option<u16>,
    /// Index of first point in the outline.
    pub(crate) first_ix: u16,
    /// Index of last point in the outline.
    pub(crate) last_ix: u16,
    /// Index of edge that is associated with the segment.
    pub(crate) edge_ix: Option<u16>,
    /// Index of next segment in edge's segment list.
    pub(crate) edge_next_ix: Option<u16>,
}

impl Segment {
    pub(crate) fn first(&self) -> usize {
        self.first_ix as usize
    }

    pub(crate) fn first_point<'a>(&self, points: &'a [Point]) -> &'a Point {
        &points[self.first()]
    }

    pub(crate) fn last(&self) -> usize {
        self.last_ix as usize
    }

    pub(crate) fn last_point<'a>(&self, points: &'a [Point]) -> &'a Point {
        &points[self.last()]
    }

    pub(crate) fn edge<'a>(&self, edges: &'a [Edge]) -> Option<&'a Edge> {
        edges.get(self.edge_ix.map(|ix| ix as usize)?)
    }

    /// Returns the next segment in this segment's parent edge.
    pub(crate) fn next_in_edge<'a>(&self, segments: &'a [Segment]) -> Option<&'a Segment> {
        segments.get(self.edge_next_ix.map(|ix| ix as usize)?)
    }

    pub(crate) fn link<'a>(&self, segments: &'a [Segment]) -> Option<&'a Segment> {
        segments.get(self.link_ix.map(|ix| ix as usize)?)
    }

    /// Adjust the height of the segment by half the distance between v1
    /// and v2 using wrapping arithmetic.
    pub(crate) fn adjust_height(&mut self, v1: i32, v2: i32) {
        self.height = (self.height as i32).wrapping_add(v1.wrapping_sub(v2) >> 1) as i16;
    }
}

/// Sequence of segments used for grid-fitting.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Edge {
    /// Original, unscaled position in font units.
    pub(crate) fpos: i16,
    /// Original, scaled position.
    pub(crate) opos: i32,
    /// Current position.
    pub(crate) pos: i32,
    /// Edge flags.
    pub(crate) flags: TopoFlags,
    /// Edge direction.
    pub(crate) dir: Direction,
    /// Present if this is a blue edge.
    pub(crate) blue_edge: Option<ScaledWidth>,
    /// Index of linked edge.
    pub(crate) link_ix: Option<u16>,
    /// Index of primary edge for serif.
    pub(crate) serif_ix: Option<u16>,
    /// Used to speed up edge interpolation.
    pub(crate) scale: i32,
    /// Index of first segment in edge.
    pub(crate) first_ix: u16,
    /// Index of last segment in edge.
    pub(crate) last_ix: u16,
}

impl Edge {
    pub(crate) fn link<'a>(&self, edges: &'a [Edge]) -> Option<&'a Edge> {
        edges.get(self.link_ix.map(|ix| ix as usize)?)
    }

    pub(crate) fn serif<'a>(&self, edges: &'a [Edge]) -> Option<&'a Edge> {
        edges.get(self.serif_ix.map(|ix| ix as usize)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_adjust_height_extreme_values_do_not_panic() {
        let mut segment = Segment {
            height: i16::MAX,
            ..Default::default()
        };
        segment.adjust_height(i32::MIN, i32::MAX);
        segment.adjust_height(i32::MAX, i32::MIN);
    }
}
