// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
