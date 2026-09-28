// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BAND'S COLOURS, TESTED WHERE THEY ARE DECIDED (design ruling 328):
//! the message band's colour tests, moved from aterm-gui's `message_band.rs`
//! onto [`ink::paint_band`], which has decided every colour of the band since
//! ruling 324 — which ground a meter cell and a chip keep, which ink a word
//! wears on the fill, what a High Contrast capsule looks like, whether a
//! label clears AA. Each keeps its host name where it moved whole; where only
//! a test's colour half moved, the host keeps its structure half under the
//! old name. The inputs are the host's own: its default theme, its builtin
//! schemes and its four stock High Contrast palettes, transcribed below and
//! held equal to the host's by [`fixtures_are_the_hosts`] (the host computes
//! the same digest from `aterm_render::Theme::default()`,
//! `aterm_types::scheme` and `chrome_band::hc_fixtures::STOCK`). The palette
//! is derived on the theme blend ([`BarBase::Blend`]), the host's base on
//! every platform but Linux, whose CSD tones the host's own tests keep. The
//! host's `RenderCell` write copies each resolved cell — the host holds that
//! with `paint_rows_on_writes_the_engines_resolved_cells`.

use crate::ink::{
    self, BandInks, BarBase, ForcedPalette, InkedCell, MeterInks, RowRaster, ThemeInks, WORD_AA,
    chip_inks, contrast, crisp_on_fill, fill_anchor, fill_ink, floor_word, keep_side, mix3,
    words_anchor,
};
use crate::paint::{Geometry, Hover, HoverTarget, MeterSpan, band_fp};
use crate::palette::{luminance, oklch};
use crate::{
    ActionIndex, BandMotion, CapsuleRole, GLYPH_COL, Hold, Instant, Intent, Links, Look, MARGIN,
    Message, MessageCenter, MessageLog, Meter, Presentation, RowLayout, Severity, Tone, WallStamp,
    tags,
};

// ---- The host's inputs -----------------------------------------------------

/// `aterm_render::Theme::default()`: the default dark theme.
const DEFAULT: ThemeInks = ThemeInks {
    bg: [0x11, 0x13, 0x18],
    fg: [0xd0, 0xd0, 0xd0],
    cursor: [0x50, 0xfa, 0x7b],
};

/// The light theme `the_comet_never_flips_a_words_ink` paints on (GitHub
/// Light's chrome).
const LIGHT: ThemeInks = ThemeInks {
    bg: [0xff, 0xff, 0xff],
    fg: [0x1f, 0x23, 0x28],
    cursor: [0x09, 0x69, 0xda],
};

/// Every builtin scheme as a theme, `Default` first — the host's
/// `aterm_types::scheme::builtin_names()` through `to_theme_parts()` (the
/// cursor falls back to the foreground), with no ANSI palette.
const BUILTIN: [(&str, ThemeInks); 12] = [
    (
        "Default",
        ThemeInks {
            bg: [0x11, 0x13, 0x18],
            fg: [0xd0, 0xd0, 0xd0],
            cursor: [0x50, 0xfa, 0x7b],
        },
    ),
    (
        "Dracula",
        ThemeInks {
            bg: [0x28, 0x2a, 0x36],
            fg: [0xf8, 0xf8, 0xf2],
            cursor: [0xf8, 0xf8, 0xf2],
        },
    ),
    (
        "Nord",
        ThemeInks {
            bg: [0x2e, 0x34, 0x40],
            fg: [0xd8, 0xde, 0xe9],
            cursor: [0xec, 0xef, 0xf4],
        },
    ),
    (
        "Tokyo Night",
        ThemeInks {
            bg: [0x1a, 0x1b, 0x26],
            fg: [0xc0, 0xca, 0xf5],
            cursor: [0x7a, 0xa2, 0xf7],
        },
    ),
    (
        "Catppuccin Mocha",
        ThemeInks {
            bg: [0x1e, 0x1e, 0x2e],
            fg: [0xcd, 0xd6, 0xf4],
            cursor: [0xf5, 0xe0, 0xdc],
        },
    ),
    (
        "Gruvbox Dark",
        ThemeInks {
            bg: [0x28, 0x28, 0x28],
            fg: [0xeb, 0xdb, 0xb2],
            cursor: [0xeb, 0xdb, 0xb2],
        },
    ),
    (
        "Solarized Dark",
        ThemeInks {
            bg: [0x00, 0x2b, 0x36],
            fg: [0x83, 0x94, 0x96],
            cursor: [0x83, 0x94, 0x96],
        },
    ),
    (
        "One Dark",
        ThemeInks {
            bg: [0x21, 0x25, 0x2b],
            fg: [0xab, 0xb2, 0xbf],
            cursor: [0xab, 0xb2, 0xbf],
        },
    ),
    (
        "Solarized Light",
        ThemeInks {
            bg: [0xfd, 0xf6, 0xe3],
            fg: [0x65, 0x7b, 0x83],
            cursor: [0x65, 0x7b, 0x83],
        },
    ),
    (
        "Gruvbox Light",
        ThemeInks {
            bg: [0xfb, 0xf1, 0xc7],
            fg: [0x3c, 0x38, 0x36],
            cursor: [0x3c, 0x38, 0x36],
        },
    ),
    (
        "Catppuccin Latte",
        ThemeInks {
            bg: [0xef, 0xf1, 0xf5],
            fg: [0x4c, 0x4f, 0x69],
            cursor: [0xdc, 0x8a, 0x78],
        },
    ),
    (
        "GitHub Light",
        ThemeInks {
            bg: [0xff, 0xff, 0xff],
            fg: [0x1f, 0x23, 0x28],
            cursor: [0x09, 0x69, 0xda],
        },
    ),
];

