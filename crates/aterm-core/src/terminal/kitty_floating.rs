// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kitty placements relative to a virtual placement ("floating" placements).

use std::num::NonZeroU32;

use aterm_grid::{Grid, MAX_IMAGE_LAYERS};

use super::kitty_placeholder;
use super::transient_state::{KittyFloatParent, KittyParent, KittyVirtual, TransientState};

/// Maximum chain of parents above a relative placement. Kitty requires at
/// least 8.
pub(super) const MAX_KITTY_RELATIVE_DEPTH: usize = 16;

/// Maximum floating placements a terminal keeps (a DoS bound; kitty has
/// none). Each covers a cell at most once, so their tiles in any one cell
/// stay within the layers a cell holds, and walking them per frame costs at
/// most what walking a full stack of cell layers does.
pub(super) const MAX_KITTY_FLOATING: usize = MAX_IMAGE_LAYERS;

/// One on-screen cell of a floating placement.
#[derive(Debug, Clone, Copy)]
pub(super) struct FloatingTile {
    /// The cell's screen row.
    pub(super) row: u16,
    /// The cell's column.
    pub(super) col: u16,
    /// The placement: its index in `kitty_floating`.
    pub(super) index: usize,
    /// Which footprint row the cell shows.
    pub(super) cell_row: u16,
    /// Which footprint column the cell shows.
    pub(super) cell_col: u16,
}

/// The virtual placement a Unicode placeholder naming image `image_id` and
/// placement `placement_id` shows: that placement (its newest, should one id
/// name several), or for placement `0` the image's newest virtual placement —
/// kitty lets the terminal choose.
#[inline]
pub(super) fn placeholder_virtual(
    transient: &TransientState,
    image_id: u32,
    placement_id: u32,
) -> Option<&KittyVirtual> {
    let virtuals = transient.kitty_virtual.get(&image_id)?;
    if placement_id == 0 {
        return virtuals.last();
    }
    virtuals
        .iter()
        .rev()
        .find(|v| v.tag.placement_id == placement_id)
}

/// The virtual placement `parent` names — the newest, should it name several.
#[inline]
pub(super) fn named_virtual(
    transient: &TransientState,
    parent: KittyParent,
) -> Option<&KittyVirtual> {
    transient
        .kitty_virtual
        .get(&parent.image_id())?
        .iter()
        .rev()
        .find(|v| parent.names(&v.tag))
}

/// The index in `kitty_floating` of the floating placement `parent` names —
/// the newest, should it name several.
#[inline]
pub(super) fn named_floating(transient: &TransientState, parent: KittyParent) -> Option<usize> {
    transient
        .kitty_floating
        .iter()
        .enumerate()
        .filter(|(_, f)| parent.names(&f.tag))
        .max_by_key(|(_, f)| f.tag.serial)
        .map(|(index, _)| index)
}

/// Whether `parent` still names a placement a floating placement can follow.
#[inline]
pub(super) fn parent_exists(transient: &TransientState, parent: KittyFloatParent) -> bool {
    match parent {
        KittyFloatParent::Virtual(p) => named_virtual(transient, p).is_some(),
        KittyFloatParent::Floating(p) => named_floating(transient, p).is_some(),
    }
}

/// Every on-screen cell of the floating placements, placement by placement
/// in `kitty_floating` order. Empty — and free — while there are none.
pub(super) fn floating_tiles(grid: &Grid, transient: &TransientState) -> Vec<FloatingTile> {
    let mut tiles = Vec::new();
    let origins = floating_origins(grid, transient);
    for (index, (floating, origin)) in transient.kitty_floating.iter().zip(origins).enumerate() {
        let Some((top, left)) = origin else {
            continue;
        };
        for cell_row in 0..floating.image.rows {
            let Ok(row) = u16::try_from(top.saturating_add(i32::from(cell_row))) else {
                continue;
            };
            if row >= grid.rows() {
                break;
            }
            for cell_col in 0..floating.image.cols {
                let Ok(col) = u16::try_from(left.saturating_add(i32::from(cell_col))) else {
                    continue;
                };
                if col >= grid.cols() {
                    break;
                }
                tiles.push(FloatingTile {
                    row,
                    col,
                    index,
                    cell_row,
                    cell_col,
                });
            }
        }
    }
    tiles
}

/// Where each floating placement's top-left cell is on the live screen now,
/// `(row, col)` — off screen when negative or past the edge — indexed like
/// `kitty_floating`; `None` while its virtual ancestor's placeholders show
/// nowhere on screen, or its chain of parents is broken or too deep.
pub(super) fn floating_origins(grid: &Grid, transient: &TransientState) -> Vec<Option<(i32, i32)>> {
    if transient.kitty_floating.is_empty() {
        return Vec::new();
    }
    let mut shown: Vec<(NonZeroU32, i32, i32)> = Vec::new();
    let (rows, cols) = (grid.rows(), grid.cols());
    for (at, extra) in grid.extras().iter() {
        if at.row >= rows || at.col >= cols {
            continue;
        }
        let Some(cell) = kitty_placeholder::decode_cell(grid, at.row, at.col, extra) else {
            continue;
        };
        let Some(v) = placeholder_virtual(transient, cell.image_id, cell.placement_id) else {
            continue;
        };
        let (row, col) = (i32::from(at.row), i32::from(at.col));
        match shown
            .iter_mut()
            .find(|(serial, ..)| *serial == v.tag.serial)
        {
            Some((_, top, left)) => {
                *top = (*top).min(row);
                *left = (*left).min(col);
            }
            None => shown.push((v.tag.serial, row, col)),
        }
    }
    (0..transient.kitty_floating.len())
        .map(|index| origin(transient, &shown, index, 0))
        .collect()
}

fn origin(
    transient: &TransientState,
    shown: &[(NonZeroU32, i32, i32)],
    index: usize,
    depth: usize,
) -> Option<(i32, i32)> {
    if depth >= MAX_KITTY_RELATIVE_DEPTH {
        return None;
    }
    let floating = transient.kitty_floating.get(index)?;
    let (top, left) = match floating.parent {
        KittyFloatParent::Virtual(parent) => {
            let serial = named_virtual(transient, parent)?.tag.serial;
            shown
                .iter()
                .find(|(s, ..)| *s == serial)
                .map(|&(_, top, left)| (top, left))?
        }
        KittyFloatParent::Floating(parent) => origin(
            transient,
            shown,
            named_floating(transient, parent)?,
            depth + 1,
        )?,
    };
    Some((
        top.saturating_add(floating.dy),
        left.saturating_add(floating.dx),
    ))
}
