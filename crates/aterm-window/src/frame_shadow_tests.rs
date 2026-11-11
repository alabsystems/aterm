// Copyright (c) 2022 Bartłomiej Maryńczak
// SPDX-License-Identifier: MIT
//
// Derived from sctk-adwaita 0.10.1 src/shadow.rs (the shadow, and its rendering kept by scale
// and focus), src/theme.rs (the frame's sizes
// and its border colours), src/parts.rs (where the border parts sit and their input regions)
// and src/lib.rs (`precise_location`, and the visible border's rectangles in `redraw_inner`),
// and modified by the aterm project in 2026. See ../LICENSE-MIT-sctk-adwaita for the upstream
// MIT license.

use super::*;

fn paint(part: Part, look: Look) -> Pixmap {
    Shadows::default().paint(part, look)
}

#[test]
fn a_kept_rendering_paints_what_a_fresh_one_does() {
    let mut shadows = Shadows::default();
    let base = Look { width: 744, height: 528, scale: 1, active: true, dark: false, tiled: false };
    let looks = [
        base,
        Look { width: 900, height: 600, ..base },
        Look { scale: 2, ..base },
        Look { active: false, ..base },
        Look { active: true, dark: true, ..base },
        Look { scale: 2, active: false, ..base },
        Look { tiled: true, ..base },
        base,
    ];
    for look in looks {
        for part in Part::ALL {
            let kept = shadows.paint(part, look);
            assert_eq!(kept.pixels, paint(part, look).pixels, "{part:?} {look:?}");
            if !look.tiled {
                let mut plane =
                    shadow_plane(part, look.width, look.height, look.scale, look.active);
                let color = if look.dark { BORDER_DARK } else { BORDER_LIGHT };
                plane.fill(border_span(part, look.width, look.height, look.scale), color);
                assert_eq!(kept.pixels, plane.pixels, "{part:?} {look:?}");
            }
        }
    }
    assert_eq!(shadows.rendered.len(), 4, "one rendering a scale and focus");
}

const RECORDED: &str = include_str!("../tests/recorded/sctk-adwaita-0.10.1-shadow.tsv");

fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn field<'a>(row: &[&'a str], name: &str) -> &'a str {
    row.iter()
        .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("no {name} in {row:?}"))
}

fn pair(v: &str) -> (u32, u32) {
    let (a, b) = v.split_once('x').expect("WxH");
    (a.parse().expect("a width"), b.parse().expect("a height"))
}

fn part_named(name: &str) -> Part {
    match name {
        "top" => Part::Top,
        "left" => Part::Left,
        "right" => Part::Right,
        "bottom" => Part::Bottom,
        other => panic!("no part {other}"),
    }
}

#[test]
fn the_shadow_is_sctk_adwaita_s_to_the_byte() {
    let mut rows = 0;
    for line in RECORDED.lines().filter(|l| l.starts_with("plane\t")) {
        let row: Vec<&str> = line.split('\t').collect();
        let part = part_named(field(&row, "part"));
        let (w, h) = pair(field(&row, "window"));
        let scale: u32 = field(&row, "scale").parse().expect("a scale");
        let active = field(&row, "active") == "1";
        let plane = shadow_plane(part, w, h, scale, active);
        let (bw, bh) = pair(field(&row, "buffer"));
        assert_eq!((plane.width, plane.height), (bw as usize, bh as usize), "{line}");
        let alpha: u64 = plane.pixels.iter().map(|p| u64::from(p[3])).sum();
        assert_eq!(alpha.to_string(), field(&row, "alpha_sum"), "{line}");
        let hash = fnv(plane.pixels.iter().flatten().copied());
        assert_eq!(format!("{hash:016x}"), field(&row, "fnv"), "{line}");
        rows += 1;
    }
    assert_eq!(rows, 120, "every recorded plane was drawn");
}