/// The four stock Windows High Contrast schemes: the host's
/// `chrome_band::hc_fixtures::STOCK`.
const STOCK: [(&str, ForcedPalette); 4] = [
    (
        "Aquatic",
        ForcedPalette {
            window: [0x00, 0x00, 0x00],
            window_text: [0xFF, 0xFF, 0xFF],
            highlight: [0x37, 0x00, 0x6E],
            highlight_text: [0xFF, 0xFF, 0xFF],
            btn_face: [0x00, 0x00, 0x00],
        },
    ),
    (
        "Desert",
        ForcedPalette {
            window: [0xFF, 0xFF, 0xFF],
            window_text: [0x00, 0x00, 0x00],
            highlight: [0x37, 0x00, 0x6E],
            highlight_text: [0xFF, 0xFF, 0xFF],
            btn_face: [0xFF, 0xFF, 0xFF],
        },
    ),
    (
        "Dusk",
        ForcedPalette {
            window: [0x2D, 0x32, 0x36],
            window_text: [0xFF, 0xFF, 0xFF],
            highlight: [0x1A, 0xEB, 0xFF],
            highlight_text: [0x00, 0x00, 0x00],
            btn_face: [0x2D, 0x32, 0x36],
        },
    ),
    (
        "Night sky",
        ForcedPalette {
            window: [0x00, 0x00, 0x00],
            window_text: [0xFF, 0xFF, 0xFF],
            highlight: [0x1A, 0xEB, 0xFF],
            highlight_text: [0x00, 0x00, 0x00],
            btn_face: [0x00, 0x00, 0x00],
        },
    ),
];

/// The digest the host computes over its own inputs
/// (`message_band::paint_parity_tests::the_engines_band_tests_read_the_hosts_themes`).
const HOST_INPUTS_DIGEST: u64 = 0x973b_e822_c486_d59b;

/// FNV-1a over the fixtures' `Debug` text: a digest with no hasher seed, so
/// the engine and the host compute the same number from the same values.
fn digest(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// THE INPUTS ARE THE HOST'S: the default and light themes, the twelve
/// builtin schemes and the four stock High Contrast palettes digest to the
/// number the host computes from its own sources.
#[test]
fn fixtures_are_the_hosts() {
    let text = format!("{:?}", (DEFAULT, LIGHT, &BUILTIN[..], &STOCK[..]));
    assert_eq!(digest(&text), HOST_INPUTS_DIGEST, "{text}");
}

// ---- The host's painter, on the engine's cells -----------------------------

/// The host's `chrome_band::band_colors(theme)` off Linux: the theme's inks
/// derived on the blend, with no ANSI palette.
fn inks(theme: ThemeInks) -> BandInks {
    BandInks::derive(theme, None, BarBase::Blend)
}

fn t0() -> Instant {
    Instant::now()
}

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1 }
}

/// The host's band measure: one `char` per cell.
fn cell_width(s: &str) -> usize {
    crate::text::char_width(s)
}

/// The host's own presentation: every row ending in its link.
fn present(center: &MessageCenter, cols: usize) -> Presentation {
    center.presentation(cols, &cell_width, None, Links::Painted)
}

/// Every row's resolved cells at `motion` on the unit grid (the host's
/// `paint_rows`); `forced` is the High Contrast latch.
fn paint_rows(
    p: &Presentation,
    c: &BandInks,
    forced: bool,
    hover: Option<Hover>,
    motion: &BandMotion,
) -> Vec<Vec<InkedCell>> {
    ink::paint_band(p, hover, Geometry::cells_only(p.cols), motion, forced, c)
        .into_iter()
        .map(|r| r.cells)
        .collect()
}

/// The band at `center`'s STILL frame over `p` on the unit grid.
fn paint_still(
    p: &Presentation,
    c: &BandInks,
    forced: bool,
    hover: Option<Hover>,
    center: &MessageCenter,
) -> Vec<Vec<InkedCell>> {
    paint_rows(p, c, forced, hover, &center.motion(p, t0(), Look::STILL))
}

/// One frame of `center`'s motion over `p` at `at` in `look`, on the unit
/// grid.
fn frame_at(
    center: &MessageCenter,
    p: &Presentation,
    at: Instant,
    look: Look,
    c: &BandInks,
) -> (Vec<Vec<InkedCell>>, BandMotion) {
    let m = center.motion(p, at, look);
    (paint_rows(p, c, false, None, &m), m)
}

/// [`frame_at`] with each row's pixel raster (on the unit grid).
fn frame_rasters(
    center: &MessageCenter,
    p: &Presentation,
    at: Instant,
    look: Look,
    c: &BandInks,
) -> (Vec<Vec<InkedCell>>, Vec<Option<RowRaster>>) {
    let m = center.motion(p, at, look);
    ink::paint_band(p, None, Geometry::cells_only(p.cols), &m, false, c)
        .into_iter()
        .map(|r| (r.cells, r.raster))
        .unzip()
}

