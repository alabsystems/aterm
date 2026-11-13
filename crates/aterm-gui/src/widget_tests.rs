// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::hash::Hasher;

use super::*;

/// [`ArtMask::drawn`] answers what a scan of every pixel in the span answers, for spans
/// inside, across and outside the box, inverted, and NaN.
#[test]
fn an_art_masks_counts_find_what_a_pixel_scan_finds() {
    let scan = |alpha: &[u8], w: u32, h: u32, part: ArtSpan| {
        let (wf, hf) = (w as f32, h as f32);
        let cols =
            (part.left * wf).floor().max(0.0) as u32..(part.right * wf).ceil().min(wf) as u32;
        let rows =
            (part.top * hf).floor().max(0.0) as u32..(part.bottom * hf).ceil().min(hf) as u32;
        let mut seen: Option<(u32, u32, u32, u32)> = None;
        for row in rows {
            for col in cols.clone() {
                if alpha[(row * w + col) as usize] >= ArtMask::DRAWN {
                    seen = Some(seen.map_or((col, row, col, row), |(l, t, r, b)| {
                        (l.min(col), t.min(row), r.max(col), b.max(row))
                    }));
                }
            }
        }
        seen.map(|(l, t, r, b)| ArtSpan {
            left: (l as f32 / wf).max(part.left),
            top: (t as f32 / hf).max(part.top),
            right: ((r + 1) as f32 / wf).min(part.right),
            bottom: ((b + 1) as f32 / hf).min(part.bottom),
        })
    };
    let edges = [
        -0.3, 0.0, 0.01, 0.1, 0.25, 0.333, 0.5, 0.6, 0.77, 0.9, 0.999, 1.0, 1.4,
    ];
    for art in [
        ImageArt::Kitty {
            pose: ImageArt::BRAND_POSE,
            dark: false,
        },
        ImageArt::Kitty {
            pose: ImageArt::BRAND_POSE,
            dark: true,
        },
        ImageArt::Kitty {
            pose: aterm_effects::pet_glyphs_gen::PetGlyphId::PetLoaf,
            dark: true,
        },
    ] {
        let mask = art.mask();
        let alpha: Vec<u8> = art
            .bake(mask.w, mask.h)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[3])
            .collect();
        let mut spans = vec![
            ArtSpan::BOX,
            ArtSpan {
                left: f32::NAN,
                ..ArtSpan::BOX
            },
        ];
        for &left in &edges {
            for &right in &edges {
                for (top, bottom) in [(0.0, 1.0), (0.2, 0.45), (0.5, 0.5), (0.9, 0.1), (-1.0, 2.0)]
                {
                    spans.push(ArtSpan {
                        left,
                        top,
                        right,
                        bottom,
                    });
                }
            }
        }
        for part in spans {
            assert_eq!(
                mask.drawn(part),
                scan(&alpha, mask.w, mask.h, part),
                "{art:?} {part:?}"
            );
        }
    }
}

fn fp(p: &DrawPrim) -> u64 {
    let mut state = std::collections::hash_map::DefaultHasher::new();
    p.fingerprint_into(&mut state);
    state.finish()
}

fn spec() -> TerminalSpecimenSpec {
    TerminalSpecimenSpec {
        input: Arc::new(RenderInput::default()),
        input_fingerprint: 1,
        motion: None,
        prepared_font: crate::tray_raster::PreparedSemanticFont::unavailable(
            SemanticFontCandidate::default(),
        ),
        theme: aterm_render::Theme::default(),
        font_px: 14.0,
        line_height: 1.0,
        baseline_adjust: 0,
        ligatures: true,
        merged_ligatures: false,
        cursor_break_ligatures: true,
        synthetic_styles: true,
        underline_position: 0,
        underline_thickness: 0,
        underline_skip_descenders: true,
        text_blending: SpecimenTextBlending::default(),
        font_thicken: false,
        stem_gamma: 1.0,
        variations: Vec::new(),
        minimum_contrast: 1.0,
        selection_foreground: None,
        selection_inactive: false,
        wallpaper: None,
    }
}