#[test]
fn the_parts_sit_around_the_window_and_take_the_pointer_next_to_it() {
    let (w, h) = (744, 528);
    assert_eq!(surface_rect(Part::Top, w, h), Rect { x: -44, y: -44, width: 832, height: 44 });
    assert_eq!(surface_rect(Part::Left, w, h), Rect { x: -44, y: 0, width: 44, height: 528 });
    assert_eq!(surface_rect(Part::Right, w, h), Rect { x: 744, y: 0, width: 44, height: 528 });
    assert_eq!(surface_rect(Part::Bottom, w, h), Rect { x: -44, y: 528, width: 832, height: 44 });
    assert_eq!(input_rect(Part::Top, w, h), Rect { x: 32, y: 32, width: 768, height: 12 });
    assert_eq!(input_rect(Part::Left, w, h), Rect { x: 32, y: 0, width: 12, height: 528 });
    assert_eq!(input_rect(Part::Right, w, h), Rect { x: 0, y: 0, width: 12, height: 528 });
    assert_eq!(input_rect(Part::Bottom, w, h), Rect { x: 32, y: 0, width: 768, height: 12 });
    for part in Part::ALL {
        assert_eq!(Part::ALL[part.index()], part);
    }
}

#[cfg(not(macos_platform))]
#[test]
fn a_press_resizes_by_the_edge_and_by_the_corner_near_it() {
    use ResizeDirection::*;
    let (w, h) = (744, 528);
    assert_eq!(direction(Part::Right, 6.0, 264.0, w, h), East);
    assert_eq!(direction(Part::Right, 6.0, 24.0, w, h), NorthEast);
    assert_eq!(direction(Part::Right, 6.0, 25.0, w, h), East);
    assert_eq!(direction(Part::Right, 6.0, 504.0, w, h), SouthEast);
    assert_eq!(direction(Part::Left, 38.0, 3.0, w, h), NorthWest);
    assert_eq!(direction(Part::Left, 38.0, 527.0, w, h), SouthWest);
    assert_eq!(direction(Part::Left, 38.0, 300.0, w, h), West);
    assert_eq!(direction(Part::Top, 68.0, 38.0, w, h), NorthWest);
    assert_eq!(direction(Part::Top, 69.0, 38.0, w, h), North);
    assert_eq!(direction(Part::Top, 764.0, 38.0, w, h), NorthEast);
    assert_eq!(direction(Part::Bottom, 400.0, 3.0, w, h), South);
    assert_eq!(direction(Part::Bottom, 10.0, 3.0, w, h), SouthWest);
    assert_eq!(direction(Part::Bottom, 800.0, 3.0, w, h), SouthEast);
}

#[test]
fn the_border_runs_around_the_window_in_the_theme_s_colour() {
    let look = Look { width: 100, height: 50, scale: 2, active: true, dark: true, tiled: false };
    let top = paint(Part::Top, look);
    let at = |p: &Pixmap, x: usize, y: usize| p.pixels[y * p.width + x];
    assert_eq!(at(&top, 86, 87), BORDER_DARK);
    assert_eq!(at(&top, 86, 86), BORDER_DARK);
    assert_ne!(at(&top, 85, 87), BORDER_DARK, "the shadow beyond the border's end");
    assert_eq!(at(&top, 88 + 200 + 1, 87), BORDER_DARK);
    assert_ne!(at(&top, 88 + 200 + 2, 87), BORDER_DARK);
    assert_ne!(at(&top, 100, 85), BORDER_DARK, "the shadow above the border");
    let right = paint(Part::Right, Look { dark: false, ..look });
    assert_eq!(at(&right, 0, 50), BORDER_LIGHT);
    assert_eq!(at(&right, 1, 99), BORDER_LIGHT);
    assert_ne!(at(&right, 2, 50), BORDER_LIGHT);
    let tiled = paint(Part::Bottom, Look { tiled: true, ..look });
    assert_eq!(at(&tiled, 100, 0), BORDER_DARK);
    assert!(tiled.pixels[2 * tiled.width..].iter().all(|p| *p == [0; 4]));
}

#[test]
fn a_buffer_is_argb8888_little_endian() {
    let pixmap = Pixmap { width: 1, height: 1, pixels: vec![[1, 2, 3, 4]] };
    assert_eq!(pixmap.argb8888(), [3, 2, 1, 4]);
}

#[test]
fn a_window_smaller_than_the_corners_draws_without_a_panic() {
    for part in Part::ALL {
        for (w, h) in [(1, 1), (2, 1), (30, 20)] {
            let look =
                Look { width: w, height: h, scale: 3, active: false, dark: false, tiled: false };
            let pixmap = paint(part, look);
            assert_eq!((pixmap.width, pixmap.height), buffer_size(part, w, h, 3));
        }
    }
}