/// The inks row `l`'s surface resolves against (the host's `row_inks`).
fn row_inks(c: &BandInks, l: &RowLayout, rail: bool, hc: bool) -> (MeterInks, Option<[u8; 3]>) {
    let alarm = matches!(l.severity, Severity::Warn | Severity::Error);
    let fill = if hc || !alarm { c.meter } else { c.warn };
    ink::inks_for(c, fill, l.track.is_some(), rail, hc)
}

/// A center with the toolchain meter row and a downloading update row
/// committed (the host's `two_rows`).
fn two_rows() -> MessageCenter {
    let now = t0();
    let mut center = MessageCenter::new(MessageLog::empty(), now);
    center.post(
        Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
            .line("trust — extracting 120 MB / 900 MB")
            .meter(Meter {
                fill_permille: Some(427),
                stats: "3 of 10 · 512 MB / 1.2 GB".into(),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: crate::STALE_TAILED,
            })
            .action(Intent::OpenSettings {
                route: "/packages".into(),
            }),
        stamp(),
        now,
    );
    center.post(
        Message::new(tags::UPDATE, Severity::Info, "aterm update v0.48.0")
            .line("downloading…")
            .meter(Meter {
                fill_permille: Some(608),
                stats: "45 MB / 74 MB".into(),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            }),
        stamp(),
        now,
    );
    assert_eq!(center.commit_rows(now, 3), Some(2));
    center
}

/// A busy row alone, committed at `now` (the host's `busy_center`).
fn busy_center(now: Instant) -> MessageCenter {
    let mut center = MessageCenter::new(MessageLog::empty(), now);
    center.post(
        Message::new(tags::UPDATE, Severity::Info, "Checking aterm v0.92.0")
            .meter(Meter::busy(""))
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            }),
        stamp(),
        now,
    );
    assert_eq!(center.commit_rows(now, 3), Some(1));
    center
}

/// The `(ink, ground)` pairs under column `x`'s glyph as the renderer draws
/// it (ruling 242; the host's `word_grounds`, on the engine's cells).
fn word_grounds(
    row: &[InkedCell],
    raster: Option<&RowRaster>,
    geom: Geometry,
    x: usize,
) -> Vec<([u8; 3], [u8; 3])> {
    let cell = &row[x];
    let owned = raster.is_some_and(|r| {
        r.own
            .iter()
            .any(|&(a, b)| (usize::from(a)..usize::from(b)).contains(&x))
            || r.rings
                .iter()
                .any(|&(a, b, ..)| (usize::from(a)..usize::from(b)).contains(&x))
    });
    match raster.filter(|r| !r.ground.is_empty() && !owned) {
        Some(r) => {
            let x0 = geom.cells_x + x * geom.cell_w;
            (x0..x0 + geom.cell_w)
                .filter(|&px| {
                    r.edge != Some(px as u32)
                        && !r.line.is_some_and(|(a, b)| (a..b).contains(&(px as u32)))
                        && px < r.ground.len()
                })
                .map(|px| {
                    let ink = match r.split {
                        Some((col, sx, ink, _)) if usize::from(col) == x && (px as u32) < sx => ink,
                        _ => cell.fg,
                    };
                    (ink, r.ground[px])
                })
                .collect()
        }
        None => vec![(cell.fg, cell.bg)],
    }
}

// ---- The moved tests --------------------------------------------------------

/// THE METER IS THE ROW, IN COLOUR (the colour half of the host's
/// `paint_rows_are_exactly_cols_wide_and_carry_the_words`): the toolchain
/// row's 42.7 % lights the cells left of its edge in the full accent, the
/// cells right of it are the track, and the ONE cell under the edge takes its
/// coverage — a whole-cell background tone, never meter ink; the glyph and
/// the title sit on the fill in the row's CRISP ink (ruling 222), floored
/// against it.
#[test]
fn the_meter_row_is_its_fill_and_track_and_its_words_the_crisp_ink() {
    let center = two_rows();
    let p = present(&center, 140);
    let c = inks(DEFAULT);
    let rows = paint_still(&p, &c, false, None, &center);
    let (mcol, w, fill) = p.rows[0].meter.expect("a meter at 140 cols");
    assert_eq!((mcol, w, fill), (0, 140, 427), "the meter is the row");
    let edge = 140 * 427 / 1000;
    assert!(rows[0][..edge].iter().all(|cell| cell.bg == c.accent));
    assert!(
        rows[0][edge + 1..]
            .iter()
            .all(|cell| cell.bg == c.meter_track || cell.bg == c.bar_bg),
        "past the edge: the track (a Secondary chip wears the band)"
    );
    let inks = MeterInks::of(&c, c.accent);
    let partial = rows[0][edge].bg;
    assert!(
        (1..255u8).any(|f| inks.rgb(
            Tone {
                fill: f,
                lift: 0,
                warn: 0
            }
            .into()
        ) == partial),
        "the edge cell is a mix of track and fill: {partial:?}"
    );
    // Both sit on the meter's fill (ruling 55): each wears the row's CRISP
    // ink (ruling 222) — the band's own ink that reads best on the fill —
    // floored against it.
    let fill = rows[0][MARGIN].bg;
    assert_eq!(fill, c.accent, "the glyph sits on the fill");
    let crisp = floor_word(fill_ink(&c, c.accent), fill, fill_anchor(c.accent));
    assert_eq!(rows[0][MARGIN].fg, crisp, "the glyph wears the crisp ink");
    assert_eq!(
        rows[0][p.rows[0].title.0].fg, crisp,
        "the title wears the crisp ink"
    );
}

