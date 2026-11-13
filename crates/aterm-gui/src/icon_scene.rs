// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private Lab scenes that exercise chrome in the real frame renderer.

use std::sync::{Arc, OnceLock};

use aterm_core::render::{BandIcon, ChromeIcon, ChromeRaster, RenderInput};
use aterm_core::terminal::RenderCell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scene {
    BandIcons,
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    HarnessLayouts,
}

/// The shipped app never reads this seam. A private Lab instance selects a scene once.
fn configured() -> Option<Scene> {
    static SCENE: OnceLock<Option<Scene>> = OnceLock::new();
    *SCENE.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_DEBUG_SCENE")
            .as_deref()
            .and_then(|s| s.to_str())
            .and_then(|s| match s {
                "band-icons" => Some(Scene::BandIcons),
                #[cfg(any(debug_assertions, feature = "dev-seams"))]
                "harness-layouts" => Some(Scene::HarnessLayouts),
                _ => None,
            })
    })
}

/// Whether an explicit private scene was selected; ordinary frames need no scene metrics.
pub(crate) fn active() -> bool {
    configured().is_some()
}

pub(crate) fn apply(input: &mut RenderInput, cell: (u16, u16)) {
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    if configured() == Some(Scene::HarnessLayouts) {
        crate::claude_footer::layout_scene::paint(
            input,
            (usize::from(cell.0), usize::from(cell.1)),
        );
        return;
    }
    let _ = cell;
    paint(configured(), input);
}

/// The character cells alone would use the font. The explicit icon metadata selects the
/// same procedural glyph keys that a band or menu supplies, with the live face's metrics.
fn paint(scene: Option<Scene>, input: &mut RenderInput) {
    let Some(Scene::BandIcons) = scene else {
        return;
    };
    const ROWS: [(usize, &str, bool); 5] = [
        (2, "Band icons: H is the capitals reference", false),
        (
            4,
            "H \u{23f8} pause   H ! alert   H \u{2913}18:00 land   regular",
            false,
        ),
        (
            6,
            "H \u{23f8} pause   H ! alert   H \u{2913}18:00 land   bold",
            true,
        ),
        (
            8,
            "H \u{26bf} lock   H \u{29d6} hourglass   H \u{1f5b4} drive   regular",
            false,
        ),
        (
            10,
            "H \u{26bf} lock   H \u{29d6} hourglass   H \u{1f5b4} drive   bold",
            true,
        ),
    ];
    for (offset, text, bold) in ROWS {
        let row = input.grid_top_row.saturating_add(offset);
        let Ok(raster_row) = u16::try_from(row) else {
            continue;
        };
        let Some(cells) = input.cells.get_mut(row) else {
            continue;
        };
        let template = cells.first().copied().unwrap_or(input.implicit_blank);
        let blank = RenderCell {
            fg: template.fg,
            bg: template.bg,
            bold,
            ..RenderCell::default()
        };
        cells.clear();
        cells.resize(input.cols, blank);
        let mut icons = Vec::new();
        for (col, ch) in text.chars().take(input.cols).enumerate() {
            cells[col].ch = ch;
            if let Some(icon) = BandIcon::ALL.into_iter().find(|i| i.ch() == ch)
                && let Ok(col) = u16::try_from(col)
            {
                icons.push(ChromeIcon { col, icon });
            }
        }
        if let Some(entries) = input.clusters.get_mut(row) {
            entries.clear();
        }
        if let Some(entries) = input.combining.get_mut(row) {
            entries.clear();
        }
        if let Some(entries) = input.images.get_mut(row) {
            entries.clear();
        }
        if let Some(entries) = input.default_bg_spans.get_mut(row) {
            entries.clear();
        }
        input.chrome_rasters.retain(|r| r.row != raster_row);
        if !icons.is_empty() {
            input.chrome_rasters.push(ChromeRaster {
                row: raster_row,
                ground: Arc::from(Vec::new()),
                rail: Arc::from(Vec::new()),
                rail_h: 0,
                clear_rail: false,
                own: Vec::new(),
                split: None,
                rings: Vec::new(),
                icons,
            });
        }
    }
    input.cursor_visible = false;
}

#[cfg(test)]
#[path = "icon_scene_tests.rs"]
mod tests;
