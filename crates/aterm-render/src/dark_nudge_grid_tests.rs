// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests verifying that `font_weight_dark_nudge` never changes cell geometry across sizes.

use super::*;

const WGHT: u32 = u32::from_be_bytes(*b"wght");

/// The sizes the box is compared at: what windows at 1x, 1.5x, 2x and 3x of
/// the common font sizes ask for, and the 9 px the defect was found at.
const SIZES: [f32; 12] = [
    9.0, 11.0, 12.0, 13.5, 16.0, 18.0, 19.5, 24.0, 27.0, 32.0, 36.0, 48.0,
];

/// The sizes a renderer is built at (the active px when the nudge is decided).
const ACTIVE: [f32; 4] = [9.0, 16.0, 24.0, 32.0];

fn light() -> Theme {
    Theme {
        fg: 0x0010_1010,
        bg: 0x00FA_FAFA,
        ..Theme::default()
    }
}

/// Its `'M'` advance moves with `wght` (it is not monospaced).
fn cinzel() -> Vec<u8> {
    let mut bytes = include_bytes!("../assets/game/Cinzel-var.ttf").to_vec();
    bytes.push(0);
    assert!(display_face_fit(&bytes).is_none(), "not the display face");
    bytes
}

/// What the face derives at every size in [`SIZES`] now (no kept answer).
fn boxes(r: &Renderer) -> Vec<CellGeometry> {
    SIZES
        .iter()
        .map(|&px| r.derive_window_cell_geometry(px).expect("metrics"))
        .collect()
}

/// The sizes at which two box lists differ.
fn moved_at(a: &[CellGeometry], b: &[CellGeometry]) -> Vec<f32> {
    SIZES
        .iter()
        .zip(a.iter().zip(b))
        .filter(|(_, (x, y))| x != y)
        .map(|(&px, _)| px)
        .collect()
}

/// A renderer over `bytes` at `px` on the default (dark) theme, configured
/// with `requests` and `nudge`.
fn configured(bytes: &[u8], px: f32, requests: &[(u32, f32)], nudge: f32) -> Renderer {
    let mut r = Renderer::from_bytes(bytes, px, Theme::default()).expect("parses");
    assert!(theme_is_dark(r.theme.bg), "starts dark");
    let _ = r.set_font_variations(requests, nudge);
    r
}

/// The reviewer's case (2026-10-16): Cinzel at `wght` 500 with a nudge of
/// 100. Its 600 instance has the 500 instance's box at 16 px but not at 9 px,
/// so a gate that compares the two boxes at the active size alone admits the
/// nudge at 16 px and a theme flip then moves the box of a window at 9 px.
/// Whatever size is active when the nudge is decided, a flip either way now
/// moves no box at any size, and the kept answers are what the face derives.
///
/// NEGATIVE CONTROL: the 600 instance's own box does move against the 500
/// one's at some size in [`SIZES`] while it holds at 16 px, so the one-size
/// gate would have admitted it there (measured before the fix: built at
/// 16 px, the flip to light moved the box at 9 and 27 px).
///
/// Held twice, and mutation-checked: the gate refuses this nudge (restoring
/// the one-size gate fails the next test), and the box is derived from the
/// un-nudged instance whatever the gate admits (deriving it from the drawn
/// instance fails the SF Mono test's "derives nothing"). Undoing both fails
/// this one.
#[test]
fn a_theme_flip_moves_no_cell_box_at_any_size() {
    let bytes = cinzel();
    let at = |w: f32| boxes(&configured(&bytes, 16.0, &[(WGHT, w)], 0.0));
    let (plain, heavier) = (at(500.0), at(600.0));
    let moved = moved_at(&plain, &heavier);
    assert!(
        !moved.is_empty(),
        "CONTROL: the nudged instance moves the box"
    );
    assert!(!moved.contains(&16.0), "CONTROL: but not at 16 px");

    for px in ACTIVE {
        let mut r = configured(&bytes, px, &[(WGHT, 500.0)], 100.0);
        let dark = boxes(&r);
        for &size in &SIZES {
            let _ = r.cell_geometry(size);
        }
        r.set_theme(light());
        assert_eq!(
            moved_at(&dark, &boxes(&r)),
            Vec::<f32>::new(),
            "to light at {px}"
        );
        assert_eq!(
            SIZES.map(|size| r.cell_geometry(size)).to_vec(),
            boxes(&r),
            "kept answers after the flip at {px}"
        );
        r.set_theme(Theme::default());
        assert_eq!(
            moved_at(&dark, &boxes(&r)),
            Vec::<f32>::new(),
            "to dark at {px}"
        );
    }
}