/// EACH CHIP WEARS ITS ROLE ON A METERED ROW (the colour half of the host's
/// `capsules_are_hit_where_they_are_painted`), at 60, 80, 120 and 160
/// columns: `Packages` is a navigation, the quiet chip, which on a METERED
/// row draws no ground of its own (ruling 260) — it rides the meter like
/// `Details ›`, in the value ink floored on the track under it — and
/// `Details ›` rides it in the label ink. A Primary would wear the accent.
#[test]
fn each_chip_wears_its_roles_ground_and_ink_on_a_metered_row() {
    let center = two_rows();
    let c = inks(DEFAULT);
    for cols in [60, 80, 120, 160] {
        let p = present(&center, cols);
        let rows = paint_still(&p, &c, false, None, &center);
        let layout = &p.rows[0];
        assert!(
            !layout.capsules.is_empty(),
            "cols {cols}: Packages at least"
        );
        for cap in &layout.capsules {
            match cap.role {
                CapsuleRole::Primary => {
                    assert_eq!(rows[0][cap.col].bg, c.accent);
                    assert_eq!(rows[0][cap.col + 1].fg, c.bar_bg);
                    assert!(rows[0][cap.col + 1].bold);
                }
                CapsuleRole::Secondary => {
                    assert_eq!(rows[0][cap.col].bg, c.meter_track);
                    assert_eq!(
                        rows[0][cap.col + 1].fg,
                        floor_word(c.value, c.meter_track, words_anchor(&c))
                    );
                    assert!(!rows[0][cap.col + 1].bold);
                }
                CapsuleRole::Details => {
                    assert_eq!(rows[0][cap.col].bg, c.meter_track);
                    assert_eq!(
                        rows[0][cap.col + 1].fg,
                        floor_word(c.label, c.meter_track, words_anchor(&c))
                    );
                }
            }
        }
    }
}

/// HIGH CONTRAST CAPSULES ARE ONE INK ON THE BAND (the colour half of the
/// host's `high_contrast_draws_capsules_as_brackets`): under each stock High
/// Contrast palette every capsule cell is WINDOWTEXT on the band with no
/// fill, the glyph on the metered row's fill is HIGHLIGHTTEXT (ruling 137),
/// and the hovered capsule is HIGHLIGHT.
#[test]
fn high_contrast_capsules_are_one_ink_on_the_band() {
    let center = two_rows();
    for (name, palette) in STOCK {
        let c = BandInks::forced(palette);
        let p = present(&center, 140);
        let rows = paint_still(&p, &c, true, None, &center);
        for cap in &p.rows[0].capsules {
            for cell in &rows[0][cap.col..cap.col + cap.width] {
                assert_eq!(cell.bg, c.bar_bg, "{name}: no fill");
                assert_eq!(cell.fg, c.value, "{name}: one ink");
            }
        }
        assert_eq!(
            rows[0][GLYPH_COL].fg, c.on_accent,
            "{name}: the glyph on the fill is HIGHLIGHTTEXT"
        );
        let first = &p.rows[0].capsules[0];
        let lit = paint_still(
            &p,
            &c,
            true,
            Some(Hover {
                row: 0,
                target: HoverTarget::Capsule(first.action),
            }),
            &center,
        );
        assert_eq!(lit[0][first.col].bg, palette.highlight, "{name}");
    }
}

/// THE COMET NEVER FLIPS A WORD'S INK (visual review of the merged band,
/// 2026-09-24): over one comet period, on a dark band and a light one,
/// every tone the comet draws leaves the row's words' anchor AA from it,
/// and every word — and the glyph cell's icon — stays on that anchor's side of the
/// ground under it, at least as far toward the anchor as it rests on the
/// track. The full accent under light ink flipped each letter it crossed
/// to black and back; the dark spinner split the entering comet in two.
#[test]
fn the_comet_never_flips_a_words_ink() {
    for theme in [DEFAULT, LIGHT] {
        let c = inks(theme);
        let anchor = words_anchor(&c);
        let toward = |ink: [u8; 3]| {
            if anchor == ink::WHITE {
                luminance(ink)
            } else {
                -luminance(ink)
            }
        };
        let now = t0();
        let center = busy_center(now);
        let cols = 110;
        let p = present(&center, cols);
        let (still, _) = frame_at(&center, &p, now, Look::STILL, &c);
        let steps = crate::COMET_PERIOD.as_millis() / crate::ANIM_FRAME.as_millis();
        let mut lit = 0;
        for k in 0..u32::try_from(steps).unwrap() {
            let t = now + crate::ANIM_FRAME * k;
            let (rows, _) = frame_at(&center, &p, t, Look::MOVING, &c);
            for (x, (cell, rest)) in rows[0].iter().zip(&still[0]).enumerate() {
                assert!(
                    contrast(anchor, cell.bg) >= WORD_AA - 1e-9,
                    "frame {k} col {x}: the comet left its words' side: {:?}",
                    cell.bg
                );
                if cell.bg != rest.bg {
                    lit += 1;
                }
                if cell.ch == ' ' {
                    continue;
                }
                assert!(
                    toward(cell.fg) > toward(cell.bg),
                    "frame {k} col {x} {:?}: the ink flipped side",
                    cell.ch
                );
                assert!(
                    contrast(cell.fg, cell.bg) >= WORD_AA - 1e-9,
                    "frame {k} col {x} {:?}",
                    cell.ch
                );
                if x != crate::GLYPH_COL {
                    assert!(
                        toward(cell.fg) >= toward(rest.fg) - 1e-9,
                        "frame {k} col {x} {:?}: the ink moved away from its anchor",
                        cell.ch
                    );
                }
            }
        }
        assert!(lit > 0, "the comet drew");
    }
}