/// One prim of every variant (`nudged` names each, so a new variant
/// does not compile until it has a row there and a sample here).
fn samples() -> Vec<DrawPrim> {
    let ink = [10, 20, 30, 200];
    let other = [40, 50, 60, 255];
    vec![
        DrawPrim::Panel {
            x: 1.5,
            y: 2.5,
            w: 30.0,
            h: 12.0,
            radius: 3.0,
            fill: ink,
        },
        DrawPrim::Ring {
            cx: 8.0,
            cy: 8.0,
            r_outer: 6.0,
            thickness: 1.5,
            track: ink,
            start: 0.25,
            frac: 0.5,
            fill: other,
        },
        DrawPrim::Capsule {
            x: 1.0,
            y: 2.0,
            w: 40.0,
            h: 6.0,
            frac: 0.25,
            fill: ink,
            track: other,
        },
        DrawPrim::Dot {
            cx: 3.0,
            cy: 4.0,
            r: 2.0,
            color: ink,
        },
        DrawPrim::Text {
            x: 4.0,
            baseline: 11.0,
            s: "CPU 42%".into(),
            px: 11.0,
            color: ink,
            weight: TextWeight::Regular,
            face: TextFace::Ui,
        },
        DrawPrim::TerminalSpecimen {
            x: 2.0,
            y: 3.0,
            spec: Box::new(spec()),
        },
        DrawPrim::Stroke {
            x: 1.0,
            y: 1.0,
            w: 20.0,
            h: 10.0,
            radius: 2.0,
            width: 1.0,
            color: ink,
        },
        DrawPrim::Line {
            x1: 1.0,
            y1: 2.0,
            x2: 9.0,
            y2: 7.0,
            width: 1.5,
            color: ink,
        },
        DrawPrim::Arc {
            cx: 9.0,
            cy: 9.0,
            r: 6.0,
            width: 2.5,
            from: -0.375,
            to: 0.125,
            color: ink,
        },
        DrawPrim::Dashed {
            x: 1.0,
            y: 2.0,
            w: 20.0,
            h: 10.0,
            width: 1.0,
            on: 1.5,
            off: 1.0,
            color: ink,
        },
        DrawPrim::HsvDisk {
            cx: 20.0,
            cy: 20.0,
            r: 16.0,
            value: 0.8,
        },
        DrawPrim::ClipPush {
            x: 0.5,
            y: 0.5,
            w: 50.0,
            h: 30.0,
        },
        DrawPrim::ClipPop,
        DrawPrim::FadePush {
            y: 10.0,
            h: 4.0,
            from: 1.0,
            to: 0.25,
        },
        DrawPrim::FadePop,
        DrawPrim::SubpixelPush,
        DrawPrim::SubpixelPop,
        DrawPrim::BandIcon {
            x: 2.0,
            y: 3.0,
            size: 9.0,
            icon: aterm_render::BandIcon::Info,
            color: ink,
        },
        DrawPrim::Image {
            x: 2.0,
            y: 3.0,
            w: 24.0,
            h: 12.0,
            art: ImageArt::Kitty {
                pose: ImageArt::BRAND_POSE,
                dark: true,
            },
            alpha: 200,
        },
    ]
}

/// The least changes a field's value can take.
trait Nudge: Sized {
    fn nudges(&self) -> Vec<Self>;
}

impl Nudge for f32 {
    /// One ulp up.
    fn nudges(&self) -> Vec<Self> {
        vec![Self::from_bits(self.to_bits() + 1)]
    }
}

impl Nudge for Rgba {
    /// Each channel's low bit, alpha included.
    fn nudges(&self) -> Vec<Self> {
        (0..4)
            .map(|i| {
                let mut c = *self;
                c[i] ^= 1;
                c
            })
            .collect()
    }
}