/// NEGATIVE CONTROL: before the fix Cinzel 500 + 100 resolved 500 at 9 px
/// and 600 at 16 px.
#[test]
fn the_nudge_does_not_depend_on_the_active_size() {
    let bytes = cinzel();
    for (requests, nudge) in [
        (vec![(WGHT, 500.0)], 100.0),
        (vec![], 100.0),
        (vec![], 30.0),
        (vec![(WGHT, 700.0)], 50.0),
    ] {
        let plain = configured(&bytes, 16.0, &requests, 0.0);
        let want = plain.variation_coords().map(<[_]>::to_vec);
        for px in ACTIVE {
            let mut r = configured(&bytes, px, &requests, nudge);
            let label = format!("{requests:?} + {nudge} at {px}");
            assert_eq!(r.variation_coords().map(<[_]>::to_vec), want, "{label}");
            for other in ACTIVE {
                r.activate_px(other);
                r.set_theme(Theme {
                    bg: 0x0020_2020,
                    ..Theme::default()
                });
                r.set_theme(Theme::default());
                assert_eq!(
                    r.variation_coords().map(<[_]>::to_vec),
                    want,
                    "{label}, re-decided at {other}"
                );
            }
        }
    }
}

/// CONTROL: the decision really does differ between two active sizes.
#[test]
fn a_nudge_decided_by_size_still_moves_no_box() {
    let bytes = cinzel();
    let requests = vec![(WGHT, 600.0)];
    let plain = boxes(&configured(&bytes, 16.0, &requests, 0.0));
    let mut decided = Vec::new();
    for px in ACTIVE {
        let r = configured(&bytes, px, &requests, 5.0);
        decided.push(r.variation_coords().map(<[_]>::to_vec));
        assert_eq!(
            moved_at(&plain, &boxes(&r)),
            Vec::<f32>::new(),
            "decided at {px}"
        );
    }
    assert!(
        decided.windows(2).any(|w| w[0] != w[1]),
        "the advance check is decided by size here: {decided:?}"
    );
}

/// The system SF Mono, a variable face every macOS ships: monospaced, so its
/// instances share one `'M'` advance and one set of line metrics.
#[cfg(target_os = "macos")]
fn sf_mono() -> Vec<u8> {
    std::fs::read("/System/Library/Fonts/SFNSMono.ttf").expect("macOS ships SFNSMono.ttf")
}

/// NEGATIVE CONTROL: a write that is an input of the box (the line height)
/// derives each kept size again.
#[cfg(target_os = "macos")]
#[test]
fn a_nudge_that_keeps_the_metrics_is_kept_and_the_flip_derives_nothing() {
    use crate::test_counters::take_cell_geometry_derivations;
    let bytes = sf_mono();
    let kept = [12.0, 16.0, 24.0, 32.0];
    for px in ACTIVE {
        let plain = configured(&bytes, px, &[], 0.0);
        let mut r = configured(&bytes, px, &[], 100.0);
        let base = plain.variation_coords().expect("instantiated").to_vec();
        let nudged = r.variation_coords().expect("instantiated").to_vec();
        let wi = base.iter().position(|&(tag, _)| tag == WGHT).expect("wght");
        assert_eq!(nudged[wi].1, base[wi].1 + 100.0, "kept at {px}");
        let dark = boxes(&r);
        assert_eq!(
            dark,
            boxes(&plain),
            "the nudged box is the plain one at {px}"
        );
        let answers = kept.map(|size| r.cell_geometry(size));
        let _ = take_cell_geometry_derivations();
        r.set_theme(light());
        assert_eq!(
            r.variation_coords(),
            Some(&base[..]),
            "light un-nudges at {px}"
        );
        assert_eq!(
            moved_at(&dark, &boxes(&r)),
            Vec::<f32>::new(),
            "to light at {px}"
        );
        let _ = take_cell_geometry_derivations();
        assert_eq!(
            kept.map(|size| r.cell_geometry(size)),
            answers,
            "kept at {px}"
        );
        r.set_theme(Theme::default());
        assert_eq!(
            r.variation_coords(),
            Some(&nudged[..]),
            "dark nudges at {px}"
        );
        assert_eq!(
            kept.map(|size| r.cell_geometry(size)),
            answers,
            "kept at {px}"
        );
        assert_eq!(
            take_cell_geometry_derivations(),
            0,
            "a flip derives nothing at {px}"
        );
        r.set_line_height(1.2);
        let _ = kept.map(|size| r.cell_geometry(size));
        assert_eq!(
            take_cell_geometry_derivations(),
            kept.len(),
            "CONTROL: an input's write derives each kept size again"
        );
    }
}