/// THE TIME SLOT'S WORDS WEAR THEIR MEANING'S INK (review round 3,
/// 2026-09-24): a determinate row whose estimate is hidden shows its
/// elapsed CLOCK in the ETA slot in the label ink, a latched estimate
/// (`… left`) in the value ink, and its Complete echo says `done` in the
/// ink its ✓ wears — the accent's family, never warn (`stalled` and
/// `failed` keep warn). On the full-row meter every one of those inks is
/// floored against the cell under it (ruling 55's AA floor), so the echo's
/// `done` and ✓, both on the completing fill, wear the same floored ink.
#[test]
fn the_time_slot_words_wear_their_meanings_ink() {
    let c = inks(DEFAULT);
    let download = |done: u64| {
        Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
            .meter(Meter {
                fill_permille: None,
                stats: "31 MB / 74 MB".into(),
                amount: Some(crate::Amount {
                    series: crate::Amount::series_of("aterm 0.91.0"),
                    done,
                    total: 74_000_000,
                    unit: crate::Unit::Bytes,
                }),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: crate::STALE_UPDATE,
            })
            .key("update.progress")
    };
    let slot = |center: &MessageCenter, at: Instant| {
        let p = present(center, 120);
        let col = p.rows[0].eta.expect("the ETA slot at 120");
        let (rows, _) = frame_at(center, &p, at, Look::MOVING, &c);
        let words: String = rows[0][col..col + p.rows[0].eta_width()]
            .iter()
            .map(|cell| cell.ch)
            .collect();
        (
            words.trim_end().to_string(),
            rows[0][col].fg,
            rows[0][col].bg,
            rows[0][crate::GLYPH_COL].fg,
        )
    };
    let floored = |ink: [u8; 3], bg: [u8; 3]| {
        if bg == c.bar_bg {
            ink
        } else if bg == c.meter_track {
            floor_word(ink, bg, words_anchor(&c))
        } else {
            floor_word(fill_ink(&c, c.accent), bg, fill_anchor(c.accent))
        }
    };
    let now = t0();
    let mut center = MessageCenter::new(MessageLog::empty(), now);
    let id = center.post(download(10_000_000), stamp(), now).id;
    center.commit_rows(now, 3);
    let (words, _, _, _) = slot(&center, now + crate::Duration::from_millis(4200));
    assert_eq!(
        words, "",
        "a hidden estimate leaves the slot blank (ruling 241)"
    );
    // Fed steadily, the estimate latches: `… left` in the value ink.
    let mut t = now;
    for k in 1..=16u64 {
        t = now + crate::Duration::from_millis(500 * k);
        center.restate(
            id,
            crate::Restatement {
                meter: Some(download(10_000_000 + 2_000_000 * k).meter),
                ..crate::Restatement::default()
            },
            t,
        );
    }
    let (words, ink, bg, _) = slot(&center, t);
    assert!(words.ends_with(" left"), "latched: {words:?}");
    assert_eq!(ink, floored(c.value, bg));
    // Delivered: the ✓ and the finished words only — the slot says
    // nothing (ruling 244: no `100% done`).
    assert!(center.resolve(id, crate::Outcome::Ok, t));
    let (words, _, _, _) = slot(&center, t + crate::Duration::from_millis(100));
    assert_eq!(words, "");
}

