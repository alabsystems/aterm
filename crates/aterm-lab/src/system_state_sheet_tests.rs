// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn native_sheet_keeps_a_one_pixel_slit() {
    let mut pixels = vec![[255, 255, 255]; 900 * 2];
    pixels[600] = [0, 0, 0];
    pixels[900 + 600] = [0, 0, 0];
    let frame = Image::from_pixels(900, 2, pixels);
    let crop = frame
        .crop(crate::trail_contrast::PxBox {
            x0: 100,
            y0: 0,
            x1: 800,
            y1: 2,
        })
        .unwrap();
    let tiles = [Tile {
        frame: crop,
        caption: "frame 1".into(),
        verdicts: vec![],
    }];
    let bytes = native_png("native", "no visual verdict", &tiles).unwrap();
    let sheet = Image::decode(Path::new("sheet.png"), &bytes).unwrap();
    let head = GAP + 7 * TITLE_SCALE + 8 + LINE_H + GAP;
    assert_eq!(sheet.at(GAP + 500, head), [0, 0, 0]);
    assert_eq!(sheet.at(GAP + 500, head + 1), [0, 0, 0]);
    assert_eq!(sheet.at(GAP + 499, head), [255, 255, 255]);
    assert_eq!(sheet.at(GAP + 501, head), [255, 255, 255]);
}

#[test]
fn a_sheet_shrinks_frames_to_tiles_and_letters_them() {
    let frame = |w, h, c| Image::from_pixels(w, h, vec![c; w * h]);
    let tiles = vec![
        Tile {
            frame: frame(900, 644, [10, 20, 30]),
            caption: "running".into(),
            verdicts: vec![("text 4.6".into(), true), ("card 1.1".into(), false)],
        },
        Tile {
            frame: frame(900, 644, [200, 200, 200]),
            caption: "set-up".into(),
            verdicts: vec![],
        },
    ];
    assert_eq!(shrink(900), 2);
    assert_eq!(shrink(1800), 4);
    assert_eq!(shrink(368), 1);
    let dir = aterm_tempfile::Builder::new()
        .prefix("sheet")
        .suffix(".noindex")
        .tempdir()
        .expect("a scratch folder");
    let path = dir.path().join("sheet.png");
    write(&path, "system state", "settled", &tiles).unwrap();
    let sheet = Image::read(&path).unwrap();
    assert_eq!(
        sheet.width,
        GAP + 2 * (450 + GAP),
        "two 450-pixel tiles in a row"
    );
    // The first tile's mean colour lands at its top left, under the head.
    let head = GAP + 7 * TITLE_SCALE + 8 + LINE_H + GAP;
    assert_eq!(sheet.at(GAP, head), [10, 20, 30]);
    assert_eq!(sheet.at(GAP + 450 + GAP, head), [200, 200, 200]);
    // A failed verdict is lettered in red somewhere under the first tile.
    let under = head + 322 + 4 + LINE_H;
    let red = (under..under + 14)
        .flat_map(|y| (GAP..GAP + 450).map(move |x| (x, y)))
        .any(|(x, y)| sheet.at(x, y) == FAIL);
    assert!(red);
    assert!(write(&path, "empty", "", &[]).is_err());
    // One narrow tile: the sheet widens for its title, and the verdicts wrap to the tile.
    let words: Vec<(String, bool)> = [
        "text 6.05",
        "glyph 6.14",
        "card 1.15",
        "clip 0",
        "rhythm 76",
    ]
    .iter()
    .map(|w| ((*w).to_string(), true))
    .collect();
    let lines = verdict_lines(&words, 346);
    assert!(lines.len() > 1, "{lines:?}");
    for line in &lines {
        let chars: usize = line.iter().map(|&i| words[i].0.len() + 1).sum::<usize>() - 1;
        assert!(text_w(chars, TEXT_SCALE) <= 346, "{line:?}");
    }
    assert_eq!(
        lines.concat(),
        (0..words.len()).collect::<Vec<_>>(),
        "every word, in order"
    );
    let narrow = [Tile {
        frame: frame(346, 644, [10, 20, 30]),
        caption: "running".into(),
        verdicts: words,
    }];
    let title = "system state · hc-dark · compact 46 columns at 1x";
    write(&path, title, "settled", &narrow).unwrap();
    let sheet = Image::read(&path).unwrap();
    assert_eq!(
        sheet.width,
        2 * GAP + text_w(title.chars().count(), TITLE_SCALE)
    );
    assert_eq!(
        sheet.height,
        head + 644 + 4 + (1 + lines.len()) * LINE_H + GAP,
        "a line per verdict row under the caption"
    );
}