impl Nudge for String {
    /// One character more, and the first one swapped.
    fn nudges(&self) -> Vec<Self> {
        let mut swapped = self.clone().into_bytes();
        swapped[0] ^= 1;
        vec![
            format!("{self}."),
            Self::from_utf8(swapped).expect("an ASCII sample"),
        ]
    }
}

impl Nudge for TextWeight {
    fn nudges(&self) -> Vec<Self> {
        [Self::Regular, Self::Bold]
            .into_iter()
            .filter(|w| w != self)
            .collect()
    }
}

impl Nudge for TextFace {
    fn nudges(&self) -> Vec<Self> {
        [
            Self::Mono,
            Self::Ui,
            Self::UiBold,
            Self::Display(0),
            Self::Display(1),
        ]
        .into_iter()
        .filter(|f| f != self)
        .collect()
    }
}

impl Nudge for aterm_render::BandIcon {
    fn nudges(&self) -> Vec<Self> {
        [Self::Info, Self::Success]
            .into_iter()
            .filter(|i| i != self)
            .collect()
    }
}

impl Nudge for u8 {
    /// The low bit.
    fn nudges(&self) -> Vec<Self> {
        vec![*self ^ 1]
    }
}

impl Nudge for ImageArt {
    fn nudges(&self) -> Vec<Self> {
        let Self::Kitty { pose, dark } = *self;
        let other = if pose == ImageArt::BRAND_POSE {
            aterm_effects::pet_glyphs_gen::PetGlyphId::PetSit
        } else {
            ImageArt::BRAND_POSE
        };
        vec![
            Self::Kitty { pose, dark: !dark },
            Self::Kitty { pose: other, dark },
        ]
    }
}

impl Nudge for Box<TerminalSpecimenSpec> {
    /// Each setting the specimen's pixels depend on, the font's readiness
    /// alone included. `input` is not read: `input_fingerprint` is its
    /// identity. `motion` hashes by its scene's fingerprint, and no scene
    /// is built here.
    fn nudges(&self) -> Vec<Self> {
        let edits: &[fn(&mut TerminalSpecimenSpec)] = &[
            |s| s.input_fingerprint += 1,
            |s| s.prepared_font.candidate.regular = Some("Menlo".into()),
            |s| s.prepared_font.snapshot.status.push('.'),
            |s| s.prepared_font.snapshot.ready_epoch += 1,
            |s| s.prepared_font.snapshot.pending ^= true,
            |s| {
                let mut ready = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
                    &s.prepared_font.candidate,
                );
                ready.snapshot = s.prepared_font.snapshot.clone();
                assert!(ready.is_ready() && !s.prepared_font.is_ready());
                assert!(ready.matches(&s.prepared_font.candidate));
                s.prepared_font = ready;
            },
            |s| s.theme.bg ^= 1,
            |s| s.font_px = s.font_px.nudges()[0],
            |s| s.line_height = s.line_height.nudges()[0],
            |s| s.baseline_adjust += 1,
            |s| s.ligatures ^= true,
            |s| s.merged_ligatures ^= true,
            |s| s.cursor_break_ligatures ^= true,
            |s| s.synthetic_styles ^= true,
            |s| s.underline_position += 1,
            |s| s.underline_thickness += 1,
            |s| s.underline_skip_descenders ^= true,
            |s| s.text_blending = SpecimenTextBlending::Linear,
            |s| s.font_thicken ^= true,
            |s| s.stem_gamma = s.stem_gamma.nudges()[0],
            |s| {
                s.variations
                    .push(SemanticVariation::new(0x7767_6874, 500.0))
            },
            |s| s.minimum_contrast = s.minimum_contrast.nudges()[0],
            |s| s.selection_foreground = Some(0x11_22_33),
            |s| s.selection_inactive ^= true,
            |s| {
                s.wallpaper = Some(SpecimenWallpaper {
                    rgba: Arc::from(vec![0_u8; 4]),
                    w: 1,
                    h: 1,
                    fp: 7,
                    dim: 0.5,
                    tint: false,
                });
            },
        ];
        edits
            .iter()
            .map(|edit| {
                let mut s = self.clone();
                edit(&mut s);
                s
            })
            .collect()
    }
}