/// THE BAND'S REPAINT KEY (FL-1; rulings 55, 140): exactly `0` with no row
/// committed, whatever the hover, the geometry or the motion term;
/// otherwise nonzero and moved by each of them — a hover change, a resize
/// that keeps the column count (the meter is mapped onto the WINDOW), and
/// a motion frame that draws something new — and the same inputs are the
/// same key. A band of held rows only has a motion fingerprint of exactly
/// 0 in both looks; a busy row's STILL frame does not move with time, and
/// its MOVING frames do.
#[test]
fn band_fp_is_zero_with_no_row_and_moves_only_with_what_it_draws() {
    let g = Geometry::cells_only(100);
    let wide = Geometry {
        win_w: 1000,
        cells_x: 20,
        cell_w: 9,
    };
    let hovers = [
        None,
        Some(Hover {
            row: 0,
            target: HoverTarget::Body,
        }),
        Some(Hover {
            row: 2,
            target: HoverTarget::Capsule(ActionIndex(1)),
        }),
    ];
    for hover in hovers {
        for geom in [g, wide] {
            for motion in [0u64, 0x1234] {
                assert_eq!(band_fp(0, hover, geom, motion), 0, "no row, no key");
            }
        }
    }
    let fp = 0x1234_5678_u64;
    let key = band_fp(fp, None, g, 0);
    assert_ne!(key, 0);
    assert_eq!(
        key,
        band_fp(fp, None, g, 0),
        "the same inputs, the same key"
    );
    assert_ne!(key, band_fp(fp, hovers[1], g, 0), "a hover moves it");
    assert_ne!(key, band_fp(fp, None, wide, 0), "the window moves it");
    assert_ne!(key, band_fp(fp, None, g, 0x1234), "a frame moves it");
    assert_ne!(band_fp(fp, None, g, 0x1234), band_fp(fp, None, g, 0x5678));
    // Held rows only: no motion term, in either look.
    let now = t0();
    let mut center = MessageCenter::new(MessageLog::empty(), now);
    center.post(
        Message::new(tags::CONFIG, Severity::Warn, "Font not found").line("Nope"),
        stamp(),
        now,
    );
    center.commit_rows(now, 3);
    let p = present(&center, 100);
    for look in [Look::MOVING, Look::STILL] {
        assert_eq!(center.motion(&p, now, look).fingerprint(), 0);
    }
    // A busy row: still, the frame holds; moving, it moves.
    let center = busy_center(now);
    let p = present(&center, 100);
    let at = |k: u32, look| {
        center
            .motion(&p, now + crate::ANIM_FRAME * k, look)
            .fingerprint()
    };
    assert_ne!(at(0, Look::STILL), 0, "the still track is drawn");
    assert_eq!(
        at(0, Look::STILL),
        at(90, Look::STILL),
        "still: no motion term moves"
    );
    assert_ne!(
        at(0, Look::MOVING),
        at(4, Look::MOVING),
        "moving: the frames move the key"
    );
}

/// EVERY CHIP LABEL CLEARS AA ON ITS OWN CELL (design ruling 155): on
/// every builtin scheme, a Primary, a Secondary and `Details ›` — resting
/// and lit — on the plain band, over a meter's track and over its fill,
/// and on a busy row, each label glyph clears 4.5:1 against the cell it
/// sits on. The Primary measured 3.69:1 on Solarized Light and 4.28:1 on
/// GitHub Light before; it keeps its polarity (the band's ink on a
/// deepened accent), so no label turned black on a blue chip.
#[test]
fn every_chip_label_clears_aa_on_every_builtin_theme() {
    let now = t0();
    let actions = |m: Message| {
        m.action(Intent::NewWindow).action(Intent::OpenSettings {
            route: "/packages".into(),
        })
    };
    let live = |meter: Meter| {
        actions(
            Message::new(tags::UPDATE, Severity::Info, "Work")
                .meter(meter)
                .hold(Hold::Live {
                    stale_after: crate::STALE_UPDATE,
                }),
        )
    };
    let filled = |p: u16| {
        live(Meter {
            fill_permille: Some(p),
            ..Meter::default()
        })
    };
    let rows = [
        (
            "band",
            actions(Message::new(tags::CONFIG, Severity::Warn, "Held")),
        ),
        ("track", filled(20)),
        ("fill", filled(1000)),
        ("busy", live(Meter::busy(""))),
    ];
    for (name, theme) in BUILTIN {
        let c = inks(theme);
        let primary = keep_side(c.accent, c.bar_bg, WORD_AA);
        if contrast(c.accent, c.bar_bg) >= WORD_AA {
            assert_eq!(primary, c.accent, "{name}: an accent at AA is untouched");
        } else if oklch(c.accent).1 > 2.0 * 0.03 {
            let (h0, h1) = (oklch(c.accent).2, oklch(primary).2);
            let turn = (h1 - h0 + 540.0).rem_euclid(360.0) - 180.0;
            assert!(
                turn.abs() < 3.0,
                "{name}: the deepened accent keeps its hue"
            );
        }
        // …and the LIT Primary is a whole lift off the rest it wears
        // (ruling 20, kept by ruling 160): at least `PRIMARY_HOVER_LIFT`
        // of the way from the worn accent to the theme's ink on every
        // channel, up to rounding — the AA deepening only carries it on.
        let rest = chip_inks(&c, CapsuleRole::Primary, false, false).1;
        let lit = chip_inks(&c, CapsuleRole::Primary, true, false).1;
        assert_eq!(
            rest, primary,
            "{name}: the resting Primary wears the AA accent"
        );
        if c.accent != c.primary_lift_toward {
            let want = mix3(rest, c.primary_lift_toward, ink::PRIMARY_HOVER_LIFT);
            for k in 0..3 {
                let (a, f) = (f64::from(rest[k]), f64::from(c.primary_lift_toward[k]));
                let toward = (f - a).signum();
                assert!(
                    (f64::from(lit[k]) - a) * toward + 1.0 >= (f64::from(want[k]) - a) * toward,
                    "{name}: channel {k} of the lit Primary {lit:?} is short of a whole \
                     lift from the rest {rest:?} toward {:?}",
                    c.primary_lift_toward
                );
            }
        }
        for (ground, msg) in &rows {
            let mut center = MessageCenter::new(MessageLog::empty(), now);
            center.post(msg.clone(), stamp(), now);
            center.commit_rows(now, 3);
            let p = present(&center, 120);
            let caps = &p.rows[0].capsules;
            assert_eq!(caps.len(), 3, "{name} {ground}: {caps:?}");
            let hovers = std::iter::once(None).chain(caps.iter().map(|cap| {
                Some(Hover {
                    row: 0,
                    target: HoverTarget::Capsule(cap.action),
                })
            }));
            for hover in hovers {
                let painted = paint_still(&p, &c, false, hover, &center);
                for cap in caps {
                    for cell in &painted[0][cap.col..cap.col + cap.width] {
                        if cell.ch == ' ' {
                            continue;
                        }
                        let ratio = contrast(cell.fg, cell.bg);
                        assert!(
                            ratio >= WORD_AA - 1e-9,
                            "{name} {ground} {:?} {:?} hover {hover:?}: {:?} on {:?} is \
                             {ratio:.2}:1",
                            cap.role,
                            cell.ch,
                            cell.fg,
                            cell.bg
                        );
                    }
                }
            }
        }
    }
}

