// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::BandIcon;
use crate::terminal::Terminal;

/// [`BandIcon::ALL`] lists every icon at its own index, so [`BandIcon::from_index`] (a glyph cache key's way back) gives each one back and nothing past the set.
#[test]
fn every_icon_round_trips_through_its_index() {
    for (i, icon) in BandIcon::ALL.into_iter().enumerate() {
        let i = u32::try_from(i).expect("a small set");
        assert_eq!(icon.index(), i, "{icon:?} sits at its index in ALL");
        assert_eq!(BandIcon::from_index(i), Some(icon));
    }
    let past = u32::try_from(BandIcon::ALL.len()).expect("a small set");
    assert_eq!(BandIcon::from_index(past), None);
    assert_eq!(BandIcon::from_index(u32::MAX), None);
}

/// Each icon is exactly one of a band glyph's ([`BandIcon::for_char`]), a glass mark ([`BandIcon::glass_mark`]) or a need pictogram, which no character maps to; a pictogram's stand-in takes one cell on a grid, as every other icon's character does.
#[test]
fn each_icon_is_a_glyph_a_mark_or_a_pictogram() {
    for icon in BandIcon::ALL {
        let ch = icon.ch();
        let kinds = [
            BandIcon::for_char(ch) == Some(icon),
            BandIcon::glass_mark(ch) == Some(icon),
            icon.is_pictogram(),
        ];
        assert_eq!(
            kinds.iter().filter(|&&k| k).count(),
            1,
            "{icon:?} ({ch:?}): glyph, mark, pictogram = {kinds:?}"
        );
        if icon.is_pictogram() {
            assert_eq!(BandIcon::for_char(ch), None, "{icon:?}: no band glyph");
            assert_eq!(BandIcon::glass_mark(ch), None, "{icon:?}: no glass mark");
        }
        let mut term = Terminal::new(1, 4);
        let mut bytes = [0u8; 4];
        term.process(ch.encode_utf8(&mut bytes).as_bytes());
        term.process(b"x");
        let row = &term.cell_frame(1, 4).cells[0];
        assert_eq!(row[0].ch, ch, "{icon:?}: the grid keeps its character");
        assert_eq!(row[1].ch, 'x', "{icon:?} ({ch:?}) takes one cell");
    }
}
