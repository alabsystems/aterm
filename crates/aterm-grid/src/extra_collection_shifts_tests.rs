// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::collections::HashMap;

use super::*;

const ROWS: u16 = 24;
const COLS: u16 = 24;
const RING_COLS: u16 = 80;

#[derive(Clone, Copy, Debug)]
enum Shift {
    RowsUp {
        n: u16,
    },
    RegionUp {
        top: u16,
        bottom: u16,
        n: u16,
    },
    RegionDown {
        top: u16,
        bottom: u16,
        n: u16,
    },
    RectUp {
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    },
    RectDown {
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    },
    ColsRight {
        row: u16,
        start: u16,
        n: u16,
        max: u16,
    },
    ColsLeft {
        row: u16,
        start: u16,
        n: u16,
        max: u16,
    },
}

impl Shift {
    fn apply(self, extras: &mut CellExtras) {
        match self {
            Self::RowsUp { n } => extras.shift_rows_up_by(n),
            Self::RegionUp { top, bottom, n } => extras.shift_region_up_by(top, bottom, n),
            Self::RegionDown { top, bottom, n } => extras.shift_region_down_by(top, bottom, n),
            Self::RectUp {
                top,
                bottom,
                left,
                right,
                n,
            } => {
                extras.shift_rect_up_by(top, bottom, left, right, n);
            }
            Self::RectDown {
                top,
                bottom,
                left,
                right,
                n,
            } => {
                extras.shift_rect_down_by(top, bottom, left, right, n);
            }
            Self::ColsRight { row, start, n, max } => extras.shift_cols_right(row, start, n, max),
            Self::ColsLeft { row, start, n, max } => extras.shift_cols_left(row, start, n, max),
        }
    }

    /// The documented contract: where the entry at `(row, col)` lands, or
    /// `None` when the shift drops it.
    fn model(self, row: u16, col: u16) -> Option<(u16, u16)> {
        let (r, c) = (u32::from(row), u32::from(col));
        let up = |top: u16, bottom: u16, left: u16, right: u16, n: u16| {
            if n == 0 || row < top || row > bottom || col < left || col > right {
                Some((row, col))
            } else if r >= u32::from(top) + u32::from(n) {
                Some((row - n, col))
            } else {
                None
            }
        };
        let down = |top: u16, bottom: u16, left: u16, right: u16, n: u16| {
            if n == 0 || row < top || row > bottom || col < left || col > right {
                Some((row, col))
            } else if r + u32::from(n) <= u32::from(bottom) {
                Some((row + n, col))
            } else {
                None
            }
        };
        match self {
            Self::RowsUp { n } => up(0, u16::MAX, 0, u16::MAX, n),
            Self::RegionUp { top, bottom, n } => up(top, bottom, 0, u16::MAX, n),
            Self::RegionDown { top, bottom, n } => down(top, bottom, 0, u16::MAX, n),
            Self::RectUp {
                top,
                bottom,
                left,
                right,
                n,
            } => up(top, bottom, left, right, n),
            Self::RectDown {
                top,
                bottom,
                left,
                right,
                n,
            } => down(top, bottom, left, right, n),
            Self::ColsRight {
                row: at,
                start,
                n,
                max,
            } => {
                if row != at || n == 0 || col < start || col >= max {
                    Some((row, col))
                } else if c + u32::from(n) < u32::from(max) {
                    Some((row, col + n))
                } else {
                    None
                }
            }
            Self::ColsLeft {
                row: at,
                start,
                n,
                max,
            } => {
                if row != at || n == 0 || col < start || col >= max {
                    Some((row, col))
                } else if c >= u32::from(start) + u32::from(n) {
                    Some((row, col - n))
                } else {
                    None
                }
            }
        }
    }
}