/// CRISP INK ON FILLS (design ruling 222): on every builtin scheme and in
/// every frame of a determinate row's life — its glide across the words,
/// the glint's travel, the Complete echo's wipe and bloom and the Fault
/// echo's flash — every word cell over the fill wears the row's ONE crisp
/// ink (the band's `bar_bg` or `value`, whichever reads better on the
/// resting fill): it clears 4.5:1 always, 7:1 on the resting fill wherever
/// either candidate reaches it there, and it never changes side of the
/// ground under it — no letter flips as the edge, the glint or an echo
/// passes.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one row's life, frame by frame, on every scheme"
)]
fn words_on_a_fill_wear_the_rows_crisp_ink_in_every_frame() {
    use crate::{
        ANIM_FRAME, Duration, ECHO_FAULT_FLASH, ECHO_SWEEP, GLINT_DELAY, GLINT_TRAVEL, Outcome,
    };
    let cols = 120;
    let mut sevens = 0;
    let themes = BUILTIN;
    for (name, theme) in &themes {
        let (name, theme) = (*name, *theme);
        let c = inks(theme);
        let crisp = fill_ink(&c, c.accent);
        let best = |bg: [u8; 3]| {
            [c.field_bg, c.bar_bg, c.value]
                .map(|ink| contrast(ink, bg))
                .into_iter()
                .fold(0.0f64, f64::max)
        };
        if best(c.accent) >= WORD_AA {
            assert!(
                (contrast(crisp, c.accent) - best(c.accent)).abs() < 1e-9,
                "{name}: the crisp ink is the better candidate on the fill"
            );
        }
        if best(c.accent) >= 7.0 {
            sevens += 1;
        }
        let crisp_lighter = fill_anchor(c.accent) == ink::WHITE;
        // (start fill, the life to run): a glide from 20 % to 95 %, the
        // glint at 95 %, a Complete echo from 95 %, a Fault echo from 95 %.
        for (life, outcome) in [
            ("glide", None),
            ("glint", None),
            ("complete", Some(Outcome::Ok)),
            ("fault", Some(Outcome::Warn)),
        ] {
            let now = t0();
            let mut center = MessageCenter::new(MessageLog::empty(), now);
            let start = if life == "glide" { 200 } else { 950 };
            let row = |fill: u16| {
                Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
                    .no_excerpt()
                    .meter(Meter {
                        fill_permille: Some(fill),
                        stats: "31 MB / 74 MB".into(),
                        ..Meter::default()
                    })
                    .hold(Hold::Live {
                        stale_after: crate::STALE_UPDATE,
                    })
            };
            let id = center.post(row(start), stamp(), now).id;
            center.commit_rows(now, 3);
            let (from, span) = match (life, outcome) {
                ("glide", _) => {
                    let at = now + Duration::from_millis(500);
                    center.restate(
                        id,
                        crate::Restatement {
                            meter: Some(row(950).meter),
                            ..crate::Restatement::default()
                        },
                        at,
                    );
                    (at, Duration::from_millis(1500))
                }
                (_, None) => (now + GLINT_DELAY, GLINT_TRAVEL),
                (_, Some(o)) => {
                    let at = now + Duration::from_millis(1000);
                    assert!(center.resolve(id, o, at));
                    let span = if o == Outcome::Ok {
                        crate::animate::glide_span(800, 1000) + ECHO_SWEEP
                    } else {
                        ECHO_FAULT_FLASH
                    };
                    (at, span)
                }
            };
            let p = present(&center, cols);
            let l = &p.rows[0];
            // Every word cell on the row: glyph, title, percent, stats.
            let mut words: Vec<usize> = vec![l.glyph.0];
            let mut add = |col: usize, text: &str| {
                for (k, ch) in text.chars().enumerate() {
                    if !ch.is_whitespace() {
                        words.push(col + k);
                    }
                }
            };
            add(l.title.0, &l.title.1);
            if let Some((col, text)) = &l.pct {
                add(*col, text);
            }
            if let Some((col, text)) = &l.stats {
                add(*col, text);
            }
            let mut t = Duration::ZERO;
            while t <= span {
                let (rows, m) = frame_at(&center, &p, from + t, Look::MOVING, &c);
                let (_, rasters) = frame_rasters(&center, &p, from + t, Look::MOVING, &c);
                let geom = Geometry::cells_only(cols);
                let span_of = MeterSpan { geom, cols };
                for &x in &words {
                    // Wholly on the fill: the cell the edge crosses wears
                    // the track's ink, split by the renderer (ruling 242).
                    let on_fill = m.rows[0].surface.is_empty()
                        || u32::from(span_of.fine(&m.rows[0].surface, x).fill)
                            + u32::from(span_of.fine(&m.rows[0].surface, x).warn)
                            >= 255 << 8;
                    let (fg, bg) = (rows[0][x].fg, rows[0][x].bg);
                    let at = format!("{name} {life} t={t:?} col {x}: {fg:?} on {bg:?}");
                    for (ink, g) in word_grounds(&rows[0], rasters[0].as_ref(), geom, x) {
                        let r = contrast(ink, g);
                        assert!(r >= WORD_AA - 0.02, "{at}: under AA ({r:.2}) on {g:?}");
                    }
                    let ratio = contrast(fg, bg);
                    if !on_fill || rows[0][x].ch == ' ' {
                        continue;
                    }
                    assert_eq!(
                        luminance(fg) > luminance(bg),
                        crisp_lighter,
                        "{at}: the word changed side over the fill"
                    );
                    if bg == c.accent {
                        let (inks, _) = row_inks(&c, l, false, false);
                        assert_eq!(
                            fg,
                            crisp_on_fill(&c, c.accent, &inks),
                            "{at}: the crisp ink on the fill"
                        );
                        assert!(
                            ratio >= best(bg).min(7.0) - 1e-9,
                            "{at}: {ratio:.2}:1 where a candidate reaches {:.2}",
                            best(bg)
                        );
                    }
                }
                t += ANIM_FRAME;
            }
        }
    }
    assert!(sevens > 0, "some scheme reaches 7:1 on its fill");
    // The default ground: the band's own dark on the neon cursor green.
    let c = inks(DEFAULT);
    let crisp = contrast(fill_ink(&c, c.accent), c.accent);
    assert!(crisp >= 7.0, "the default ground's crisp ink: {crisp:.2}:1");
}