/// One copy of `$p` per least change of each field of variant `$v`. The
/// pattern names every field (no `..`), so one left out does not compile.
macro_rules! nudged {
    ($p:expr, $v:ident($($f:ident),*)) => {{
        let p: &DrawPrim = $p;
        assert!(matches!(p, DrawPrim::$v { $($f: _),* }));
        let mut out = Vec::new();
        $(
            if let DrawPrim::$v { $f, .. } = p {
                for value in Nudge::nudges($f) {
                    let mut q = p.clone();
                    if let DrawPrim::$v { $f, .. } = &mut q {
                        *$f = value;
                    }
                    out.push((stringify!($f), q));
                }
            }
        )*
        out
    }};
}

fn nudged(p: &DrawPrim) -> Vec<(&'static str, DrawPrim)> {
    match p {
        DrawPrim::Panel { .. } => nudged!(p, Panel(x, y, w, h, radius, fill)),
        DrawPrim::Ring { .. } => {
            nudged!(
                p,
                Ring(cx, cy, r_outer, thickness, track, start, frac, fill)
            )
        }
        DrawPrim::Capsule { .. } => nudged!(p, Capsule(x, y, w, h, frac, fill, track)),
        DrawPrim::Dot { .. } => nudged!(p, Dot(cx, cy, r, color)),
        DrawPrim::Text { .. } => {
            nudged!(p, Text(x, baseline, s, px, color, weight, face))
        }
        DrawPrim::TerminalSpecimen { .. } => nudged!(p, TerminalSpecimen(x, y, spec)),
        DrawPrim::Stroke { .. } => {
            nudged!(p, Stroke(x, y, w, h, radius, width, color))
        }
        DrawPrim::Line { .. } => nudged!(p, Line(x1, y1, x2, y2, width, color)),
        DrawPrim::Arc { .. } => nudged!(p, Arc(cx, cy, r, width, from, to, color)),
        DrawPrim::Dashed { .. } => {
            nudged!(p, Dashed(x, y, w, h, width, on, off, color))
        }
        DrawPrim::HsvDisk { .. } => nudged!(p, HsvDisk(cx, cy, r, value)),
        DrawPrim::ClipPush { .. } => nudged!(p, ClipPush(x, y, w, h)),
        DrawPrim::FadePush { .. } => nudged!(p, FadePush(y, h, from, to)),
        DrawPrim::BandIcon { .. } => nudged!(p, BandIcon(x, y, size, icon, color)),
        DrawPrim::Image { .. } => nudged!(p, Image(x, y, w, h, art, alpha)),
        DrawPrim::ClipPop | DrawPrim::FadePop | DrawPrim::SubpixelPush | DrawPrim::SubpixelPop => {
            Vec::new()
        }
    }
}

#[test]
fn equal_prims_fingerprint_equally() {
    let (a, b) = (samples(), samples());
    for (a, b) in a.iter().zip(&b) {
        assert_eq!(fp(a), fp(b), "{a:?}");
    }
}

#[test]
fn every_variant_fingerprints_apart() {
    let fps: std::collections::BTreeSet<u64> = samples().iter().map(fp).collect();
    assert_eq!(fps.len(), samples().len());
}

#[test]
fn a_least_change_to_any_field_moves_the_fingerprint() {
    let mut missed = Vec::new();
    for p in samples() {
        let base = fp(&p);
        for (field, q) in nudged(&p) {
            if fp(&q) == base {
                missed.push(format!("{field} of {p:?} → {q:?}"));
            }
        }
    }
    assert!(
        missed.is_empty(),
        "changes the fingerprint misses: {missed:#?}"
    );
}