#[test]
fn pending_images_survive_rekeying_before_the_next_mutable_access() {
    use crate::{ImageData, ImageFormat, ImageRef, ImageScaling};
    use Shift::*;
    use std::sync::Arc;

    let image = Arc::new(ImageData {
        bytes: vec![7; 4],
        format: ImageFormat::Png,
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    });
    let shifts = [
        RowsUp { n: 1 },
        RegionUp {
            top: 0,
            bottom: 3,
            n: 1,
        },
        RegionDown {
            top: 0,
            bottom: 3,
            n: 1,
        },
        RectUp {
            top: 0,
            bottom: 3,
            left: 0,
            right: 7,
            n: 1,
        },
        RectDown {
            top: 0,
            bottom: 3,
            left: 0,
            right: 7,
            n: 1,
        },
        ColsRight {
            row: 1,
            start: 0,
            n: 1,
            max: 8,
        },
        ColsLeft {
            row: 1,
            start: 0,
            n: 1,
            max: 8,
        },
    ];
    for offset in [0, 1, 128] {
        for shift in shifts {
            let mut extras = CellExtras::new();
            extras
                .get_or_create(crate::CellCoord::new(5, 0))
                .add_combining('\u{0301}');
            extras.shift_rows_up_by(offset);
            extras
                .get_or_create(crate::CellCoord::new(1, 2))
                .set_image(Some(ImageRef {
                    image: image.clone(),
                    cell_row: 0,
                    cell_col: 0,
                    kitty: None,
                }));
            extras.modify(crate::CellCoord::new(3, 7), |extra| {
                extra.set_fg_rgb(Some([1, 2, 3]))
            });
            shift.apply(&mut extras);
            extras.compact();
            let actual: Vec<_> = extras
                .iter()
                .filter_map(|(coord, extra)| extra.image().map(|_| (coord.row, coord.col)))
                .collect();
            let expected: Vec<_> = shift.model(1, 2).into_iter().collect();
            assert_eq!(actual, expected, "{shift:?}, offset={offset}");
            assert!(
                extras.any_image(),
                "rekeyed pending image: {shift:?}, offset={offset}"
            );
            extras
                .fresh(crate::CellCoord::new(3, 7))
                .set_fg_rgb(Some([1, 2, 3]));
            assert!(
                extras.any_image(),
                "another mutable entry cannot hide its predecessor"
            );
            extras.clear();
            assert!(!extras.any_image());
        }
    }
}

fn fg(row: u16, col: u16) -> [u8; 3] {
    [row as u8, col as u8, 1]
}

fn bg(row: u16, col: u16) -> [u8; 3] {
    [row as u8, col as u8, 2]
}

fn complex(row: u16, col: u16) -> char {
    char::from_u32(0x1F000 + u32::from(row) * 64 + u32::from(col)).expect("valid char")
}