/// THE BAND'S REPAINT KEY KEEPS ITS VALUES (ruling 328): the key moved into
/// the engine with its arithmetic — [`Geometry`]'s derived `Hash` and the std
/// hasher's fixed keys — so a key is the number the host's copy computed.
/// Pinned from the host's copy before the move (round 25's oracle held all
/// 772,500 of its keys equal across the move).
#[test]
fn band_fp_keeps_the_hosts_values() {
    let fp = 0x1234_5678;
    let (g0, g80, g120) = (
        Geometry::cells_only(0),
        Geometry::cells_only(80),
        Geometry::cells_only(120),
    );
    assert_eq!(band_fp(fp, None, g0, 0), 0x4b24_2ffa_f170_5c97);
    assert_eq!(band_fp(fp, None, g0, 1), 0x175e_6053_27ef_d9f3);
    assert_eq!(band_fp(fp, None, g0, 0x1234), 0x2956_306c_4568_6581);
    assert_eq!(band_fp(fp, None, g80, 0), 0x9a19_f0ca_8586_9c7b);
    // The two hover keys below were re-pinned by ruling 330 (the presence
    // bit moved below the target); the no-hover keys above are unchanged.
    let body = Some(Hover {
        row: 0,
        target: HoverTarget::Body,
    });
    assert_eq!(band_fp(fp, body, g80, 0x1234), 0xf541_03e4_25dc_0379);
    let chip = Some(Hover {
        row: 2,
        target: HoverTarget::Capsule(ActionIndex(1)),
    });
    assert_eq!(band_fp(fp, chip, g120, 1), 0x2920_c264_5045_fa0d);
}

/// EVERY HOVER TARGET ON A ROW IS ITS OWN KEY (ruling 330): a hover that
/// moves from one capsule to the next must change the key, or the band
/// never repaints the highlight. NEGATIVE CONTROL: the term the band used
/// before OR-ed its presence bit into the target, so Capsule(0) and
/// Capsule(1) — and every even/odd pair — collided.
#[test]
fn every_hover_target_on_a_row_is_its_own_key() {
    let fp = 0x1234_5678;
    let g = Geometry::cells_only(120);
    let key = |target| band_fp(fp, Some(Hover { row: 1, target }), g, 0);
    let mut seen = std::collections::HashSet::new();
    assert!(seen.insert(band_fp(fp, None, g, 0)), "no hover");
    assert!(seen.insert(key(HoverTarget::Body)), "the body");
    for k in 0..=8u8 {
        assert!(
            seen.insert(key(HoverTarget::Capsule(ActionIndex(k)))),
            "capsule {k} shares a key"
        );
    }
    let old = |row: u16, t: u64| (u64::from(row) << 16) | t | 1;
    assert_eq!(old(1, 0x200), old(1, 0x201), "the old term collided");
}
