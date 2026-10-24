// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private Lab scenes in the real frame renderer (`ATERM_DEBUG_SCENE`, `lab instance --scene`):
//! `band-icons` exercises chrome icon dispatch, and in a debug build `deck-busy`, `deck-calm`
//! and `deck-absent` put one of the merged deck's scenes ([`aterm_vitals::fixtures`]) in place
//! of the machine.

use std::sync::{Arc, OnceLock};

use aterm_core::render::{BandIcon, ChromeIcon, ChromeRaster, RenderInput};
use aterm_core::terminal::RenderCell;
#[cfg(debug_assertions)]
use aterm_vitals::{
    fixtures::{self, DeckScene},
    model::Snapshot,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scene {
    BandIcons,
    /// One of the merged deck's scenes: each reading is the scene's ([`deck_reading`]) and the
    /// deck's front tab is the scene window's (`stats_deck::deck_fixtures::DeckContext`).
    #[cfg(debug_assertions)]
    Deck(DeckScene),
}

impl Scene {
    /// The scene `name` selects: `band-icons`, or `deck-` and a deck scene's name.
    fn named(name: &str) -> Option<Self> {
        if name == "band-icons" {
            return Some(Self::BandIcons);
        }
        #[cfg(debug_assertions)]
        if let Some(deck) = name.strip_prefix("deck-") {
            return DeckScene::ALL
                .into_iter()
                .find(|scene| scene.name() == deck)
                .map(Self::Deck);
        }
        None
    }
}

/// The shipped app never reads this seam. A private Lab instance selects a scene once.
fn configured() -> Option<Scene> {
    static SCENE: OnceLock<Option<Scene>> = OnceLock::new();
    *SCENE.get_or_init(|| {
        let named = aterm_types::dev_seam!("ATERM_DEBUG_SCENE")?;
        let scene = named.to_str().and_then(Scene::named);
        if scene.is_none() {
            aterm_log::warn!(
                "ATERM_DEBUG_SCENE={} names no scene this build has",
                named.to_string_lossy()
            );
        }
        scene
    })
}

/// The deck scene the seam selects, if it selects one.
#[cfg(debug_assertions)]
pub(crate) fn deck() -> Option<DeckScene> {
    match configured()? {
        Scene::Deck(scene) => Some(scene),
        Scene::BandIcons => None,
    }
}

/// The reading `live` as `scene` has it: the scene's snapshot ([`fixtures::snapshot`]) under
/// `live`'s sequence number, so each reading is a new one and the deck's repaint path runs at
/// the readings' cadence. The scene keeps its own clock (`at_unix_ms`, `at_ns`), since its runs,
/// rings and free history end there and at the machine's clock would read as empty. `live`
/// itself when no scene is selected.
#[cfg(debug_assertions)]
pub(crate) fn deck_reading(scene: Option<DeckScene>, live: Arc<Snapshot>) -> Arc<Snapshot> {
    match scene {
        Some(scene) => Arc::new(Snapshot {
            seq: live.seq,
            ..fixtures::snapshot(scene)
        }),
        None => live,
    }
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
mod tests {
    use super::*;

    /// A seam value selects a scene only by its exact name; any other selects none.
    #[test]
    fn a_seam_value_names_a_scene_or_none() {
        assert_eq!(Scene::named("band-icons"), Some(Scene::BandIcons));
        for scene in DeckScene::ALL {
            assert_eq!(
                Scene::named(&format!("deck-{}", scene.name())),
                Some(Scene::Deck(scene))
            );
        }
        for other in [
            "",
            "deck",
            "deck-",
            "deck-hot",
            "Deck-busy",
            "busy",
            "band-icons ",
        ] {
            assert_eq!(Scene::named(other), None, "{other:?}");
        }
    }

    #[test]
    fn only_the_selected_scene_replaces_cells_and_supplies_procedural_marks() {
        let mut term = aterm_core::terminal::Terminal::new(12, 60);
        term.process(b"\x1b]10;#d0d0d0\x07\x1b]11;#111318\x07x");
        let mut input = term.cell_frame(12, 60);
        for row in [4, 6, 8, 10] {
            assert!(input.cells[row].is_empty());
        }
        let original = input.clone();
        paint(None, &mut input);
        assert_eq!(input, original);
        paint(Some(Scene::BandIcons), &mut input);
        assert_eq!(input.cells[0], original.cells[0]);
        assert_eq!(input.cells[4][0].fg, original.implicit_blank.fg);
        assert_eq!(input.cells[4][0].bg, original.implicit_blank.bg);
        assert_eq!(input.chrome_rasters.len(), 4);
        for raster in &input.chrome_rasters {
            let want: &[BandIcon] = if raster.row < 8 {
                &[BandIcon::Pause, BandIcon::Alert, BandIcon::ToBar]
            } else {
                &[BandIcon::Lock, BandIcon::Hourglass, BandIcon::Drive]
            };
            assert_eq!(
                raster.icons.iter().map(|i| i.icon).collect::<Vec<_>>(),
                want
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
        assert!(!input.cells[8][0].bold);
        assert!(input.cells[10][0].bold);
        let painted = input.clone();
        paint(Some(Scene::BandIcons), &mut input);
        assert_eq!(input, painted, "repainting does not duplicate chrome marks");
    }
}
