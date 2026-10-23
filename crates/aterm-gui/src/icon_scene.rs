// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private Lab scenes that exercise chrome icon dispatch in the real frame renderer.

use std::sync::{Arc, OnceLock};

use aterm_core::render::{BandIcon, ChromeIcon, ChromeRaster, RenderInput};
use aterm_core::terminal::RenderCell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scene {
    BandIcons,
}

/// The shipped app never reads this seam. A private Lab instance selects a scene once.
fn configured() -> Option<Scene> {
    static SCENE: OnceLock<Option<Scene>> = OnceLock::new();
    *SCENE.get_or_init(|| {
        aterm_types::dev_seam!("ATERM_DEBUG_SCENE")
            .as_deref()
            .and_then(|s| s.to_str())
            .filter(|s| *s == "band-icons")
            .map(|_| Scene::BandIcons)
    })
}

pub(crate) fn apply(input: &mut RenderInput) {
    paint(configured(), input);
}

/// The character cells alone would use the font. The explicit icon metadata selects the
/// same procedural glyph keys that a band or menu supplies, with the live face's metrics.
fn paint(scene: Option<Scene>, input: &mut RenderInput) {
    let Some(Scene::BandIcons) = scene else {
        return;
    };
    const ROWS: [(usize, &str, bool); 3] = [
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
            let icon = match ch {
                '\u{23f8}' => Some(BandIcon::Pause),
                '!' => Some(BandIcon::Alert),
                '\u{2913}' => Some(BandIcon::ToBar),
                _ => None,
            };
            if let Some(icon) = icon
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
mod tests {
    use super::*;

    #[test]
    fn only_the_selected_scene_replaces_cells_and_supplies_procedural_marks() {
        let mut term = aterm_core::terminal::Terminal::new(12, 60);
        term.process(b"\x1b]10;#d0d0d0\x07\x1b]11;#111318\x07x");
        let mut input = term.cell_frame(12, 60);
        assert!(input.cells[4].is_empty());
        assert!(input.cells[6].is_empty());
        let original = input.clone();
        paint(None, &mut input);
        assert_eq!(input, original);
        paint(Some(Scene::BandIcons), &mut input);
        assert_eq!(input.cells[0], original.cells[0]);
        assert_eq!(input.cells[4][0].fg, original.implicit_blank.fg);
        assert_eq!(input.cells[4][0].bg, original.implicit_blank.bg);
        assert_eq!(input.chrome_rasters.len(), 2);
        for raster in &input.chrome_rasters {
            assert_eq!(
                raster.icons.iter().map(|i| i.icon).collect::<Vec<_>>(),
                [BandIcon::Pause, BandIcon::Alert, BandIcon::ToBar]
            );
            let cells = &input.cells[usize::from(raster.row)];
            for icon in &raster.icons {
                let col = usize::from(icon.col);
                assert_eq!(cells[col].ch, icon.icon.ch());
                assert_eq!(
                    aterm_render::chrome_icon_spills(icon.icon, cells, col),
                    !icon.icon.is_glass_mark()
                );
            }
        }
        assert!(!input.cells[4][0].bold);
        assert!(input.cells[6][0].bold);
        let painted = input.clone();
        paint(Some(Scene::BandIcons), &mut input);
        assert_eq!(input, painted, "repainting does not duplicate chrome marks");
    }
}
