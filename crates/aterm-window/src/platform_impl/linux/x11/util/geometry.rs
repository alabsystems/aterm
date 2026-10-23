// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/geometry.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (aterm-x11's requests, where
// upstream used x11rb's. `get_frame_extents_heuristic` returns a request's failure, where
// upstream's `expect`s panicked on a display that had gone.)

use std::cmp;

use aterm_x11::proto::atom;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaRect {
    x: i64,
    y: i64,
    width: i64,
    height: i64,
}

impl AaRect {
    pub fn new((x, y): (i32, i32), (width, height): (u32, u32)) -> Self {
        let (x, y) = (x as i64, y as i64);
        let (width, height) = (width as i64, height as i64);
        AaRect { x, y, width, height }
    }

    pub fn contains_point(&self, x: i64, y: i64) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }

    pub fn get_overlapping_area(&self, other: &Self) -> i64 {
        let x_overlap = cmp::max(
            0,
            cmp::min(self.x + self.width, other.x + other.width) - cmp::max(self.x, other.x),
        );
        let y_overlap = cmp::max(
            0,
            cmp::min(self.y + self.height, other.y + other.height) - cmp::max(self.y, other.y),
        );
        x_overlap * y_overlap
    }
}

#[derive(Debug, Clone)]
pub struct FrameExtents {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

impl FrameExtents {
    pub fn new(left: u32, right: u32, top: u32, bottom: u32) -> Self {
        FrameExtents { left, right, top, bottom }
    }

    pub fn from_border(border: u32) -> Self {
        Self::new(border, border, border, border)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameExtentsHeuristicPath {
    Supported,
    UnsupportedNested,
    UnsupportedBordered,
}

#[derive(Debug, Clone)]
pub struct FrameExtentsHeuristic {
    pub frame_extents: FrameExtents,
    pub heuristic_path: FrameExtentsHeuristicPath,
}

impl FrameExtentsHeuristic {
    pub fn inner_pos_to_outer(&self, x: i32, y: i32) -> (i32, i32) {
        use self::FrameExtentsHeuristicPath::*;
        if self.heuristic_path != UnsupportedBordered {
            (x - self.frame_extents.left as i32, y - self.frame_extents.top as i32)
        } else {
            (x, y)
        }
    }

    pub fn inner_size_to_outer(&self, width: u32, height: u32) -> (u32, u32) {
        (
            width.saturating_add(
                self.frame_extents.left.saturating_add(self.frame_extents.right) as _
            ),
            height.saturating_add(
                self.frame_extents.top.saturating_add(self.frame_extents.bottom) as _
            ),
        )
    }
}

impl XConnection {
    pub fn translate_coords(
        &self,
        window: xproto::Window,
        root: xproto::Window,
    ) -> Result<xproto::TranslateCoordinatesReply, X11Error> {
        self.request(xproto::translate_coordinates(window, root, 0, 0))
    }

    pub fn get_geometry(
        &self,
        window: xproto::Window,
    ) -> Result<xproto::GetGeometryReply, X11Error> {
        self.request(xproto::get_geometry(window))
    }

    fn get_frame_extents(&self, window: xproto::Window) -> Option<FrameExtents> {
        let atoms = self.atoms();
        let extents_atom = atoms[_NET_FRAME_EXTENTS];

        if !hint_is_supported(extents_atom) {
            return None;
        }

        let extents: Option<Vec<u32>> =
            self.get_property(window, extents_atom, atom::CARDINAL).ok();

        extents.and_then(|extents| {
            if extents.len() >= 4 {
                Some(FrameExtents {
                    left: extents[0],
                    right: extents[1],
                    top: extents[2],
                    bottom: extents[3],
                })
            } else {
                None
            }
        })
    }

    pub fn is_top_level(&self, window: xproto::Window, root: xproto::Window) -> Option<bool> {
        let atoms = self.atoms();
        let client_list_atom = atoms[_NET_CLIENT_LIST];

        if !hint_is_supported(client_list_atom) {
            return None;
        }

        let client_list: Option<Vec<xproto::Window>> =
            self.get_property(root, client_list_atom, atom::WINDOW).ok();

        client_list.map(|client_list| client_list.contains(&(window as xproto::Window)))
    }

    fn get_parent_window(&self, window: xproto::Window) -> Result<xproto::Window, X11Error> {
        let parent = self.request(xproto::query_tree(window))?.parent;
        Ok(parent)
    }

    fn climb_hierarchy(
        &self,
        window: xproto::Window,
        root: xproto::Window,
    ) -> Result<xproto::Window, X11Error> {
        let mut outer_window = window;
        loop {
            let candidate = self.get_parent_window(outer_window)?;
            if candidate == root {
                break;
            }
            outer_window = candidate;
        }
        Ok(outer_window)
    }

    /// Query the window's frame extents using `_NET_FRAME_EXTENTS`, hierarchy climbing, or border
    /// width.
    pub fn get_frame_extents_heuristic(
        &self,
        window: xproto::Window,
        root: xproto::Window,
    ) -> Result<FrameExtentsHeuristic, X11Error> {
        use self::FrameExtentsHeuristicPath::*;

        let (inner_y_rel_root, child) = {
            let coords = self.translate_coords(window, root)?;
            (coords.dst_y, coords.child)
        };

        let (width, height, border) = {
            let inner_geometry = self.get_geometry(window)?;
            (inner_geometry.width, inner_geometry.height, inner_geometry.border_width)
        };

        let nested = !(window == child || self.is_top_level(child, root) == Some(true));

        if let Some(mut frame_extents) = self.get_frame_extents(window) {
            if !nested {
                frame_extents = FrameExtents::new(0, 0, 0, 0);
            }

            Ok(FrameExtentsHeuristic { frame_extents, heuristic_path: Supported })
        } else if nested {
            let outer_window = self.climb_hierarchy(window, root)?;
            let (outer_y, outer_width, outer_height) = {
                let outer_geometry = self.get_geometry(outer_window)?;
                (outer_geometry.y, outer_geometry.width, outer_geometry.height)
            };

            let diff_x = outer_width.saturating_sub(width) as u32;
            let diff_y = outer_height.saturating_sub(height) as u32;
            let offset_y = inner_y_rel_root.saturating_sub(outer_y) as u32;

            let left = diff_x / 2;
            let right = left;
            let top = offset_y;
            let bottom = diff_y.saturating_sub(offset_y);

            let frame_extents = FrameExtents::new(left, right, top, bottom);
            Ok(FrameExtentsHeuristic { frame_extents, heuristic_path: UnsupportedNested })
        } else {
            let frame_extents = FrameExtents::from_border(border.into());
            Ok(FrameExtentsHeuristic { frame_extents, heuristic_path: UnsupportedBordered })
        }
    }
}