/// Verifies that every shift moves entries in the HashMap, RGB ring, and complex-char ring according to `Shift::model`.
#[test]
fn every_shift_moves_every_store_by_the_model() {
    use Shift::*;
    let scenarios: &[(&str, &[Shift])] = &[
        (
            "full-screen SU",
            &[RegionUp {
                top: 0,
                bottom: 23,
                n: 3,
            }],
        ),
        (
            "SU in margins",
            &[RegionUp {
                top: 5,
                bottom: 15,
                n: 1,
            }],
        ),
        (
            "SU leaves rows above (bug A)",
            &[RegionUp {
                top: 12,
                bottom: 17,
                n: 4,
            }],
        ),
        (
            "SU single-row region",
            &[RegionUp {
                top: 5,
                bottom: 5,
                n: 1,
            }],
        ),
        (
            "SU n == region",
            &[RegionUp {
                top: 2,
                bottom: 5,
                n: 4,
            }],
        ),
        (
            "SU n > region",
            &[RegionUp {
                top: 5,
                bottom: 6,
                n: 9,
            }],
        ),
        (
            "SU n == 0",
            &[RegionUp {
                top: 1,
                bottom: 3,
                n: 0,
            }],
        ),
        (
            "SD in margins",
            &[RegionDown {
                top: 5,
                bottom: 15,
                n: 1,
            }],
        ),
        (
            "SD drops bottom",
            &[RegionDown {
                top: 0,
                bottom: 9,
                n: 2,
            }],
        ),
        (
            "SD n == region",
            &[RegionDown {
                top: 0,
                bottom: 2,
                n: 3,
            }],
        ),
        (
            "SD n > region",
            &[RegionDown {
                top: 1,
                bottom: 3,
                n: 5,
            }],
        ),
        (
            "SD n == 0",
            &[RegionDown {
                top: 1,
                bottom: 3,
                n: 0,
            }],
        ),
        (
            "SD single-row region",
            &[RegionDown {
                top: 5,
                bottom: 5,
                n: 1,
            }],
        ),
        (
            "rect up",
            &[RectUp {
                top: 2,
                bottom: 8,
                left: 5,
                right: 15,
                n: 2,
            }],
        ),
        (
            "rect up past grid",
            &[RectUp {
                top: 2,
                bottom: 8,
                left: 10,
                right: 30,
                n: 1,
            }],
        ),
        (
            "rect up n == 0",
            &[RectUp {
                top: 0,
                bottom: 9,
                left: 5,
                right: 15,
                n: 0,
            }],
        ),
        (
            "rect down",
            &[RectDown {
                top: 2,
                bottom: 8,
                left: 5,
                right: 15,
                n: 2,
            }],
        ),
        (
            "rect down past grid",
            &[RectDown {
                top: 2,
                bottom: 8,
                left: 10,
                right: 30,
                n: 1,
            }],
        ),
        (
            "ICH",
            &[ColsRight {
                row: 5,
                start: 10,
                n: 3,
                max: 24,
            }],
        ),
        (
            "ICH from col 0",
            &[ColsRight {
                row: 0,
                start: 0,
                n: 5,
                max: 80,
            }],
        ),
        (
            "ICH at the margin",
            &[ColsRight {
                row: 3,
                start: 23,
                n: 1,
                max: 24,
            }],
        ),
        (
            "ICH inside DECLRMM",
            &[ColsRight {
                row: 0,
                start: 5,
                n: 3,
                max: 12,
            }],
        ),
        (
            "ICH n == 0",
            &[ColsRight {
                row: 0,
                start: 3,
                n: 0,
                max: 24,
            }],
        ),
        (
            "DCH",
            &[ColsLeft {
                row: 3,
                start: 5,
                n: 3,
                max: 24,
            }],
        ),
        (
            "DCH whole row",
            &[ColsLeft {
                row: 0,
                start: 0,
                n: 24,
                max: 24,
            }],
        ),
        (
            "DCH inside DECLRMM",
            &[ColsLeft {
                row: 0,
                start: 3,
                n: 2,
                max: 10,
            }],
        ),
        (
            "DCH n == 0",
            &[ColsLeft {
                row: 0,
                start: 3,
                n: 0,
                max: 24,
            }],
        ),
        (
            "SU after a full-screen offset",
            &[
                RowsUp { n: 2 },
                RegionUp {
                    top: 0,
                    bottom: 23,
                    n: 1,
                },
            ],
        ),
        (
            "ICH after a full-screen offset",
            &[
                RowsUp { n: 3 },
                ColsRight {
                    row: 4,
                    start: 2,
                    n: 2,
                    max: 24,
                },
            ],
        ),
    ];
    for (name, shifts) in scenarios {
        for dense in [true, false] {
            let cells: Vec<(u16, u16)> = (0..ROWS)
                .flat_map(|r| (0..COLS).map(move |c| (r, c)))
                .filter(|&(r, c)| dense || (r * COLS + c).is_multiple_of(7))
                .collect();
            let mut extras = CellExtras::new();
            let mut want = HashMap::new();
            for &(r, c) in &cells {
                let mut extra = CellExtra::default();
                extra.set_fg_rgb(Some(fg(r, c)));
                extras.set(CellCoord::new(r, c), extra);
                extras.set_rgb_ring_range(r, c, c + 1, None, Some(bg(r, c)), ROWS, RING_COLS);
                extras.set_complex_char_ring(r, c, complex(r, c), ROWS, RING_COLS);
                let dest = shifts
                    .iter()
                    .try_fold((r, c), |(row, col), s| s.model(row, col));
                if let Some(dest) = dest {
                    want.insert(dest, (r, c));
                }
            }
            for shift in *shifts {
                shift.apply(&mut extras);
            }

            let got: HashMap<(u16, u16), [u8; 3]> = extras
                .iter()
                .map(|(coord, e)| ((coord.row, coord.col), e.fg_rgb().expect("fg")))
                .collect();
            let want_map: HashMap<(u16, u16), [u8; 3]> =
                want.iter().map(|(&at, &(r, c))| (at, fg(r, c))).collect();
            assert_eq!(got, want_map, "{name} (dense: {dense}): map");
            assert_eq!(extras.len(), want.len(), "{name} (dense: {dense}): len");
            for row in 0..ROWS {
                for col in 0..COLS + 8 {
                    let origin = want.get(&(row, col)).copied();
                    assert_eq!(
                        extras.bg_rgb_for(row, col),
                        origin.map(|(r, c)| bg(r, c)),
                        "{name} (dense: {dense}): rgb ring at ({row}, {col})"
                    );
                    assert_eq!(
                        extras.complex_codepoint_for(row, col),
                        origin.map(|(r, c)| complex(r, c)),
                        "{name} (dense: {dense}): complex ring at ({row}, {col})"
                    );
                }
            }
        }
    }
}
