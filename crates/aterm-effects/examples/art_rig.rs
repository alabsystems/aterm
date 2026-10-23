// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `art_rig` — the authoring rigs behind the parametric glyph rosters.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let art = Path::new(env!("CARGO_MANIFEST_DIR")).join("art");
    let done = match args.as_slice() {
        [rig] if rig == "dogs" => dogs::main(&art.join("dogs")),
        [rig, out] if rig == "pet" => pet::main(&PathBuf::from(out)),
        _ => Err("usage: art_rig dogs | pet <out-dir>".to_string()),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("art_rig: {why}");
            ExitCode::FAILURE
        }
    }
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// One decimal, a trailing `.0` dropped (`12.0` is `12`, `-0.04` is `-0`).
fn fmt(v: f64) -> String {
    let s = format!("{v:.1}");
    match s.strip_suffix(".0") {
        Some(whole) => whole.to_string(),
        None => s,
    }
}

/// Degrees to radians, as the rigs always computed it.
fn radians(deg: f64) -> f64 {
    deg * (std::f64::consts::PI / 180.0)
}

/// The Euclidean norm the rigs were measured with: the two-term case of
/// CPython 3.9's `math.hypot` (scaled by the larger magnitude, compensated
/// summation from 1.0), so a joint normal lands on the same last bit.
fn hypot(dx: f64, dy: f64) -> f64 {
    let v = [dx.abs(), dy.abs()];
    let max = v[0].max(v[1]);
    if max == 0.0 {
        return 0.0;
    }
    let (mut csum, mut frac) = (1.0f64, 0.0f64);
    for x in v {
        let x = x / max;
        let x = x * x;
        let old = csum;
        csum += x;
        frac += (old - csum) + x;
    }
    max * (csum - 1.0 + frac).sqrt()
}

/// [`hypot`], with a zero length read as one (a degenerate segment has no
/// direction to divide by).
fn length_or_one(dx: f64, dy: f64) -> f64 {
    let ln = hypot(dx, dy);
    if ln == 0.0 { 1.0 } else { ln }
}

/// One `[[layer]]` block of a glyph asset.
fn layer_block(out: &mut Vec<String>, role: &str, fill: &str, recolor: &str, paths: &[String]) {
    let quoted: Vec<String> = paths.iter().map(|p| format!("\"{p}\"")).collect();
    out.extend([
        String::new(),
        "[[layer]]".to_string(),
        format!("role = \"{role}\""),
        format!("ref_fill = \"{fill}\""),
        format!("recolor = \"{recolor}\""),
        format!("paths = [{}]", quoted.join(", ")),
    ]);
}

mod seg {
    use super::{fmt, radians};

    /// The circle-to-cubic constant of the dog and robot rigs.
    pub const K: f64 = 0.5522847498;

    /// One path segment: `M`, `L`, `C` or `Z`.
    #[derive(Clone, Copy, Debug)]
    pub enum Seg {
        M(f64, f64),
        L(f64, f64),
        C(f64, f64, f64, f64, f64, f64),
        Z,
    }

    pub type Path = Vec<Seg>;

    /// The path's `d` string.
    pub fn emit(path: &[Seg]) -> String {
        let nums = |cs: &[f64]| cs.iter().map(|&c| fmt(c)).collect::<Vec<_>>().join(" ");
        path.iter()
            .map(|seg| match *seg {
                Seg::M(x, y) => format!("M {}", nums(&[x, y])),
                Seg::L(x, y) => format!("L {}", nums(&[x, y])),
                Seg::C(a, b, c, d, e, f) => format!("C {}", nums(&[a, b, c, d, e, f])),
                Seg::Z => "Z".to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Every coordinate pair through `f`.
    pub fn apply(path: &[Seg], f: impl Fn(f64, f64) -> (f64, f64)) -> Path {
        path.iter()
            .map(|seg| match *seg {
                Seg::M(x, y) => {
                    let (x, y) = f(x, y);
                    Seg::M(x, y)
                }
                Seg::L(x, y) => {
                    let (x, y) = f(x, y);
                    Seg::L(x, y)
                }
                Seg::C(x1, y1, x2, y2, x, y) => {
                    let (x1, y1) = f(x1, y1);
                    let (x2, y2) = f(x2, y2);
                    let (x, y) = f(x, y);
                    Seg::C(x1, y1, x2, y2, x, y)
                }
                Seg::Z => Seg::Z,
            })
            .collect()
    }

    pub fn rotate(path: &[Seg], cx: f64, cy: f64, deg: f64) -> Path {
        let a = radians(deg);
        let (ca, sa) = (a.cos(), a.sin());
        apply(path, |x, y| {
            let (dx, dy) = (x - cx, y - cy);
            (cx + dx * ca - dy * sa, cy + dx * sa + dy * ca)
        })
    }

    /// A closed four-arc cubic ellipse.
    pub fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64) -> Path {
        let (kx, ky) = (rx * K, ry * K);
        vec![
            Seg::M(cx + rx, cy),
            Seg::C(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry),
            Seg::C(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy),
            Seg::C(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry),
            Seg::C(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy),
            Seg::Z,
        ]
    }

    /// A filled crescent smile arc, open at the top, `t` thick.
    pub fn smile(cx: f64, cy: f64, rx: f64, ry: f64, t: f64) -> Path {
        let (kx, ky) = (rx * K, ry * K);
        let (irx, iry) = (rx - t, (ry - t).max(1.0));
        let (ikx, iky) = (irx * K, iry * K);
        vec![
            Seg::M(cx - rx, cy),
            Seg::C(cx - rx, cy + ky, cx - kx, cy + ry, cx, cy + ry),
            Seg::C(cx + kx, cy + ry, cx + rx, cy + ky, cx + rx, cy),
            Seg::L(cx + irx, cy),
            Seg::C(cx + irx, cy + iky, cx + ikx, cy + iry, cx, cy + iry),
            Seg::C(cx - ikx, cy + iry, cx - irx, cy + iky, cx - irx, cy),
            Seg::Z,
        ]
    }

    /// A filled open smile: the lower half-ellipse.
    pub fn mouth_open(cx: f64, cy: f64, rx: f64, ry: f64) -> Path {
        let (kx, _ky) = (rx * K, ry * K);
        vec![
            Seg::M(cx - rx, cy),
            Seg::C(cx - kx, cy, cx - kx, cy + ry, cx, cy + ry),
            Seg::C(cx + kx, cy + ry, cx + kx, cy, cx + rx, cy),
            Seg::Z,
        ]
    }
}

mod dogs {

    use super::seg::{Path, Seg, apply, ellipse, emit, mouth_open, rotate, smile};
    use super::{fmt, layer_block, write};

    const VB_W: f64 = 160.0;
    const VB_H: f64 = 136.0;

    const INK: &str = "#2B2530";
    const COAT: &str = "#C8955C";
    const MUZZLE: &str = "#F7E9DE";
    const EYE: &str = "#241F29";
    const LIGHT: &str = "#FFFFFF";
    const NOSE: &str = "#2A2029";
    const MOUTH: &str = "#2A2029";
    const TONGUE: &str = "#E87E8E";
    const INNER_EAR: &str = "#EFA3AE";
    const BLUSH: &str = "#F2A9B4";
    const PATTERN: &str = "#8A6242";
    const PALE: &str = "#F3EBDF";
    const SPOT: &str = "#332C38";
    const WHITE_COAT: &str = "#F2EEE6";

    /// The outline margin grown around every coat part.
    const S: f64 = 4.0;

    /// Reflect about the vertical centreline.
    fn mirror(path: &[Seg]) -> Path {
        apply(path, |x, y| (VB_W - x, y))
    }

    /// An upright rounded-triangle ear: base centred at (cx, base_y), apex
    /// above by h, half-width w; `tip_round` softens the apex.
    fn teardrop(cx: f64, base_y: f64, w: f64, h: f64, tip_round: f64) -> Path {
        let (ax, ay) = (cx, base_y - h);
        let r = w * tip_round;
        vec![
            Seg::M(cx - w, base_y),
            Seg::C(cx - w, base_y - h * 0.55, ax - r, ay + h * 0.18, ax, ay),
            Seg::C(
                ax + r,
                ay + h * 0.18,
                cx + w,
                base_y - h * 0.55,
                cx + w,
                base_y,
            ),
            Seg::C(
                cx + w * 0.7,
                base_y + h * 0.10,
                cx - w * 0.7,
                base_y + h * 0.10,
                cx - w,
                base_y,
            ),
            Seg::Z,
        ]
    }

    /// One breed's drawlist under construction, painter order.
    struct Head {
        ident: &'static str,
        note: &'static str,
        eye_y: f64,
        layers: Vec<(&'static str, &'static str, &'static str, Vec<String>)>,
    }

    impl Head {
        fn layer(
            &mut self,
            role: &'static str,
            fill: &'static str,
            recolor: &'static str,
            paths: &[Path],
        ) {
            self.layers
                .push((role, fill, recolor, paths.iter().map(|p| emit(p)).collect()));
        }

        fn toml(&self) -> String {
            let anchor: f64 = format!("{:.2}", self.eye_y / VB_H)
                .parse()
                .unwrap_or(f64::NAN);
            let mut out = vec![
                format!("id = \"{}\"", self.ident),
                "kind = \"head\"".to_string(),
                format!("viewbox = [{}, {}]", VB_W as i64, VB_H as i64),
                format!(
                    "anchor = {{ eye_y = {}, center_x = 0.5, word_top = 1.0 }}",
                    fmt(anchor)
                ),
                String::new(),
                format!("# {}", self.note),
                "# Generated by examples/art_rig.rs dogs — edit the rig, not this file."
                    .to_string(),
            ];
            for (role, fill, recolor, paths) in &self.layers {
                layer_block(&mut out, role, fill, recolor, paths);
            }
            out.join("\n") + "\n"
        }
    }

    fn eyes(cy: f64, dx: f64) -> Vec<Path> {
        let (rx, ry) = (7.5, 9.0);
        vec![
            ellipse(80.0 - dx, cy, rx, ry),
            ellipse(80.0 + dx, cy, rx, ry),
        ]
    }

    fn glints(cy: f64, dx: f64) -> Vec<Path> {
        let r = 2.6;
        vec![
            ellipse(80.0 - dx - 2.2, cy - 3.0, r, r),
            ellipse(80.0 + dx - 2.2, cy - 3.0, r, r),
        ]
    }

    fn blush(cy: f64) -> Vec<Path> {
        let dx = 42.0;
        vec![
            ellipse(80.0 - dx, cy, 8.5, 5.5),
            ellipse(80.0 + dx, cy, 8.5, 5.5),
        ]
    }

    fn nose(cy: f64, rx: f64) -> Vec<Path> {
        vec![ellipse(80.0, cy, rx, 7.0)]
    }

    /// One breed's feature table; every field but the first five has the
    /// default [`Breed::new`] gives it.
    struct Breed {
        id: &'static str,
        note: &'static str,
        /// Margin to the LEFT ear's paths; the rig mirrors them.
        ears: fn(f64) -> Vec<Path>,
        mouth: &'static str,
        inner_ear: Option<Vec<Path>>,
        /// Margin to unmirrored coat extras (the poodle's topknot).
        extra_coat: Option<fn(f64) -> Vec<Path>>,
        patterns: Vec<(&'static str, Vec<Path>)>,
        coat: (&'static str, &'static str),
        head_cy: f64,
        head_rx: f64,
        head_ry: f64,
        eye_cy: f64,
        eye_dx: f64,
        muzzle: (f64, f64, f64, f64),
        muzzle_fill: &'static str,
        blush_cy: f64,
        nose_cy: f64,
        nose_rx: f64,
        mouth_cy: f64,
    }

    impl Breed {
        fn new(
            id: &'static str,
            note: &'static str,
            ears: fn(f64) -> Vec<Path>,
            mouth: &'static str,
        ) -> Self {
            Self {
                id,
                note,
                ears,
                mouth,
                inner_ear: None,
                extra_coat: None,
                patterns: Vec::new(),
                coat: (COAT, "coat"),
                head_cy: 76.0,
                head_rx: 55.0,
                head_ry: 52.0,
                eye_cy: 70.0,
                eye_dx: 27.0,
                muzzle: (80.0, 98.0, 30.0, 22.0),
                muzzle_fill: MUZZLE,
                blush_cy: 84.0,
                nose_cy: 88.0,
                nose_rx: 10.0,
                mouth_cy: 100.0,
            }
        }
    }

    /// Assemble one breed from its feature table.
    fn build(b: &Breed) -> Head {
        let mut h = Head {
            ident: b.id,
            note: b.note,
            eye_y: b.eye_cy,
            layers: Vec::new(),
        };
        let (hx, hy) = (80.0, b.head_cy);
        let head_shape = |m: f64| ellipse(hx, hy, b.head_rx + m, b.head_ry + m);
        let ear_paths = |m: f64| {
            let left = (b.ears)(m);
            let mut all: Vec<Path> = left.clone();
            all.extend(left.iter().map(|p| mirror(p)));
            all.extend(b.extra_coat.map_or_else(Vec::new, |f| f(m)));
            all
        };
        let mut outline = ear_paths(S);
        outline.push(head_shape(S));
        h.layer("outline", INK, "fixed", &outline);
        let mut coat = ear_paths(0.0);
        coat.push(head_shape(0.0));
        h.layer("coat", b.coat.0, b.coat.1, &coat);
        if let Some(inner) = &b.inner_ear {
            h.layer("inner_ear", INNER_EAR, "fixed", inner);
        }
        for &(fill, ref paths) in &b.patterns {
            h.layer("pattern", fill, "fixed", paths);
        }
        let (mx, my, mrx, mry) = b.muzzle;
        h.layer(
            "muzzle",
            b.muzzle_fill,
            "fixed",
            &[ellipse(mx, my, mrx, mry)],
        );
        h.layer("blush", BLUSH, "fixed", &blush(b.blush_cy));
        h.layer("eye", EYE, "fixed", &eyes(b.eye_cy, b.eye_dx));
        h.layer("detail", LIGHT, "fixed", &glints(b.eye_cy, b.eye_dx));
        h.layer("nose", NOSE, "fixed", &nose(b.nose_cy, b.nose_rx));
        let mcy = b.mouth_cy;
        match b.mouth {
            "tongue" => {
                h.layer("mouth", MOUTH, "fixed", &[mouth_open(80.0, mcy, 13.0, 9.0)]);
                h.layer(
                    "detail",
                    TONGUE,
                    "fixed",
                    &[ellipse(80.0, mcy + 7.5, 7.5, 9.5)],
                );
            }
            "smile" => {
                h.layer(
                    "mouth",
                    MOUTH,
                    "fixed",
                    &[smile(80.0, mcy - 2.0, 13.0, 8.0, 3.0)],
                );
            }
            "jowl" => {
                h.layer(
                    "mouth",
                    MOUTH,
                    "fixed",
                    &[
                        smile(64.0, mcy, 11.0, 8.0, 3.0),
                        smile(96.0, mcy, 11.0, 8.0, 3.0),
                    ],
                );
                h.layer(
                    "detail",
                    LIGHT,
                    "fixed",
                    &[
                        ellipse(72.0, mcy + 5.0, 3.5, 5.0),
                        ellipse(88.0, mcy + 5.0, 3.5, 5.0),
                    ],
                );
            }
            "pug" => {
                h.layer(
                    "detail",
                    TONGUE,
                    "fixed",
                    &[ellipse(80.0, mcy + 4.0, 6.0, 7.5)],
                );
            }
            _ => {}
        }
        h
    }

    fn drop_ear_at(m: f64, cx: f64, cy: f64, rx: f64, ry: f64, tilt: f64) -> Vec<Path> {
        vec![rotate(
            &ellipse(cx - m * 0.3, cy, rx + m, ry + m),
            cx,
            cy,
            tilt,
        )]
    }

    fn drop_ear(m: f64) -> Vec<Path> {
        drop_ear_at(m, 26.0, 64.0, 16.0, 38.0, 22.0)
    }

    fn long_drop_ear(m: f64) -> Vec<Path> {
        drop_ear_at(m, 23.0, 76.0, 15.0, 46.0, 18.0)
    }

    fn prick_ear_at(m: f64, cx: f64, base_y: f64, w: f64, hh: f64, tilt: f64) -> Vec<Path> {
        vec![rotate(
            &teardrop(cx, base_y, w + m, hh + m * 1.5, 0.55),
            cx,
            base_y - hh / 2.0,
            tilt,
        )]
    }

    fn prick_ear(m: f64) -> Vec<Path> {
        prick_ear_at(m, 42.0, 48.0, 21.0, 44.0, -14.0)
    }

    fn big_prick_ear(m: f64) -> Vec<Path> {
        prick_ear_at(m, 40.0, 50.0, 25.0, 50.0, -17.0)
    }

    fn fold_ear(m: f64) -> Vec<Path> {
        let (cx, cy, rx, ry, tilt) = (31.0, 40.0, 10.0, 17.0, -38.0);
        vec![rotate(&ellipse(cx, cy, rx + m, ry + m), cx, cy, tilt)]
    }

    fn pom_ear(m: f64) -> Vec<Path> {
        vec![ellipse(28.0, 66.0, 17.0 + m, 24.0 + m)]
    }

    /// The inner ear of a prick ear, both sides; it pivots where the default
    /// prick ear does (`44 / 2` above its base) whatever its own height.
    fn inner_prick(cx: f64, base_y: f64, w: f64, hh: f64, tilt: f64) -> Vec<Path> {
        let left = rotate(
            &teardrop(cx, base_y, w, hh, 0.55),
            cx,
            base_y - 44.0 / 2.0,
            tilt,
        );
        let right = mirror(&left);
        vec![left, right]
    }

    /// The poodle's cloud topknot.
    fn topknot(m: f64) -> Vec<Path> {
        vec![
            ellipse(58.0, 26.0, 17.0 + m, 14.0 + m),
            ellipse(80.0, 20.0, 19.0 + m, 15.0 + m),
            ellipse(102.0, 26.0, 17.0 + m, 14.0 + m),
        ]
    }

    fn breeds() -> Vec<Breed> {
        vec![
            Breed::new(
                "d1_retriever",
                "Golden retriever: soft drop ears, tongue-out grin.",
                drop_ear,
                "tongue",
            ),
            Breed {
                inner_ear: Some(inner_prick(42.0, 46.0, 11.0, 28.0, -14.0)),
                patterns: vec![(
                    PALE,
                    vec![
                        ellipse(80.0, 92.0, 34.0, 30.0),
                        ellipse(62.0, 66.0, 12.0, 9.0),
                        ellipse(98.0, 66.0, 12.0, 9.0),
                    ],
                )],
                ..Breed::new(
                    "d1_husky",
                    "Husky: tall prick ears, pale mask, knowing smile.",
                    prick_ear,
                    "smile",
                )
            },
            Breed {
                inner_ear: Some(inner_prick(40.0, 48.0, 13.0, 32.0, -17.0)),
                patterns: vec![(
                    PALE,
                    vec![
                        ellipse(80.0, 100.0, 26.0, 26.0),
                        ellipse(80.0, 62.0, 7.0, 26.0),
                    ],
                )],
                ..Breed::new(
                    "d1_corgi",
                    "Corgi: enormous rounded prick ears, pale blaze and cheeks.",
                    big_prick_ear,
                    "tongue",
                )
            },
            Breed {
                head_ry: 50.0,
                patterns: vec![(PATTERN, vec![ellipse(80.0, 44.0, 16.0, 6.0)])],
                muzzle: (80.0, 98.0, 27.0, 21.0),
                muzzle_fill: "#4A4048",
                eye_dx: 29.0,
                mouth_cy: 102.0,
                ..Breed::new(
                    "d1_pug",
                    "Pug: folded flap ears, the dark mask muzzle, big worried eyes.",
                    fold_ear,
                    "pug",
                )
            },
            Breed {
                patterns: vec![
                    (PATTERN, vec![ellipse(80.0, 34.0, 34.0, 12.0)]),
                    (
                        PALE,
                        vec![
                            ellipse(80.0, 96.0, 24.0, 24.0),
                            ellipse(80.0, 52.0, 9.0, 34.0),
                        ],
                    ),
                ],
                ..Breed::new(
                    "d1_beagle",
                    "Beagle: long velvet drop ears, pale blaze up the brow.",
                    long_drop_ear,
                    "tongue",
                )
            },
            Breed {
                coat: (WHITE_COAT, "fixed"),
                patterns: vec![(
                    SPOT,
                    vec![
                        ellipse(52.0, 44.0, 6.5, 5.5),
                        ellipse(104.0, 38.0, 5.5, 4.5),
                        ellipse(118.0, 60.0, 6.0, 5.0),
                        ellipse(44.0, 92.0, 5.0, 4.5),
                        ellipse(96.0, 116.0, 5.5, 4.5),
                        ellipse(64.0, 24.0, 4.5, 4.0),
                        ellipse(30.0, 62.0, 7.0, 9.0),
                        ellipse(130.0, 74.0, 7.0, 9.0),
                    ],
                )],
                ..Breed::new(
                    "d1_dalmatian",
                    "Dalmatian: fixed white coat, ink spots, spotted drop ears.",
                    drop_ear,
                    "tongue",
                )
            },
            Breed {
                inner_ear: Some(inner_prick(42.0, 46.0, 11.0, 28.0, -14.0)),
                patterns: vec![
                    (SPOT, vec![ellipse(80.0, 40.0, 40.0, 16.0)]),
                    (
                        PATTERN,
                        vec![
                            ellipse(57.0, 56.0, 8.0, 4.5),
                            ellipse(103.0, 56.0, 8.0, 4.5),
                        ],
                    ),
                ],
                muzzle: (80.0, 100.0, 32.0, 24.0),
                ..Breed::new(
                    "d1_shepherd",
                    "German shepherd: alert prick ears, tan brows on a saddle brow band.",
                    prick_ear,
                    "tongue",
                )
            },
            Breed {
                extra_coat: Some(topknot),
                ..Breed::new(
                    "d1_poodle",
                    "Poodle: cloud topknot and pom ears.",
                    pom_ear,
                    "smile",
                )
            },
            Breed {
                head_ry: 48.0,
                muzzle: (80.0, 100.0, 34.0, 25.0),
                nose_cy: 90.0,
                nose_rx: 12.0,
                mouth_cy: 103.0,
                ..Breed::new(
                    "d1_dachshund",
                    "Dachshund: the long face, longer ears, biggest nose.",
                    long_drop_ear,
                    "tongue",
                )
            },
            Breed {
                head_rx: 62.0,
                head_ry: 48.0,
                head_cy: 78.0,
                muzzle: (80.0, 100.0, 34.0, 22.0),
                eye_dx: 31.0,
                mouth_cy: 98.0,
                ..Breed::new(
                    "d1_bulldog",
                    "Bulldog: wide jowly head, folded ears, proud underbite.",
                    fold_ear,
                    "jowl",
                )
            },
        ]
    }

    pub fn main(dir: &std::path::Path) -> Result<(), String> {
        for breed in breeds() {
            let name = format!("{}.toml", breed.id);
            write(&dir.join(&name), &build(&breed).toml())?;
            println!("wrote {name}");
        }
        Ok(())
    }
}

mod pet {
    pub use poses::main;

    mod rig {

        use super::super::{fmt, length_or_one, radians};

        pub const VB_W: f64 = 244.0;
        pub const VB_H: f64 = 148.0;
        /// Where the lowest INK of a planted pose lands in the viewbox.
        pub const GROUND_INK: f64 = 143.0;
        /// The rig -> viewbox offset, sized by the widest pose (the leap).
        pub const OX: f64 = 34.0;
        /// The circle -> cubic constant of this rig.
        const K: f64 = 0.5523;
        /// Catmull-Rom's default tension.
        pub const TENSION: f64 = 1.0 / 6.0;

        pub type Pt = (f64, f64);

        /// The coordinates, space-separated.
        pub fn nums(ns: &[f64]) -> String {
            ns.iter().map(|&n| fmt(n)).collect::<Vec<_>>().join(" ")
        }

        fn pts(ps: &[Pt]) -> String {
            nums(&ps.iter().flat_map(|&(x, y)| [x, y]).collect::<Vec<_>>())
        }

        /// A closed ellipse as four cubics, rotated `rot` radians about its centre.
        pub fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64, rot: f64) -> String {
            let (c, s) = (rot.cos(), rot.sin());
            let t = |(x, y): Pt| {
                let (x, y) = (x - cx, y - cy);
                (cx + x * c - y * s, cy + x * s + y * c)
            };
            let (kx, ky) = (rx * K, ry * K);
            let p: Vec<Pt> = [
                (cx, cy - ry),
                (cx + kx, cy - ry),
                (cx + rx, cy - ky),
                (cx + rx, cy),
                (cx + rx, cy + ky),
                (cx + kx, cy + ry),
                (cx, cy + ry),
                (cx - kx, cy + ry),
                (cx - rx, cy + ky),
                (cx - rx, cy),
                (cx - rx, cy - ky),
                (cx - kx, cy - ry),
                (cx, cy - ry),
            ]
            .into_iter()
            .map(t)
            .collect();
            let mut d = vec![format!("M {}", pts(&p[..1]))];
            for i in (1..13).step_by(3) {
                d.push(format!("C {}", pts(&p[i..i + 3])));
            }
            d.push("Z".to_string());
            d.join(" ")
        }

        /// A closed smooth shape through `spokes` = [(angle_deg, radius)], angles clockwise from 12 o'clock in screen space (y down), as Catmull-Rom through the spoke tips, one cubic per span.
        pub fn blob(cx: f64, cy: f64, spokes: &[Pt], rot: f64, sx: f64, sy: f64) -> String {
            let tips: Vec<Pt> = spokes
                .iter()
                .map(|&(a, r)| {
                    let th = radians(a - 90.0);
                    let x = th.cos() * r * sx;
                    let y = th.sin() * r * sy;
                    let (ca, sa) = (rot.cos(), rot.sin());
                    (cx + x * ca - y * sa, cy + x * sa + y * ca)
                })
                .collect();
            catmull_closed(&tips, TENSION)
        }

        /// [`blob`] unrotated and unscaled.
        pub fn blob0(cx: f64, cy: f64, spokes: &[Pt]) -> String {
            blob(cx, cy, spokes, 0.0, 1.0, 1.0)
        }

        /// The two Catmull-Rom control points of the span p1 -> p2.
        fn controls(p0: Pt, p1: Pt, p2: Pt, p3: Pt, tension: f64) -> [Pt; 2] {
            [
                (
                    p1.0 + (p2.0 - p0.0) * tension,
                    p1.1 + (p2.1 - p0.1) * tension,
                ),
                (
                    p2.0 - (p3.0 - p1.0) * tension,
                    p2.1 - (p3.1 - p1.1) * tension,
                ),
            ]
        }

        /// A closed Catmull-Rom curve through `ps`, as cubics.
        pub fn catmull_closed(ps: &[Pt], tension: f64) -> String {
            let n = ps.len();
            let mut d = vec![format!("M {}", pts(&ps[..1]))];
            for i in 0..n {
                let [c1, c2] = controls(
                    ps[(i + n - 1) % n],
                    ps[i],
                    ps[(i + 1) % n],
                    ps[(i + 2) % n],
                    tension,
                );
                d.push(format!("C {}", pts(&[c1, c2, ps[(i + 1) % n]])));
            }
            d.push("Z".to_string());
            d.join(" ")
        }

        /// An open Catmull-Rom polyline, as cubic command strings.
        fn catmull_open(ps: &[Pt], tension: f64) -> Vec<String> {
            let n = ps.len();
            (0..n.saturating_sub(1))
                .map(|i| {
                    let [c1, c2] = controls(
                        ps[i.saturating_sub(1)],
                        ps[i],
                        ps[i + 1],
                        ps[(i + 2).min(n - 1)],
                        tension,
                    );
                    format!("C {}", pts(&[c1, c2, ps[i + 1]]))
                })
                .collect()
        }

        /// The unit normal at each joint of a polyline, averaged across the
        /// joint, with a miter compensation so a bend does not pinch.
        fn normals(ps: &[Pt]) -> Vec<Pt> {
            let n = ps.len();
            let segn: Vec<Pt> = ps
                .windows(2)
                .map(|w| {
                    let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
                    let ln = length_or_one(dx, dy);
                    (-dy / ln, dx / ln)
                })
                .collect();
            (0..n)
                .map(|i| {
                    if i == 0 {
                        segn[0]
                    } else if i == n - 1 {
                        segn[segn.len() - 1]
                    } else {
                        let (ax, ay) = segn[i - 1];
                        let (bx, by) = segn[i];
                        let (nx, ny) = (ax + bx, ay + by);
                        let ln = length_or_one(nx, ny);
                        let (nx, ny) = (nx / ln, ny / ln);
                        let cosb = ax * nx + ay * ny;
                        if cosb > 0.25 {
                            (nx / cosb, ny / cosb)
                        } else {
                            (nx, ny)
                        }
                    }
                })
                .collect()
        }

        /// A closed tapered stroke around the polyline `ps`, with per-joint `radii`.
        pub fn limb(ps: &[Pt], radii: &[f64], smooth: bool) -> String {
            let nrm = normals(ps);
            let rails = |sign: f64| -> Vec<Pt> {
                ps.iter()
                    .zip(&nrm)
                    .zip(radii)
                    .map(|((p, (nx, ny)), r)| {
                        if sign > 0.0 {
                            (p.0 + nx * r, p.1 + ny * r)
                        } else {
                            (p.0 - nx * r, p.1 - ny * r)
                        }
                    })
                    .collect()
            };
            let (left, right) = (rails(1.0), rails(-1.0));
            let last = left.len() - 1;
            let (r_first, r_last) = (radii[0], radii[radii.len() - 1]);
            let mut d = vec![format!("M {}", pts(&left[..1]))];
            if smooth {
                d.extend(catmull_open(&left, TENSION));
            } else {
                d.extend(left[1..].iter().map(|p| format!("L {}", pts(&[*p]))));
            }
            d.push(cap(
                left[last],
                right[last],
                r_last,
                dir(ps[ps.len() - 2], ps[ps.len() - 1]),
            ));
            let rev: Vec<Pt> = right.iter().rev().copied().collect();
            if smooth {
                d.extend(catmull_open(&rev, TENSION));
            } else {
                d.extend(rev[1..].iter().map(|p| format!("L {}", pts(&[*p]))));
            }
            d.push(cap(right[0], left[0], r_first, dir(ps[1], ps[0])));
            d.push("Z".to_string());
            d.join(" ")
        }

        /// The unit vector `from` -> `to`: the OUTWARD direction a cap bulges in.
        fn dir(from: Pt, to: Pt) -> Pt {
            let (dx, dy) = (to.0 - from.0, to.1 - from.1);
            let ln = length_or_one(dx, dy);
            (dx / ln, dy / ln)
        }

        /// A semicircular cap from rail point `a` to rail point `b`, bulging
        /// `r` along the unit vector `out`.
        fn cap(a: Pt, b: Pt, r: f64, out: Pt) -> String {
            let k = 1.3333 * r;
            let c1 = (a.0 + out.0 * k, a.1 + out.1 * k);
            let c2 = (b.0 + out.0 * k, b.1 + out.1 * k);
            format!("C {}", pts(&[c1, c2, b]))
        }

        /// (px, py) rotated `deg` about (cx, cy).
        pub fn rot_about(px: f64, py: f64, cx: f64, cy: f64, deg: f64) -> Pt {
            let th = radians(deg);
            let (c, s) = (th.cos(), th.sin());
            let (x, y) = (px - cx, py - cy);
            (cx + x * c - y * s, cy + x * s + y * c)
        }

        /// A forward-kinematic chain: `bones` = [(length, absolute_angle_deg)], 0 degrees pointing DOWN the screen, positive rotating toward +x.
        pub fn chain(root: Pt, bones: &[Pt]) -> Vec<Pt> {
            let mut out = vec![root];
            let (mut x, mut y) = root;
            for &(ln, ang) in bones {
                let th = radians(ang);
                x += th.sin() * ln;
                y += th.cos() * ln;
                out.push((x, y));
            }
            out
        }
    }

    mod model {

        use super::super::{fmt, radians};
        use super::rig::{
            GROUND_INK, OX, Pt, VB_H, VB_W, blob, blob0, catmull_closed, chain, ellipse, limb,
            rot_about,
        };

        pub const INK: &str = "#2B2530";
        const COAT: &str = "#D9A273";
        const PATTERN: &str = "#A9754C";
        const INNER_EAR: &str = "#EFA3AE";
        const MUZZLE: &str = "#F7E9DE";
        const EYE: &str = "#241F29";
        const IRIS: &str = "#7FA88E";
        const PUPIL: &str = "#1A161E";
        const LIGHT: &str = "#FFFFFF";
        const NOSE: &str = "#B0637A";
        const BLUSH: &str = "#F2A9B4";
        /// A dog's nose is a dark button, not a cat's pink triangle.
        const DOG_NOSE: &str = "#2B2530";

        /// The outline half-width added to every coat part.
        const S: f64 = 4.0;

        /// A leg: (thigh_angle, shin_angle, thigh_len, shin_len).
        pub type Leg = (f64, f64, f64, f64);

        /// The seated rump: a circle (cx, cy, r), or an ellipse (cx, cy, rx, ry, rot_deg) for a pose that needs the hip mass ORIENTED (the peek's rump swings away from the viewer, and a circle has no away to swing).
        #[derive(Clone, Copy, Debug)]
        pub enum Haunch {
            Circle(f64, f64, f64),
            Ellipse(f64, f64, f64, f64, f64),
        }

        /// One authored frame of the pet.
        #[derive(Clone, Debug)]
        pub struct Pose {
            pub ident: String,
            pub note: String,
            /// Which animal wears this pose: "cat" reproduces the original rig; "dog" swaps the head skin.
            pub species: &'static str,
            pub bx: f64,
            pub by: f64,
            pub brx: f64,
            pub bry: f64,
            pub brot: f64,
            pub hx: f64,
            pub hy: f64,
            pub hr: f64,
            pub hrot: f64,
            pub hsx: f64,
            pub hsy: f64,
            /// The ears' sweep away from vertical (near, far); negative flattens.
            pub ear_near: f64,
            pub ear_far: f64,
            /// 0 = perked, 1 = pinned back.
            pub ear_flat: f64,
            pub fl_near: Leg,
            pub fl_far: Leg,
            pub hl_near: Leg,
            pub hl_far: Leg,
            /// The absolute angles of a four-bone chain from the rump, and its taper.
            pub tail: [f64; 4],
            pub tail_len: f64,
            pub tail_thick: f64,
            /// open | happy | closed | squint | halflid | wide | wink
            pub eyes: &'static str,
            /// smile | open | flat | oof | yawn | none
            pub mouth: &'static str,
            pub blush: bool,
            pub gaze: Pt,
            /// The whole-body curl (sleep) replaces torso and legs.
            pub curl: bool,
            pub show_far_legs: bool,
            /// The curl tucks every paw out of sight.
            pub hide_legs: bool,
            /// A sit puts the haunch on the ground instead.
            pub hide_hind: bool,
            pub haunch_at: Option<Haunch>,
            /// Vertical registration.
            pub airborne: f64,
            /// The forelimb's attach point override.
            pub fl_root: Option<Pt>,
            /// The tail's attach point override.
            pub tail_root: Option<Pt>,
            /// Face-on mode: the whole drawing turns to address the viewer.
            pub front: bool,
            /// The off-side whisker fan.
            pub whisker_far: bool,
            /// The NEAR whisker fan.
            pub whisker_near: bool,
            /// Where the tabby back bars ride: "torso" (the topline), or "haunch" for a pose whose head covers the torso's top (the peek's over-the-shoulder seat).
            pub bar_site: &'static str,
            /// Head yaw: 0 = the rest read (a nearly frontal face, eye contact, the point of a settled pose), 1 = the full 3/4 locomotion head: the far eye foreshortened and pulled toward the leading edge, the muzzle pushed into the travel direction, the near ear rotated out while the far ear narrows behind, the whiskers asymmetric, the skull egg-shaped toward the muzzle.
            pub yaw: f64,
            /// (dx, dy, sx, sy) for the FAR eye only.
            pub far_eye: Option<[f64; 4]>,
            /// (cx, cy, rx, ry) of a coat blob painted with the coat, under the face: `pet_bat`'s raised hind leg encloses a counter that would show the terminal background THROUGH the animal.
            pub belly: Option<[f64; 4]>,
        }

        impl Default for Pose {
            fn default() -> Self {
                Self {
                    ident: String::new(),
                    note: String::new(),
                    species: "cat",
                    bx: 90.0,
                    by: 64.0,
                    brx: 40.0,
                    bry: 23.0,
                    brot: 0.0,
                    hx: 150.0,
                    hy: 41.0,
                    hr: 29.0,
                    hrot: 0.0,
                    hsx: 1.0,
                    hsy: 1.0,
                    ear_near: 0.0,
                    ear_far: 0.0,
                    ear_flat: 0.0,
                    fl_near: (0.0, 0.0, 22.0, 16.0),
                    fl_far: (0.0, 0.0, 22.0, 16.0),
                    hl_near: (0.0, 0.0, 22.0, 16.0),
                    hl_far: (0.0, 0.0, 22.0, 16.0),
                    tail: [-112.0, -140.0, -172.0, -206.0],
                    tail_len: 13.5,
                    tail_thick: 8.5,
                    eyes: "open",
                    mouth: "smile",
                    blush: true,
                    gaze: (0.0, 0.0),
                    curl: false,
                    show_far_legs: true,
                    hide_legs: false,
                    hide_hind: false,
                    haunch_at: None,
                    airborne: 0.0,
                    fl_root: None,
                    tail_root: None,
                    front: false,
                    whisker_far: true,
                    whisker_near: true,
                    bar_site: "torso",
                    yaw: 0.0,
                    far_eye: None,
                    belly: None,
                }
            }
        }

        pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
            a + (b - a) * t
        }

        fn dog(p: &Pose) -> bool {
            p.species == "dog"
        }

        /// The off-side pair sits back and up: an instant 3/4 read.
        const FAR_OFFSET: Pt = (-7.0, -2.0);
        /// Face-on, the off-side foreleg mirrors across the chest (24 units
        /// apart: any closer and the two mitts' outlines fuse into one paw).
        const FRONT_OFFSET: Pt = (-24.0, 0.0);

        fn hip(p: &Pose, far: bool) -> Pt {
            let (dx, dy) = if far { FAR_OFFSET } else { (0.0, 0.0) };
            rot_about(
                p.bx - p.brx * 0.60 + dx,
                p.by + p.bry * 0.50 + dy,
                p.bx,
                p.by,
                p.brot,
            )
        }

        fn shoulder(p: &Pose, far: bool) -> Pt {
            let (dx, dy) = match (far, p.front) {
                (false, _) => (0.0, 0.0),
                (true, true) => FRONT_OFFSET,
                (true, false) => FAR_OFFSET,
            };
            if let Some((x, y)) = p.fl_root {
                return (x + dx, y + dy);
            }
            rot_about(
                p.bx + p.brx * 0.62 + dx,
                p.by + p.bry * 0.48 + dy,
                p.bx,
                p.by,
                p.brot,
            )
        }

        fn rump(p: &Pose) -> Pt {
            if let Some(root) = p.tail_root {
                return root;
            }
            rot_about(p.bx - p.brx * 1.00, p.by - p.bry * 0.20, p.bx, p.by, p.brot)
        }

        fn leg_pts(root: Pt, spec: Leg) -> Vec<Pt> {
            let (thigh_a, shin_a, thigh_l, shin_l) = spec;
            chain(root, &[(thigh_l, thigh_a), (shin_l, shin_a)])
        }

        /// Each part builder returns (coat paths, outline paths).
        type Part = (Vec<String>, Vec<String>);

        /// `spokes` with each radius scaled by `k`.
        fn scaled(spokes: &[Pt], k: f64) -> Vec<Pt> {
            spokes.iter().map(|&(a, r)| (a, r * k)).collect()
        }

        fn torso_paths(p: &Pose) -> Part {
            let spokes: &[Pt] = if p.curl {
                &[
                    (0.0, 1.02),
                    (38.0, 1.04),
                    (72.0, 0.98),
                    (108.0, 0.90),
                    (145.0, 0.84),
                    (180.0, 0.80),
                    (215.0, 0.86),
                    (250.0, 0.96),
                    (288.0, 1.06),
                    (324.0, 1.05),
                ]
            } else if p.front {
                &[
                    (0.0, 0.96),
                    (38.0, 1.00),
                    (72.0, 1.04),
                    (106.0, 1.08),
                    (142.0, 0.98),
                    (180.0, 0.92),
                    (218.0, 0.98),
                    (254.0, 1.08),
                    (288.0, 1.04),
                    (322.0, 1.00),
                ]
            } else {
                &[
                    (0.0, 1.00),
                    (42.0, 1.10),
                    (78.0, 1.02),
                    (112.0, 0.92),
                    (150.0, 0.85),
                    (180.0, 0.84),
                    (212.0, 0.90),
                    (250.0, 1.00),
                    (285.0, 1.05),
                    (322.0, 1.03),
                ]
            };
            let rot = radians(p.brot);
            let core = blob(p.bx, p.by, &scaled(spokes, p.brx), rot, 1.0, p.bry / p.brx);
            let out = blob(
                p.bx,
                p.by,
                &scaled(spokes, p.brx + S),
                rot,
                1.0,
                (p.bry + S) / (p.brx + S),
            );
            (vec![core], vec![out])
        }

        fn haunch_paths(p: &Pose) -> Part {
            let (cx, cy, rx, ry) = match p.haunch_at {
                Some(Haunch::Ellipse(cx, cy, rx, ry, rot)) => {
                    let th = radians(rot);
                    return (
                        vec![ellipse(cx, cy, rx, ry, th)],
                        vec![ellipse(cx, cy, rx + S, ry + S, th)],
                    );
                }
                Some(Haunch::Circle(cx, cy, r)) => (cx, cy, r * 1.02, r),
                None => {
                    let (hx, hy) = hip(p, false);
                    let r = p.bry * 0.86;
                    (hx - 2.0, hy - p.bry * 0.42, r * 1.05, r)
                }
            };
            (
                vec![ellipse(cx, cy, rx, ry, 0.0)],
                vec![ellipse(cx, cy, rx + S, ry + S, 0.0)],
            )
        }

        fn head_spokes(p: &Pose) -> Vec<Pt> {
            if p.front {
                return vec![
                    (0.0, 0.96),
                    (36.0, 1.00),
                    (72.0, 1.03),
                    (108.0, 1.00),
                    (144.0, 0.92),
                    (180.0, 0.88),
                    (216.0, 0.92),
                    (252.0, 1.00),
                    (288.0, 1.03),
                    (324.0, 1.00),
                ];
            }
            let rest = [
                (0.0, 0.94),
                (35.0, 0.99),
                (70.0, 1.03),
                (100.0, 1.02),
                (135.0, 0.94),
                (168.0, 0.88),
                (200.0, 0.94),
                (232.0, 1.02),
                (262.0, 1.03),
                (295.0, 1.00),
                (325.0, 0.96),
            ];
            let yawed = [
                1.03, 1.05, 1.03, 1.00, 0.90, 0.85, 0.92, 1.01, 1.03, 1.00, 1.01,
            ];
            rest.iter()
                .zip(yawed)
                .map(|(&(a, r0), r1)| (a, lerp(r0, r1, p.yaw)))
                .collect()
        }

        fn head_paths(p: &Pose) -> Part {
            let sp = head_spokes(p);
            let rot = radians(p.hrot);
            let core = blob(p.hx, p.hy, &scaled(&sp, p.hr), rot, p.hsx, p.hsy);
            let out = blob(p.hx, p.hy, &scaled(&sp, p.hr + S), rot, p.hsx, p.hsy);
            (vec![core], vec![out])
        }

        /// The cat's ear: `side` = +1 near (toward +x), -1 far.
        fn ear_tri(p: &Pose, side: f64, inset: f64) -> String {
            let flat_scale = 1.0;
            let near = side > 0.0;
            let mut base_a = if p.front {
                // face-on: the pair sits symmetric
                36.0 * side
            } else {
                30.0 * side + if near { 14.0 } else { -6.0 }
            };
            if !p.front {
                // yawed, the near ear rotates OUT with the turning skull and
                base_a += (if near { 4.0 } else { -6.0 }) * p.yaw;
            }
            let sweep = if near { p.ear_near } else { p.ear_far };
            let pin = p.ear_flat;
            let th = radians(base_a - 90.0 + p.hrot);
            let bx = p.hx + th.cos() * (p.hr - 4.0) * p.hsx;
            let by = p.hy + th.sin() * (p.hr - 4.0) * p.hsy;
            let ear_k = if p.front || near {
                1.0
            } else {
                lerp(1.0, 0.80, p.yaw)
            };
            let w = (14.0 - inset * 0.55) * flat_scale * ear_k;
            let h = (20.5 - inset)
                * flat_scale
                * if ear_k == 1.0 {
                    1.0
                } else {
                    lerp(1.0, 0.92, p.yaw)
                };
            let tip_a = base_a + sweep + pin * if near { -62.0 } else { -50.0 };
            let tth = radians(tip_a - 90.0 + p.hrot);
            let tx = bx + tth.cos() * h;
            let ty = by + tth.sin() * h;
            let (px, py) = (-tth.sin(), tth.cos());
            let a = (bx + px * w * 0.5, by + py * w * 0.5);
            let b = (bx - px * w * 0.5, by - py * w * 0.5);
            let mid_a = ((a.0 + tx) * 0.5 + px * 1.5, (a.1 + ty) * 0.5 + py * 1.5);
            let mid_b = ((b.0 + tx) * 0.5 - px * 1.5, (b.1 + ty) * 0.5 - py * 1.5);
            catmull_closed(&[a, mid_a, (tx, ty), mid_b, b], 0.10)
        }

        /// The DOG ear: a long rounded lobe HANGING down the side of the skull.
        fn ear_drop(p: &Pose, side: f64, inset: f64) -> String {
            let flat_scale = 1.0;
            let near = side > 0.0;
            let mut base_a = if p.front {
                62.0 * side
            } else {
                58.0 * side + if near { 10.0 } else { -4.0 }
            };
            if !p.front {
                base_a += (if near { 4.0 } else { -6.0 }) * p.yaw;
            }
            let sweep = if near { p.ear_near } else { p.ear_far };
            let th = radians(base_a - 90.0 + p.hrot);
            let bx = p.hx + th.cos() * (p.hr - 6.0) * p.hsx;
            let by = p.hy + th.sin() * (p.hr - 6.0) * p.hsy;
            let ear_k = if p.front || near {
                1.0
            } else {
                lerp(1.0, 0.82, p.yaw)
            };
            let w = (15.5 - inset * 0.55) * flat_scale * ear_k;
            let h = (27.0 - inset)
                * flat_scale
                * if ear_k == 1.0 {
                    1.0
                } else {
                    lerp(1.0, 0.90, p.yaw)
                };
            let tip_a = base_a + side * 82.0 + sweep + p.ear_flat * 26.0;
            let tth = radians(tip_a - 90.0 + p.hrot);
            let (dx, dy) = (tth.cos(), tth.sin());
            let (px, py) = (-tth.sin(), tth.cos());
            let at = |along: f64, across: f64| {
                (
                    bx + dx * h * along + px * w * across,
                    by + dy * h * along + py * w * across,
                )
            };
            catmull_closed(
                &[
                    at(0.0, 0.46),
                    at(0.42, 0.60),
                    at(0.82, 0.40),
                    at(1.0, 0.0),
                    at(0.82, -0.40),
                    at(0.42, -0.52),
                    at(0.0, -0.46),
                ],
                0.12,
            )
        }

        /// One ear, by its species' builder.
        fn ear_shape(p: &Pose, side: f64, inset: f64) -> String {
            if dog(p) {
                ear_drop(p, side, inset)
            } else {
                ear_tri(p, side, inset)
            }
        }

        fn ears_paths(p: &Pose) -> Part {
            let (mut coat, mut out) = (Vec::new(), Vec::new());
            for side in [-1.0, 1.0] {
                out.push(ear_shape(p, side, -S * 0.9));
                coat.push(ear_shape(p, side, 0.0));
            }
            (coat, out)
        }

        /// The dog's inner lobe insets harder: on a hanging ear only the upper
        /// inside shows, so a cat-sized inner shape would paint pink over the
        /// whole flap and read as a tongue.
        fn inner_ear_paths(p: &Pose) -> Vec<String> {
            let inset = if dog(p) { 13.0 } else { 9.0 };
            vec![ear_shape(p, -1.0, inset), ear_shape(p, 1.0, inset)]
        }

        fn tail_pts(p: &Pose) -> Vec<Pt> {
            let bones: Vec<Pt> = p.tail.iter().map(|&a| (p.tail_len, a)).collect();
            chain(rump(p), &bones)
        }

        /// A dog's tail is a thicker, blunter club than a cat's tapering whip:
        /// with the tabby rings gone its OUTLINE has to carry it.
        fn tail_paths(p: &Pose) -> Part {
            let ps = tail_pts(p);
            let t = p.tail_thick * if dog(p) { 1.18 } else { 1.0 };
            let radii = [t, t * 0.92, t * 0.84, t * 0.76, t * 0.66];
            let grown: Vec<f64> = radii.iter().map(|r| r + S).collect();
            (vec![limb(&ps, &radii, true)], vec![limb(&ps, &grown, true)])
        }

        fn legs(p: &Pose, near: bool) -> Part {
            let mut specs = vec![(shoulder(p, !near), if near { p.fl_near } else { p.fl_far })];
            if near && p.front {
                // face-on both forelegs stand IN FRONT of the chest, side by
                specs.insert(0, (shoulder(p, true), p.fl_far));
            }
            if !p.hide_hind {
                specs.push((hip(p, !near), if near { p.hl_near } else { p.hl_far }));
            }
            let (mut coat, mut out) = (Vec::new(), Vec::new());
            for (root, spec) in specs {
                let ps = leg_pts(root, spec);
                let w = if near { 8.2 } else { 7.2 };
                let radii = [w, w * 0.76, w * 0.94];
                let grown: Vec<f64> = radii.iter().map(|r| r + S).collect();
                coat.push(limb(&ps, &radii, false));
                out.push(limb(&ps, &grown, false));
            }
            (coat, out)
        }

        /// A face feature in head-local units (x right, y down, 1 = radius).
        fn face_anchor(p: &Pose, dx: f64, dy: f64) -> Pt {
            let x = p.hx + dx * p.hr * p.hsx;
            let y = p.hy + dy * p.hr * p.hsy;
            rot_about(x, y, p.hx, p.hy, p.hrot)
        }

        /// The muzzle group's x offset: at rest a hint toward the near cheek;
        /// yawed ~0.22 head radii into the travel direction (the strongest
        /// "the face turned" cue); face-on dead centre.
        fn muzzle_dx(p: &Pose) -> f64 {
            if p.front {
                0.0
            } else {
                lerp(0.08, 0.22, p.yaw)
            }
        }

        fn muzzle_paths(p: &Pose) -> Vec<String> {
            if dog(p) {
                return dog_muzzle_paths(p);
            }
            let (cx, cy) = face_anchor(p, muzzle_dx(p), 0.38);
            vec![blob0(
                cx,
                cy,
                &[
                    (0.0, 13.5),
                    (50.0, 15.5),
                    (95.0, 17.0),
                    (140.0, 14.5),
                    (180.0, 12.0),
                    (220.0, 14.5),
                    (265.0, 17.0),
                    (310.0, 15.5),
                ],
            )]
        }

        /// How far the DOG's muzzle sits ahead of the cat's: modest on purpose.
        fn dog_snout_dx(p: &Pose) -> f64 {
            muzzle_dx(p)
                + if p.front {
                    0.0
                } else {
                    lerp(0.08, 0.12, p.yaw)
                }
        }

        /// The snout: the cat's round muzzle drawn out along the facing axis,
        /// shallower on top and longer ahead (90 degrees is the +x side).
        fn dog_muzzle_paths(p: &Pose) -> Vec<String> {
            if p.front {
                let (cx, cy) = face_anchor(p, dog_snout_dx(p), 0.46);
                return vec![blob0(
                    cx,
                    cy,
                    &[
                        (0.0, 11.0),
                        (45.0, 14.0),
                        (90.0, 15.5),
                        (135.0, 16.5),
                        (180.0, 17.0),
                        (225.0, 16.5),
                        (270.0, 15.5),
                        (315.0, 14.0),
                    ],
                )];
            }
            let (cx, cy) = face_anchor(p, dog_snout_dx(p), 0.44);
            vec![blob0(
                cx,
                cy,
                &[
                    (0.0, 11.0),
                    (45.0, 15.5),
                    (90.0, 20.0),
                    (135.0, 17.0),
                    (180.0, 13.5),
                    (225.0, 12.0),
                    (270.0, 11.0),
                    (315.0, 10.0),
                ],
            )]
        }

        /// The eye line rides the skull's equator.
        fn eye_centres(p: &Pose) -> (Pt, Pt) {
            if p.front {
                return (face_anchor(p, 0.42, 0.04), face_anchor(p, -0.42, 0.04));
            }
            let near = face_anchor(p, lerp(0.46, 0.48, p.yaw), lerp(0.02, -0.04, p.yaw));
            let mut far = face_anchor(p, lerp(-0.33, -0.16, p.yaw), lerp(0.04, -0.01, p.yaw));
            if let Some([dx, dy, _, _]) = p.far_eye {
                far = (far.0 + dx, far.1 + dy);
            }
            (near, far)
        }

        /// (near_r, far_rx, far_ry): the far eye foreshortens in WIDTH with
        /// the yaw and only mildly in height: a turning eyeball loses azimuth,
        /// not elevation.
        fn eye_scales(p: &Pose) -> (f64, f64, f64) {
            let rn = 8.6;
            if p.front {
                return (rn, rn, rn);
            }
            let fw = lerp(0.884, 0.62, p.yaw);
            let fh = lerp(0.884, 0.80, p.yaw);
            if let Some([_, _, sx, sy]) = p.far_eye {
                return (rn, rn * fw * sx, rn * fh * sy);
            }
            (rn, rn * fw, rn * fh)
        }

        fn eye_paths(p: &Pose) -> Vec<String> {
            let (near, far) = eye_centres(p);
            let (rn, rfx, rfy) = eye_scales(p);
            match p.eyes {
                "closed" => vec![lid(near, rn, 1.0), lid(far, rfx, 1.0)],
                "happy" => vec![arc_eye(near, rn), arc_eye(far, rfx)],
                "squint" => vec![lid(near, rn, 0.55), lid(far, rfx, 0.55)],
                "halflid" => vec![half(near, rn, 1.0), half(far, rfx, -1.0)],
                "wink" => vec![
                    arc_eye(near, rn),
                    ellipse(far.0, far.1, rfx, rfy * 1.06, 0.0),
                ],
                eyes => {
                    let k = if eyes == "wide" { 1.22 } else { 1.06 };
                    vec![
                        ellipse(near.0, near.1, rn, rn * k, 0.0),
                        ellipse(far.0, far.1, rfx, rfy * k, 0.0),
                    ]
                }
            }
        }

        /// A closed sleeping lid: a shallow downward arc with body.
        fn lid((x, y): Pt, r: f64, thick: f64) -> String {
            let w = r * 1.15;
            let t = 4.8 * thick;
            catmull_closed(
                &[
                    (x - w, y - 1.5),
                    (x, y + r * 0.52),
                    (x + w, y - 1.5),
                    (x, y + r * 0.52 - t),
                ],
                0.16,
            )
        }

        /// A half-lidded eye: a slim lens on the upper half of the aperture
        /// (where a half-closed lid sits, clear of the whisker roots) whose
        /// OUTER corner (`sgn`, +1 near / -1 far) droops a step lower: drowsy
        /// contentment, not the squint's wince.
        fn half((x, y): Pt, r: f64, sgn: f64) -> String {
            let yy = y - r * 0.14;
            let w = r * 0.90;
            let li = r * if sgn < 0.0 { 0.16 } else { 0.0 };
            let ri = r * if sgn > 0.0 { 0.16 } else { 0.0 };
            catmull_closed(
                &[
                    (x - w, yy + li),
                    (x, yy + r * 0.10),
                    (x + w, yy + ri),
                    (x + w * 0.55, yy + ri + r * 0.30),
                    (x, yy + r * 0.52),
                    (x - w * 0.55, yy + li + r * 0.30),
                ],
                0.10,
            )
        }

        /// A happy ^ eye: a fat arc, so the purr and the groom keep a face at 1x.
        fn arc_eye((x, y): Pt, r: f64) -> String {
            let w = r * 1.18;
            let t = 5.8;
            catmull_closed(
                &[
                    (x - w, y + r * 0.42),
                    (x, y - r * 0.46),
                    (x + w, y + r * 0.42),
                    (x, y - r * 0.46 + t),
                ],
                0.14,
            )
        }

        fn lids_down(p: &Pose) -> bool {
            matches!(p.eyes, "closed" | "happy" | "squint" | "halflid")
        }

        fn iris_paths(p: &Pose) -> Vec<String> {
            if lids_down(p) {
                return Vec::new();
            }
            let (near, far) = eye_centres(p);
            let (_, rfx, rfy) = eye_scales(p);
            let mut out = if p.eyes == "wink" {
                Vec::new()
            } else {
                vec![ellipse(near.0, near.1, 6.2, 6.8, 0.0)]
            };
            let (frx, fry) = if p.front {
                (6.2, 6.8)
            } else {
                (rfx * 0.71, rfy * 0.79)
            };
            out.push(ellipse(far.0, far.1, frx, fry, 0.0));
            out
        }

        fn pupil_paths(p: &Pose) -> Vec<String> {
            if lids_down(p) {
                return Vec::new();
            }
            let (near, far) = eye_centres(p);
            let (_, rfx, rfy) = eye_scales(p);
            let (gx, gy) = p.gaze;
            let k = if p.eyes == "wide" { 1.9 } else { 2.5 };
            let mut out = if p.eyes == "wink" {
                Vec::new()
            } else {
                vec![ellipse(
                    near.0 + gx,
                    near.1 + gy,
                    2.9,
                    6.4 / k * 2.5 * 0.62,
                    0.0,
                )]
            };
            let (frx, fh) = if p.front {
                (2.9, 6.4)
            } else {
                (rfx * 0.33, rfy * 0.74)
            };
            let fgx = if p.front { gx } else { gx * 0.85 };
            out.push(ellipse(
                far.0 + fgx,
                far.1 + gy,
                frx,
                fh / k * 2.5 * 0.62,
                0.0,
            ));
            out
        }

        fn catchlight_paths(p: &Pose) -> Vec<String> {
            if lids_down(p) {
                return Vec::new();
            }
            let (near, far) = eye_centres(p);
            let (_, rfx, rfy) = eye_scales(p);
            let mut out = if p.eyes == "wink" {
                Vec::new()
            } else {
                vec![ellipse(near.0 - 2.6, near.1 - 3.4, 2.5, 2.5, 0.0)]
            };
            if p.front {
                out.push(ellipse(far.0 - 2.6, far.1 - 3.4, 2.5, 2.5, 0.0));
            } else {
                out.push(ellipse(
                    far.0 - rfx * 0.32,
                    far.1 - rfy * 0.42,
                    rfx * 0.29,
                    rfx * 0.29,
                    0.0,
                ));
            }
            out
        }

        /// The yawn's gape: the open mouth grown ~1.4x and dropped a step.
        fn gape(cx: f64, cy: f64) -> String {
            blob0(
                cx,
                cy + 9.6,
                &[
                    (0.0, 7.4),
                    (60.0, 9.2),
                    (120.0, 9.6),
                    (180.0, 9.0),
                    (240.0, 9.6),
                    (300.0, 9.2),
                ],
            )
        }

        /// The yawed forward shift of the dog's nose and mouth along its snout.
        fn dog_nose_dx(p: &Pose) -> f64 {
            dog_snout_dx(p)
                + if p.front {
                    0.0
                } else {
                    lerp(0.28, 0.34, p.yaw)
                }
        }

        fn nose_paths(p: &Pose) -> Vec<String> {
            if dog(p) {
                let (cx, cy) = if p.front {
                    face_anchor(p, dog_snout_dx(p), 0.56)
                } else {
                    face_anchor(p, dog_snout_dx(p) + lerp(0.28, 0.34, p.yaw), 0.58)
                };
                let mut out = Vec::new();
                if p.mouth == "yawn" {
                    let (mcx, mcy) = face_anchor(p, dog_nose_dx(p), 0.62);
                    out.push(gape(mcx, mcy));
                }
                out.push(blob0(
                    cx,
                    cy,
                    &[
                        (0.0, 5.4),
                        (55.0, 7.2),
                        (110.0, 6.6),
                        (180.0, 5.6),
                        (250.0, 6.6),
                        (305.0, 7.2),
                    ],
                ));
                return out;
            }
            let mut out = Vec::new();
            if p.mouth == "yawn" {
                let (mcx, mcy) = face_anchor(p, muzzle_dx(p), 0.38);
                out.push(gape(mcx, mcy));
            }
            let (cx, cy) = face_anchor(
                p,
                muzzle_dx(p),
                lerp(0.26, 0.32, if p.front { 0.0 } else { p.yaw }),
            );
            out.push(catmull_closed(
                &[(cx - 5.2, cy - 2.6), (cx + 5.2, cy - 2.6), (cx, cy + 5.0)],
                0.22,
            ));
            out
        }

        fn mouth_paths(p: &Pose) -> Vec<String> {
            let (cx, cy) = if dog(p) {
                face_anchor(p, dog_nose_dx(p), 0.62)
            } else {
                face_anchor(p, muzzle_dx(p), 0.38)
            };
            match p.mouth {
                "none" => Vec::new(),
                "open" => vec![blob0(
                    cx,
                    cy + 8.0,
                    &[
                        (0.0, 5.4),
                        (60.0, 6.8),
                        (120.0, 7.0),
                        (180.0, 6.8),
                        (240.0, 7.0),
                        (300.0, 6.8),
                    ],
                )],
                "yawn" => vec![gape(cx, cy)],
                "oof" => vec![blob0(
                    cx,
                    cy + 7.4,
                    &[
                        (0.0, 3.2),
                        (60.0, 4.0),
                        (120.0, 4.2),
                        (180.0, 4.0),
                        (240.0, 4.2),
                        (300.0, 4.0),
                    ],
                )],
                "flat" => vec![limb(
                    &[(cx - 6.0, cy + 6.0), (cx + 6.0, cy + 6.0)],
                    &[1.6, 1.6],
                    false,
                )],
                _ => [-1.0, 1.0]
                    .into_iter()
                    .map(|sgn: f64| {
                        limb(
                            &[
                                (cx, cy + 4.4),
                                (cx + sgn * 3.4, cy + 8.6),
                                (cx + sgn * 7.6, cy + 4.6),
                            ],
                            &[1.9, 1.7, 1.3],
                            true,
                        )
                    })
                    .collect(),
            }
        }

        /// Dogs have whiskers in life and never in drawings of dogs: the fan is
        /// a CAT cue, and leaving it on undoes the ears and the snout.
        fn whisker_paths(p: &Pose) -> Vec<String> {
            if dog(p) {
                return Vec::new();
            }
            let (fans, rows): ([Fan; 2], [Pt3; 2]) = if p.front {
                (
                    [(1.0, 0.68, 0.82, 0.0), (-1.0, -0.68, 0.82, 0.0)],
                    [(0.14, 4.0, 26.0), (0.26, 16.0, 27.0)],
                )
            } else {
                (
                    [
                        (1.0, lerp(0.42, 0.50, p.yaw), 1.0, 0.08 * p.yaw),
                        (
                            -1.0,
                            lerp(-0.80, -0.52, p.yaw),
                            lerp(0.46, 0.30, p.yaw),
                            0.0,
                        ),
                    ],
                    [(-0.03, -13.0, 27.0), (0.11, 7.0, 29.0)],
                )
            };
            let mut out = Vec::new();
            for (side, base_dx, scale, droot) in fans {
                if (side < 0.0 && !p.whisker_far) || (side > 0.0 && !p.whisker_near) {
                    continue;
                }
                for (dy, sweep, ln0) in rows {
                    let ln = ln0 * scale;
                    let (bx, by) = face_anchor(p, base_dx, 0.30 + dy + droot);
                    let th = if side > 0.0 {
                        radians(sweep)
                    } else {
                        radians(180.0 - sweep)
                    };
                    let tx = bx + th.cos() * ln;
                    let ty = by + th.sin() * ln;
                    let mx = (bx + tx) / 2.0;
                    let my = (by + ty) / 2.0 - 2.0;
                    out.push(limb(
                        &[(bx, by), (mx, my), (tx, ty)],
                        &[2.4, 1.7, 0.9],
                        true,
                    ));
                }
            }
            out
        }

        type Pt3 = (f64, f64, f64);

        /// A whisker fan: (side, base_dx, scale, droot).
        type Fan = (f64, f64, f64, f64);

        /// Tucked just under the eyes, not beside the muzzle: the kawaii
        /// placement.
        fn blush_paths(p: &Pose) -> Vec<String> {
            if !p.blush {
                return Vec::new();
            }
            if p.front {
                let a = face_anchor(p, 0.55, 0.44);
                let b = face_anchor(p, -0.55, 0.44);
                return vec![
                    ellipse(a.0, a.1, 5.0, 3.5, 0.0),
                    ellipse(b.0, b.1, 5.0, 3.5, 0.0),
                ];
            }
            let a = face_anchor(p, lerp(0.60, 0.64, p.yaw), 0.44);
            let b = face_anchor(p, lerp(-0.48, -0.30, p.yaw), 0.46);
            vec![
                ellipse(a.0, a.1, 5.0, 3.5, 0.0),
                ellipse(b.0, b.1, lerp(4.4, 3.4, p.yaw), lerp(3.1, 2.5, p.yaw), 0.0),
            ]
        }

        /// The tabby: three back bars and two tail rings, riding the torso.
        fn pattern_paths(p: &Pose) -> Vec<String> {
            if dog(p) {
                return Vec::new();
            }
            let mut out = Vec::new();
            if p.front {
                for (i, t) in [-0.42, 0.0, 0.42].into_iter().enumerate() {
                    let (x, y) = face_anchor(p, t * 0.60, -0.64);
                    out.push(ellipse(x, y, 3.4 - (i as f64 - 1.0).abs() * 0.3, 7.0, 0.0));
                }
            } else if let (true, Some(haunch)) = (p.bar_site == "haunch", p.haunch_at) {
                let (cx, cy, hrx, hry, hrot_deg) = match haunch {
                    Haunch::Ellipse(cx, cy, rx, ry, rot) => (cx, cy, rx, ry, rot),
                    Haunch::Circle(cx, cy, r) => (cx, cy, r, r, 0.0),
                };
                for (i, a) in [-32.0, 0.0, 32.0].into_iter().enumerate() {
                    let th = radians(a);
                    let x0 = th.sin() * hrx * 0.74;
                    let y0 = -th.cos() * hry * 0.74;
                    let (x, y) = rot_about(cx + x0, cy + y0, cx, cy, hrot_deg);
                    out.push(ellipse(
                        x,
                        y,
                        3.5 - (i as f64 - 1.0).abs() * 0.3,
                        7.4,
                        radians(a * 0.75 + hrot_deg),
                    ));
                }
            } else if !p.curl {
                for (i, t) in [-0.40, -0.04, 0.32].into_iter().enumerate() {
                    let cx = p.bx + t * p.brx;
                    let cy = p.by - p.bry * 0.56;
                    let (x, y) = rot_about(cx, cy, p.bx, p.by, p.brot);
                    out.push(ellipse(
                        x,
                        y,
                        3.8 - i as f64 * 0.3,
                        8.4,
                        radians(p.brot + t * 26.0),
                    ));
                }
            }
            let ps = tail_pts(p);
            for (x, y) in [ps[2], ps[3]] {
                out.push(ellipse(x, y, 5.6, 3.4, 0.0));
            }
            out
        }

        /// Every coordinate of a path string, as (xs, ys).
        fn coords(d: &str) -> (Vec<f64>, Vec<f64>) {
            let nums: Vec<f64> = d
                .split_whitespace()
                .filter_map(|t| t.parse::<f64>().ok())
                .collect();
            (
                nums.iter().step_by(2).copied().collect(),
                nums.iter().skip(1).step_by(2).copied().collect(),
            )
        }

        pub type Layer = (&'static str, &'static str, &'static str, Vec<String>);

        /// (min x, min y, max x, max y) of a pose's registered ink.
        pub fn bbox(layers: &[Layer]) -> [f64; 4] {
            let mut b = [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ];
            for (_, _, _, paths) in layers {
                for d in paths {
                    let (xs, ys) = coords(d);
                    for x in xs {
                        b[0] = b[0].min(x);
                        b[2] = b[2].max(x);
                    }
                    for y in ys {
                        b[1] = b[1].min(y);
                        b[3] = b[3].max(y);
                    }
                }
            }
            b
        }

        /// The painter-ordered layers of one pose, registered into the viewbox.
        pub fn build(p: &Pose) -> Vec<Layer> {
            let ((far_coat, far_out), (near_coat, near_out)) = if p.hide_legs {
                ((Vec::new(), Vec::new()), (Vec::new(), Vec::new()))
            } else {
                let far = if p.show_far_legs {
                    legs(p, false)
                } else {
                    (Vec::new(), Vec::new())
                };
                (far, legs(p, true))
            };
            let (t_coat, t_out) = torso_paths(p);
            let (h_coat, h_out) = if p.curl {
                (Vec::new(), Vec::new())
            } else {
                haunch_paths(p)
            };
            let (hd_coat, hd_out) = head_paths(p);
            let (e_coat, e_out) = ears_paths(p);
            let (tl_coat, tl_out) = tail_paths(p);

            let outline = [tl_out, far_out, h_out, t_out, near_out, e_out, hd_out].concat();
            let mut coat = [
                tl_coat, far_coat, h_coat, t_coat, near_coat, e_coat, hd_coat,
            ]
            .concat();
            if let Some([bx, by, brx, bry]) = p.belly {
                // LAST in the coat list, and coat-only: it fills a counter the
                coat.push(ellipse(bx, by, brx, bry, 0.0));
            }
            let layers: Vec<Layer> = vec![
                ("outline", INK, "fixed", outline),
                ("coat", COAT, "coat", coat),
                ("pattern", PATTERN, "fixed", pattern_paths(p)),
                ("inner_ear", INNER_EAR, "fixed", inner_ear_paths(p)),
                ("muzzle", MUZZLE, "fixed", muzzle_paths(p)),
                ("eye", EYE, "fixed", eye_paths(p)),
                ("iris", IRIS, "iris", iris_paths(p)),
                ("detail", PUPIL, "fixed", pupil_paths(p)),
                ("catch_light", LIGHT, "fixed", catchlight_paths(p)),
                (
                    "nose",
                    if dog(p) { DOG_NOSE } else { NOSE },
                    "fixed",
                    nose_paths(p),
                ),
                ("mouth", EYE, "fixed", mouth_paths(p)),
                ("whisker", INK, "fixed", whisker_paths(p)),
                ("blush", BLUSH, "fixed", blush_paths(p)),
            ];
            let raw: Vec<Layer> = layers.into_iter().filter(|l| !l.3.is_empty()).collect();
            let (dx, dy) = registration(&raw, p);
            raw.into_iter()
                .map(|(r, f, rc, ps)| (r, f, rc, ps.iter().map(|d| translate(d, dx, dy)).collect()))
                .collect()
        }

        /// The pose's (dx, dy) into the viewbox.
        fn registration(raw: &[Layer], p: &Pose) -> Pt {
            (OX, GROUND_INK - p.airborne - bbox(raw)[3])
        }

        /// Every coordinate pair of a path string offset by (dx, dy).
        pub fn translate(d: &str, dx: f64, dy: f64) -> String {
            let mut out = Vec::new();
            let mut parts = d.split_whitespace();
            while let Some(t) = parts.next() {
                if matches!(t, "M" | "L" | "C" | "Z") {
                    out.push(t.to_string());
                    continue;
                }
                let x: f64 = t.parse().unwrap_or(f64::NAN) + dx;
                let y: f64 = parts
                    .next()
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(f64::NAN)
                    + dy;
                out.push(fmt(x));
                out.push(fmt(y));
            }
            out.join(" ")
        }

        /// Many poses merged into ONE asset, so a single render shows the set.
        pub fn sheet(poses: &[Pose], cols: usize, ident: &str) -> String {
            let rows = poses.len().div_ceil(cols);
            let mut order: Vec<(&str, &str, &str)> = Vec::new();
            let mut merged: Vec<Vec<String>> = Vec::new();
            for (i, p) in poses.iter().enumerate() {
                let dx = (i % cols) as f64 * VB_W;
                let dy = (i / cols) as f64 * VB_H;
                for (role, fill, recolor, paths) in build(p) {
                    let key = (role, fill, recolor);
                    let at = match order.iter().position(|k| *k == key) {
                        Some(at) => at,
                        None => {
                            order.push(key);
                            merged.push(Vec::new());
                            order.len() - 1
                        }
                    };
                    merged[at].extend(paths.iter().map(|d| translate(d, dx, dy)));
                }
            }
            let mut lines = vec![
                format!("id = \"{ident}\""),
                "kind = \"special\"".to_string(),
                format!(
                    "viewbox = [{}, {}]",
                    fmt(cols as f64 * VB_W),
                    fmt(rows as f64 * VB_H)
                ),
                "anchor = { eye_y = 0.34, center_x = 0.5, word_top = 1.0 }".to_string(),
            ];
            let canon: Vec<&str> = build(&poses[0]).iter().map(|l| l.0).collect();
            let mut keyed: Vec<usize> = (0..order.len()).collect();
            keyed.sort_by_key(|&k| canon.iter().position(|r| *r == order[k].0).unwrap_or(99));
            for k in keyed {
                let (role, fill, recolor) = order[k];
                super::super::layer_block(&mut lines, role, fill, recolor, &merged[k]);
            }
            lines.join("\n") + "\n"
        }

        /// One pose's asset.
        pub fn emit(p: &Pose) -> String {
            let mut lines = vec![
                format!("id = \"{}\"", p.ident),
                "kind = \"special\"".to_string(),
                format!("viewbox = [{}, {}]", fmt(VB_W), fmt(VB_H)),
                "anchor = { eye_y = 0.34, center_x = 0.5, word_top = 1.0 }".to_string(),
                String::new(),
                format!("# {}", p.note),
            ];
            for (role, fill, recolor, paths) in build(p) {
                super::super::layer_block(&mut lines, role, fill, recolor, &paths);
            }
            lines.join("\n") + "\n"
        }
    }

    mod poses {

        use super::model::{Haunch, Leg, Pose, bbox, build, emit, sheet};
        use super::rig::{VB_H, VB_W};

        /// The default thigh and shin.
        const TH: f64 = 19.0;
        const SH: f64 = 15.0;

        const fn leg(thigh: f64, shin: f64) -> Leg {
            (thigh, shin, TH, SH)
        }

        const fn leg4(thigh: f64, shin: f64, tl: f64, sl: f64) -> Leg {
            (thigh, shin, tl, sl)
        }

        /// The neutral stand: every other frame is a departure from this.
        fn stand() -> Pose {
            Pose {
                ident: "pet_stand".to_string(),
                note: "Neutral stand. The rig's rest pose and the measure of every other frame."
                    .to_string(),
                fl_near: leg(5.0, -3.0),
                fl_far: leg(-7.0, 3.0),
                hl_near: leg(-7.0, 7.0),
                hl_far: leg(5.0, -5.0),
                ..Pose::default()
            }
        }

        /// The stand, renamed: the base every `Pose { .., ..d(..) }` departs from.
        fn d(ident: &str, note: &str) -> Pose {
            Pose {
                ident: ident.to_string(),
                note: note.to_string(),
                ..stand()
            }
        }

        const REACH: Leg = leg(26.0, 12.0);
        const PASS_F: Leg = leg(6.0, -14.0);
        const TRAIL: Leg = leg(-24.0, -6.0);
        const PASS_B: Leg = leg(-8.0, 16.0);

        fn walk() -> Vec<Pose> {
            vec![
                Pose {
                    yaw: 1.0,
                    fl_near: REACH,
                    fl_far: TRAIL,
                    hl_near: TRAIL,
                    hl_far: REACH,
                    by: 64.0,
                    tail: [-108.0, -136.0, -168.0, -202.0],
                    ..d(
                        "pet_walk_0",
                        "Walk, contact: near-fore reaches, near-hind drives back.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    fl_near: PASS_F,
                    fl_far: PASS_B,
                    hl_near: PASS_B,
                    hl_far: PASS_F,
                    by: 62.0,
                    tail: [-116.0, -144.0, -176.0, -210.0],
                    ..d(
                        "pet_walk_1",
                        "Walk, pass: the legs swing under, the body lifts.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    fl_near: TRAIL,
                    fl_far: REACH,
                    hl_near: REACH,
                    hl_far: TRAIL,
                    by: 64.0,
                    tail: [-120.0, -148.0, -180.0, -214.0],
                    ..d("pet_walk_2", "Walk, contact: the other diagonal.")
                },
                Pose {
                    yaw: 1.0,
                    fl_near: PASS_B,
                    fl_far: PASS_F,
                    hl_near: PASS_F,
                    hl_far: PASS_B,
                    by: 62.0,
                    tail: [-112.0, -140.0, -172.0, -206.0],
                    ..d(
                        "pet_walk_3",
                        "Walk, pass: the legs swing under the other way.",
                    )
                },
            ]
        }

        fn run() -> Vec<Pose> {
            vec![
                Pose {
                    yaw: 1.0,
                    by: 62.0,
                    brx: 35.0,
                    bry: 26.0,
                    brot: 10.0,
                    hy: 50.0,
                    hx: 142.0,
                    hrot: 8.0,
                    ear_flat: 0.45,
                    eyes: "wide",
                    mouth: "open",
                    fl_near: leg4(-34.0, 40.0, 14.0, 11.0),
                    fl_far: leg4(-42.0, 34.0, 14.0, 11.0),
                    hl_near: leg4(44.0, -34.0, 15.0, 12.0),
                    hl_far: leg4(36.0, -40.0, 15.0, 12.0),
                    tail: [-92.0, -108.0, -124.0, -140.0],
                    tail_len: 14.5,
                    ..d(
                        "pet_run_0",
                        "Run, gather: the back rounds up and every paw swings under.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    airborne: 11.0,
                    by: 68.0,
                    brx: 45.0,
                    bry: 20.0,
                    brot: -4.0,
                    hy: 44.0,
                    hx: 157.0,
                    ear_flat: 0.7,
                    eyes: "wide",
                    mouth: "open",
                    fl_near: leg4(48.0, 40.0, 18.0, 14.0),
                    fl_far: leg4(38.0, 34.0, 18.0, 14.0),
                    hl_near: leg4(-46.0, -58.0, 19.0, 16.0),
                    hl_far: leg4(-36.0, -50.0, 19.0, 16.0),
                    tail: [-72.0, -80.0, -88.0, -96.0],
                    tail_len: 15.5,
                    ..d(
                        "pet_run_1",
                        "Run, extension: the body stretches long, every paw off the floor.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    by: 64.0,
                    brx: 42.0,
                    bry: 22.0,
                    brot: -8.0,
                    hy: 40.0,
                    hx: 155.0,
                    ear_flat: 0.5,
                    eyes: "wide",
                    mouth: "open",
                    fl_near: leg4(12.0, 2.0, 18.0, 15.0),
                    fl_far: leg4(2.0, -6.0, 18.0, 15.0),
                    hl_near: leg4(-40.0, -20.0, 19.0, 16.0),
                    hl_far: leg4(-30.0, -14.0, 19.0, 16.0),
                    tail: [-80.0, -92.0, -104.0, -118.0],
                    tail_len: 15.0,
                    ..d(
                        "pet_run_2",
                        "Run, contact: the forelegs take the whole cat.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    airborne: 13.0,
                    by: 60.0,
                    brx: 40.0,
                    bry: 22.0,
                    brot: 2.0,
                    hy: 40.0,
                    hx: 150.0,
                    ear_flat: 0.6,
                    eyes: "wide",
                    mouth: "open",
                    fl_near: leg4(-18.0, 34.0, 16.0, 13.0),
                    fl_far: leg4(-26.0, 28.0, 16.0, 13.0),
                    hl_near: leg4(28.0, -46.0, 17.0, 14.0),
                    hl_far: leg4(20.0, -52.0, 17.0, 14.0),
                    tail: [-84.0, -96.0, -110.0, -126.0],
                    tail_len: 15.0,
                    ..d(
                        "pet_run_3",
                        "Run, suspension: every paw is off the ground at once.",
                    )
                },
            ]
        }

        /// An upright chest over a seated haunch: the hind legs fold away and
        /// the rump is the ground contact, the forelegs dropping straight down
        /// the front edge.
        fn sit_base(p: Pose) -> Pose {
            Pose {
                bx: 104.0,
                by: 52.0,
                brx: 22.0,
                bry: 27.0,
                brot: -10.0,
                hide_hind: true,
                haunch_at: Some(Haunch::Circle(82.0, 88.0, 25.0)),
                hx: 134.0,
                hy: 38.0,
                fl_root: Some((124.0, 68.0)),
                fl_near: leg4(0.0, 0.0, 24.0, 22.0),
                fl_far: leg4(-8.0, 4.0, 24.0, 22.0),
                tail_root: Some((96.0, 105.0)),
                tail: [95.0, 105.0, 122.0, 152.0],
                tail_len: 12.0,
                tail_thick: 8.5,
                ..p
            }
        }

        /// [`d`] on the seat.
        fn sat(ident: &str, note: &str) -> Pose {
            sit_base(d(ident, note))
        }

        fn settled() -> Vec<Pose> {
            vec![
                sat(
                    "pet_sit",
                    "Sit: settled on the haunch, tail swept around to the forepaws.",
                ),
                Pose {
                    tail: [95.0, 112.0, 152.0, 196.0],
                    ..sat(
                        "pet_sit_flick",
                        "Sit, tail flick: the same seat, the tail-tip snapped up.",
                    )
                },
                Pose {
                    eyes: "happy",
                    mouth: "smile",
                    brx: 25.5,
                    bry: 29.0,
                    hsx: 1.04,
                    ..sat(
                        "pet_purr",
                        "Purr: eyes squeezed shut, cheeks up, the chest swelled a notch.",
                    )
                },
                Pose {
                    eyes: "happy",
                    mouth: "open",
                    hrot: 0.0,
                    hy: 40.0,
                    fl_near: leg4(150.0, 133.0, 16.0, 13.0),
                    ..sat(
                        "pet_groom",
                        "Groom: one forepaw lifted to the muzzle, eyes shut.",
                    )
                },
            ]
        }

        /// Both whisker fans are spent here: the FAR fan lands on the curled
        /// torso's dome (strokes on coat read as scratches), and the NEAR fan
        /// roots under an eye this pose tips toward it (eye, fan and outline
        /// clot into a black smear).
        fn sleep_base(p: Pose) -> Pose {
            Pose {
                curl: true,
                hide_legs: true,
                show_far_legs: false,
                bx: 104.0,
                by: 88.0,
                brx: 46.0,
                bry: 26.0,
                hx: 150.0,
                hy: 76.0,
                hr: 27.0,
                hrot: 20.0,
                eyes: "happy",
                mouth: "none",
                ear_flat: 0.30,
                whisker_far: false,
                whisker_near: false,
                tail_root: Some((62.0, 76.0)),
                tail: [150.0, 115.0, 95.0, 78.0],
                tail_len: 13.0,
                tail_thick: 8.5,
                ..p
            }
        }

        fn sleep() -> Vec<Pose> {
            vec![
                sleep_base(d(
                    "pet_sleep_0",
                    "Sleep, breath out: a curled loaf with the tail draped over.",
                )),
                Pose {
                    brx: 47.0,
                    bry: 27.6,
                    by: 87.0,
                    hy: 75.0,
                    ..sleep_base(d(
                        "pet_sleep_1",
                        "Sleep, breath in: the same loaf, one notch fuller.",
                    ))
                },
            ]
        }

        fn reactive() -> Vec<Pose> {
            vec![
                Pose {
                    yaw: 1.0,
                    by: 76.0,
                    brx: 41.0,
                    bry: 21.0,
                    brot: 4.0,
                    hy: 52.0,
                    hx: 152.0,
                    eyes: "wide",
                    ear_near: -8.0,
                    ear_far: -6.0,
                    fl_near: leg4(14.0, -20.0, 15.0, 12.0),
                    fl_far: leg4(6.0, -26.0, 15.0, 12.0),
                    hl_near: leg4(34.0, -30.0, 16.0, 13.0),
                    hl_far: leg4(26.0, -36.0, 16.0, 13.0),
                    tail: [-70.0, -58.0, -44.0, -28.0],
                    tail_len: 14.0,
                    ..d(
                        "pet_crouch",
                        "Pounce, crouch: gathered low, eyes locked on the landing.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    by: 64.0,
                    brx: 46.0,
                    bry: 19.0,
                    brot: -14.0,
                    hy: 39.0,
                    hx: 160.0,
                    eyes: "wide",
                    mouth: "open",
                    ear_flat: 0.35,
                    fl_near: leg4(56.0, 48.0, 19.0, 15.0),
                    fl_far: leg4(46.0, 40.0, 19.0, 15.0),
                    hl_near: leg4(-52.0, -64.0, 20.0, 16.0),
                    hl_far: leg4(-42.0, -56.0, 20.0, 16.0),
                    tail: [-64.0, -70.0, -76.0, -82.0],
                    tail_len: 16.0,
                    ..d(
                        "pet_leap",
                        "Pounce, flight: stretched along the arc, front paws leading.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    by: 72.0,
                    brx: 44.0,
                    bry: 20.0,
                    brot: -6.0,
                    hy: 50.0,
                    hx: 156.0,
                    eyes: "squint",
                    mouth: "oof",
                    ear_flat: 0.5,
                    fl_near: leg4(22.0, 6.0, 17.0, 13.0),
                    fl_far: leg4(14.0, -2.0, 17.0, 13.0),
                    hl_near: leg4(-32.0, 10.0, 17.0, 13.0),
                    hl_far: leg4(-24.0, 4.0, 17.0, 13.0),
                    tail: [-96.0, -114.0, -134.0, -152.0],
                    tail_len: 14.5,
                    ..d(
                        "pet_land",
                        "Pounce, landing: braced forelegs, the whole cat compressed.",
                    )
                },
                Pose {
                    by: 60.0,
                    brx: 36.0,
                    bry: 26.0,
                    brot: 0.0,
                    hy: 48.0,
                    hx: 146.0,
                    hrot: -8.0,
                    eyes: "wide",
                    mouth: "open",
                    ear_flat: 1.0,
                    blush: false,
                    fl_near: leg4(16.0, 18.0, 18.0, 15.0),
                    fl_far: leg4(8.0, 12.0, 18.0, 15.0),
                    hl_near: leg4(-20.0, -18.0, 18.0, 15.0),
                    hl_far: leg4(-12.0, -12.0, 18.0, 15.0),
                    tail: [-142.0, -160.0, -178.0, -196.0],
                    tail_len: 13.0,
                    tail_thick: 12.5,
                    ..d(
                        "pet_startle",
                        "Startle: back arched, tail bottled, ears pinned, eyes wide.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    by: 68.0,
                    brx: 42.0,
                    bry: 22.0,
                    brot: -22.0,
                    hy: 64.0,
                    hx: 152.0,
                    hrot: 10.0,
                    eyes: "happy",
                    mouth: "open",
                    fl_near: leg4(24.0, 46.0, 15.0, 11.0),
                    fl_far: leg4(16.0, 40.0, 15.0, 11.0),
                    hl_near: leg4(-6.0, 8.0, 20.0, 16.0),
                    hl_far: leg4(4.0, 2.0, 20.0, 16.0),
                    tail: [-176.0, -196.0, -216.0, -238.0],
                    tail_len: 14.5,
                    ..d(
                        "pet_playbow",
                        "Frolic, play bow: chest to the floor, rump and tail high.",
                    )
                },
                Pose {
                    eyes: "wide",
                    mouth: "open",
                    hrot: -10.0,
                    yaw: 1.0,
                    brot: -20.0,
                    hx: 142.0,
                    hy: 39.0,
                    fl_near: leg4(112.0, 116.0, 22.0, 18.0),
                    fl_far: leg4(46.0, 26.0, 21.0, 17.0),
                    tail: [92.0, 108.0, 142.0, 184.0],
                    belly: Some([139.0, 108.6, 11.5, 13.5]),
                    ..sat(
                        "pet_bat",
                        "Frolic, swipe: reared back on the haunch, a paw batting the caret.",
                    )
                },
                Pose {
                    yaw: 0.6,
                    by: 72.0,
                    brx: 48.0,
                    bry: 19.0,
                    brot: -16.0,
                    hy: 76.0,
                    hx: 162.0,
                    hrot: 16.0,
                    eyes: "happy",
                    mouth: "open",
                    fl_near: leg4(58.0, 78.0, 18.0, 14.0),
                    fl_far: leg4(50.0, 72.0, 18.0, 14.0),
                    hl_near: leg4(-10.0, 6.0, 20.0, 16.0),
                    hl_far: leg4(0.0, 0.0, 20.0, 16.0),
                    tail: [-160.0, -184.0, -206.0, -228.0],
                    tail_len: 15.0,
                    ..d(
                        "pet_stretch",
                        "Stretch: the long wake-up, forelegs out, back scooped, yawning.",
                    )
                },
                Pose {
                    by: 63.0,
                    hy: 39.0,
                    hx: 152.0,
                    eyes: "wide",
                    ear_near: 6.0,
                    ear_far: 4.0,
                    fl_near: leg(2.0, -1.0),
                    fl_far: leg(-6.0, 2.0),
                    hl_near: leg(-6.0, 6.0),
                    hl_far: leg(4.0, -4.0),
                    tail: [-172.0, -182.0, -190.0, -198.0],
                    tail_len: 14.5,
                    ..d(
                        "pet_perk",
                        "Alert: ears up, head lifted, tail flagged — it has seen you.",
                    )
                },
            ]
        }

        /// The bell: a compact seated silhouette whose base is about one
        /// head-width across, the head overlapping the chest by a third: the
        /// classic face-on cat, not a totem pole.
        fn sit_front(ident: &str, note: &str) -> Pose {
            Pose {
                front: true,
                hide_hind: true,
                show_far_legs: false,
                bx: 104.0,
                by: 80.0,
                brx: 23.0,
                bry: 27.0,
                brot: 0.0,
                haunch_at: Some(Haunch::Circle(104.0, 86.0, 29.0)),
                hx: 104.0,
                hy: 42.0,
                fl_root: Some((116.0, 78.0)),
                fl_near: leg4(0.0, 0.0, 20.0, 14.0),
                fl_far: leg4(0.0, 0.0, 20.0, 14.0),
                tail_root: Some((121.0, 104.0)),
                tail: [95.0, 100.0, 108.0, 125.0],
                tail_len: 11.0,
                tail_thick: 8.0,
                ..d(ident, note)
            }
        }

        /// The over-the-shoulder seat, built on body/head OPPOSITION: the body
        /// faces AWAY while the head comes BACK.
        fn shoulder_seat(ident: &str, note: &str) -> Pose {
            Pose {
                hide_hind: true,
                haunch_at: Some(Haunch::Ellipse(84.0, 88.0, 34.0, 27.0, -12.0)),
                bx: 112.0,
                by: 60.0,
                brx: 21.0,
                bry: 25.0,
                brot: 22.0,
                yaw: 0.8,
                show_far_legs: false,
                fl_root: Some((124.0, 74.0)),
                fl_near: leg4(-4.0, 3.0, 21.0, 16.0),
                fl_far: leg4(8.0, -4.0, 21.0, 16.0),
                ear_near: 10.0,
                ear_far: -8.0,
                bar_site: "haunch",
                whisker_far: false,
                tail_root: Some((74.0, 112.0)),
                tail: [-72.0, -88.0, -99.0, -107.0],
                tail_len: 9.0,
                tail_thick: 7.5,
                ..d(ident, note)
            }
        }

        fn address() -> Vec<Pose> {
            vec![
                sit_front(
                    "pet_sit_front",
                    "Sit, face-on: square to the viewer, tail swept round beside the mitts.",
                ),
                Pose {
                    eyes: "closed",
                    ..sit_front(
                        "pet_sit_front_blink",
                        "Sit, face-on, blink: eye contact, both lids down.",
                    )
                },
                Pose {
                    hx: 116.0,
                    hy: 40.0,
                    hrot: -6.0,
                    ..shoulder_seat(
                        "pet_peek_shoulder",
                        "Peek: seated facing away, head turned back over the shoulder.",
                    )
                },
            ]
        }

        fn flight() -> Vec<Pose> {
            vec![
                Pose {
                    yaw: 1.0,
                    airborne: 6.0,
                    by: 62.0,
                    brx: 42.0,
                    bry: 20.0,
                    brot: 30.0,
                    hx: 141.0,
                    hy: 32.0,
                    hrot: -12.0,
                    ear_flat: 0.25,
                    eyes: "wide",
                    mouth: "open",
                    fl_near: leg4(-30.0, 60.0, 12.0, 10.0),
                    fl_far: leg4(-38.0, 52.0, 12.0, 10.0),
                    hl_near: leg4(-46.0, -56.0, 20.0, 16.0),
                    hl_far: leg4(-36.0, -48.0, 20.0, 16.0),
                    tail: [-45.0, -56.0, -68.0, -80.0],
                    tail_len: 15.0,
                    ..d(
                        "pet_leap_rise",
                        "Flight, rise: hind legs still driving, forepaws tucked, nose up.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    airborne: 7.0,
                    by: 58.0,
                    brx: 42.0,
                    bry: 20.0,
                    brot: -20.0,
                    hx: 142.0,
                    hy: 61.0,
                    hrot: 14.0,
                    ear_flat: 0.35,
                    eyes: "wide",
                    mouth: "smile",
                    gaze: (1.5, 2.0),
                    fl_near: leg4(28.0, 8.0, 20.0, 16.0),
                    fl_far: leg4(20.0, 2.0, 20.0, 16.0),
                    hl_near: leg4(-62.0, -80.0, 16.0, 12.0),
                    hl_far: leg4(-52.0, -70.0, 16.0, 12.0),
                    far_eye: Some([-1.1, -2.4, 1.238, 1.143]),
                    tail: [-125.0, -140.0, -155.0, -170.0],
                    tail_len: 14.5,
                    ..d(
                        "pet_leap_descend",
                        "Flight, descent: forelegs reaching for the floor, rump high.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    airborne: 9.0,
                    by: 58.0,
                    brx: 40.0,
                    bry: 24.0,
                    brot: 0.0,
                    hx: 150.0,
                    hy: 34.0,
                    hrot: -6.0,
                    ear_flat: 0.4,
                    eyes: "wide",
                    mouth: "open",
                    gaze: (1.0, 1.0),
                    fl_near: leg4(24.0, 74.0, 13.0, 10.0),
                    fl_far: leg4(14.0, 66.0, 13.0, 10.0),
                    hl_near: leg4(-18.0, -62.0, 15.0, 12.0),
                    hl_far: leg4(-10.0, -56.0, 15.0, 12.0),
                    tail: [-88.0, -104.0, -122.0, -140.0],
                    tail_len: 15.0,
                    ..d(
                        "pet_apex",
                        "Flight, apex: the top of the bound — the back rounds, the hind legs swing under.",
                    )
                },
            ]
        }

        /// The awake bread: one smooth arc, zero visible limbs, the head
        /// RESTING on the dome's front end (leading edge flush with the
        /// bread's), the tail trimmed to a nub-and-tip, and the off-side fan
        /// flat against the bread.
        fn loaf_base(p: Pose) -> Pose {
            Pose {
                curl: true,
                hide_legs: true,
                show_far_legs: false,
                bx: 101.0,
                by: 87.0,
                brx: 45.5,
                bry: 27.5,
                hx: 128.0,
                hy: 60.0,
                hrot: 0.0,
                ear_flat: 0.12,
                mouth: "smile",
                whisker_far: false,
                tail_root: Some((68.0, 98.0)),
                tail: [-62.0, -82.0, -100.0, -114.0],
                tail_len: 8.5,
                tail_thick: 7.5,
                ..p
            }
        }

        fn low() -> Vec<Pose> {
            vec![
                Pose {
                    yaw: 1.0,
                    by: 76.0,
                    brx: 41.0,
                    bry: 21.0,
                    brot: -8.0,
                    bx: 94.0,
                    hy: 52.5,
                    hx: 151.5,
                    eyes: "wide",
                    ear_near: -8.0,
                    ear_far: -6.0,
                    fl_root: Some((114.7, 87.8)),
                    fl_near: leg4(14.0, -20.0, 15.0, 12.0),
                    fl_far: leg4(6.0, -26.0, 15.0, 12.0),
                    hl_near: leg4(40.0, -22.0, 16.0, 12.0),
                    hl_far: leg4(32.0, -28.0, 16.0, 12.0),
                    tail: [-134.0, -147.0, -157.0, -164.0],
                    tail_len: 13.5,
                    ..d(
                        "pet_crouch_wiggle",
                        "Pounce, wiggle: the crouch with its hindquarters up and offset.",
                    )
                },
                Pose {
                    eyes: "happy",
                    ..loaf_base(d(
                        "pet_loaf",
                        "Loaf: settled flat, paws tucked, eyes soft, tail hugging the side.",
                    ))
                },
            ]
        }

        fn roll() -> Vec<Pose> {
            vec![
                Pose {
                    by: 84.0,
                    bx: 96.0,
                    brx: 40.0,
                    bry: 24.0,
                    brot: 150.0,
                    hx: 150.0,
                    hy: 76.0,
                    hr: 27.0,
                    hrot: -18.0,
                    eyes: "happy",
                    mouth: "smile",
                    ear_flat: 0.30,
                    whisker_far: false,
                    fl_near: leg4(150.0, -175.0, 15.0, 11.0),
                    fl_far: leg4(162.0, -160.0, 13.0, 10.0),
                    hl_near: leg4(-160.0, 172.0, 16.0, 12.0),
                    hl_far: leg4(-172.0, 158.0, 14.0, 11.0),
                    tail_root: Some((58.0, 96.0)),
                    tail: [-95.0, -82.0, -70.0, -58.0],
                    tail_len: 12.0,
                    tail_thick: 8.0,
                    ..d(
                        "pet_roll_0",
                        "Roll, flop: hip over, shoulders following, paws folding skyward.",
                    )
                },
                Pose {
                    by: 86.0,
                    bx: 96.0,
                    brx: 41.0,
                    bry: 25.0,
                    brot: 180.0,
                    hx: 152.0,
                    hy: 84.0,
                    hr: 27.0,
                    hrot: -30.0,
                    eyes: "happy",
                    mouth: "open",
                    ear_flat: 0.40,
                    whisker_far: false,
                    fl_near: leg4(174.0, -150.0, 16.0, 12.0),
                    fl_far: leg4(-172.0, 146.0, 14.0, 10.0),
                    hl_near: leg4(-178.0, 154.0, 17.0, 12.0),
                    hl_far: leg4(170.0, -148.0, 15.0, 11.0),
                    tail_root: Some((56.0, 100.0)),
                    tail: [-88.0, -98.0, -108.0, -96.0],
                    tail_len: 12.5,
                    tail_thick: 8.0,
                    ..d(
                        "pet_roll_1",
                        "Roll, belly-up: flat on the back, paws loose in the air, bliss.",
                    )
                },
                Pose {
                    by: 84.0,
                    bx: 96.0,
                    brx: 40.0,
                    bry: 24.0,
                    brot: 210.0,
                    hx: 150.0,
                    hy: 80.0,
                    hr: 27.0,
                    hrot: -40.0,
                    eyes: "happy",
                    mouth: "smile",
                    ear_flat: 0.35,
                    whisker_far: false,
                    fl_near: leg4(-166.0, 168.0, 15.0, 11.0),
                    fl_far: leg4(-152.0, 178.0, 13.0, 10.0),
                    hl_near: leg4(168.0, -158.0, 16.0, 12.0),
                    hl_far: leg4(155.0, -170.0, 14.0, 11.0),
                    tail_root: Some((58.0, 96.0)),
                    tail: [-100.0, -112.0, -122.0, -110.0],
                    tail_len: 12.0,
                    tail_thick: 8.0,
                    ..d(
                        "pet_roll_2",
                        "Roll, over-twist: hips past vertical, the squirm's far side.",
                    )
                },
            ]
        }

        const LW_FWD: Leg = leg(19.0, 10.0);
        const LW_MID: Leg = leg(1.0, -1.0);
        const LW_BACK: Leg = leg(-18.0, -6.0);
        const LW_SWING: Leg = leg(10.0, -58.0);

        fn lwalk() -> Vec<Pose> {
            vec![
                Pose {
                    yaw: 1.0,
                    hl_near: LW_SWING,
                    fl_near: LW_BACK,
                    hl_far: LW_MID,
                    fl_far: LW_FWD,
                    by: 64.0,
                    hy: 41.0,
                    tail: [-108.0, -136.0, -168.0, -202.0],
                    ..d(
                        "pet_lwalk_0",
                        "Lateral walk: near-hind swings under, far-fore leads.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    hl_near: LW_FWD,
                    fl_near: LW_SWING,
                    hl_far: LW_BACK,
                    fl_far: LW_MID,
                    by: 63.5,
                    hy: 42.0,
                    tail: [-114.0, -142.0, -174.0, -208.0],
                    ..d(
                        "pet_lwalk_1",
                        "Lateral walk: near-fore swings, near-hind just planted ahead.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    hl_near: LW_MID,
                    fl_near: LW_FWD,
                    hl_far: LW_SWING,
                    fl_far: LW_BACK,
                    by: 64.0,
                    hy: 41.0,
                    tail: [-120.0, -148.0, -180.0, -214.0],
                    ..d(
                        "pet_lwalk_2",
                        "Lateral walk: far-hind swings, near-fore reaches ahead.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    hl_near: LW_BACK,
                    fl_near: LW_MID,
                    hl_far: LW_FWD,
                    fl_far: LW_SWING,
                    by: 63.5,
                    hy: 42.0,
                    tail: [-114.0, -142.0, -174.0, -208.0],
                    ..d(
                        "pet_lwalk_3",
                        "Lateral walk: far-fore swings, near side trails.",
                    )
                },
            ]
        }

        fn loco() -> Vec<Pose> {
            vec![
                Pose {
                    yaw: 1.0,
                    by: 68.0,
                    brx: 42.0,
                    bry: 21.0,
                    brot: -25.0,
                    hx: 146.0,
                    hy: 43.0,
                    hrot: -8.0,
                    ear_flat: 0.2,
                    eyes: "wide",
                    mouth: "open",
                    fl_near: leg4(30.0, 14.0, 19.0, 15.0),
                    fl_far: leg4(22.0, 6.0, 19.0, 15.0),
                    hl_near: leg4(-18.0, -32.0, 19.0, 15.0),
                    hl_far: leg4(-26.0, -40.0, 19.0, 15.0),
                    tail: [-60.0, -72.0, -84.0, -96.0],
                    tail_len: 14.5,
                    ..d(
                        "pet_launch",
                        "Pounce, launch: hind legs driving off the floor, chest up, forepaws lifting.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    airborne: 6.0,
                    by: 62.0,
                    brx: 37.0,
                    bry: 24.0,
                    brot: -8.0,
                    hx: 148.0,
                    hy: 40.0,
                    hrot: -2.0,
                    ear_flat: 0.3,
                    eyes: "wide",
                    mouth: "smile",
                    gaze: (1.0, 1.0),
                    fl_near: leg4(-36.0, 46.0, 13.0, 10.0),
                    fl_far: leg4(-44.0, 38.0, 13.0, 10.0),
                    hl_near: leg4(48.0, -44.0, 14.0, 11.0),
                    hl_far: leg4(40.0, -52.0, 14.0, 11.0),
                    tail: [-128.0, -144.0, -156.0, -166.0],
                    tail_len: 13.5,
                    ..d(
                        "pet_hop",
                        "Hop: a tight tuck, every paw gathered, tail up, eyes on the landing.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    by: 78.0,
                    brx: 42.0,
                    bry: 21.0,
                    brot: -14.0,
                    hx: 156.0,
                    hy: 50.0,
                    hrot: -4.0,
                    ear_flat: 0.85,
                    eyes: "wide",
                    mouth: "flat",
                    fl_near: leg4(34.0, 20.0, 20.0, 16.0),
                    fl_far: leg4(26.0, 12.0, 20.0, 16.0),
                    hl_near: leg4(48.0, -56.0, 18.0, 14.0),
                    hl_far: leg4(40.0, -64.0, 18.0, 14.0),
                    tail: [-140.0, -160.0, -185.0, -210.0],
                    tail_len: 14.0,
                    ..d(
                        "pet_skid",
                        "Skid: forelegs braced ahead, rump dropped, ears pinned — the drift-brake.",
                    )
                },
            ]
        }

        fn idle() -> Vec<Pose> {
            vec![
                Pose {
                    by: 84.0,
                    brx: 42.0,
                    bry: 20.0,
                    brot: 0.0,
                    hx: 150.0,
                    hy: 68.0,
                    hrot: 14.0,
                    ear_flat: 1.0,
                    ear_near: -10.0,
                    ear_far: -8.0,
                    eyes: "halflid",
                    mouth: "flat",
                    blush: false,
                    fl_near: leg4(20.0, -20.0, 12.0, 10.0),
                    fl_far: leg4(12.0, -26.0, 12.0, 10.0),
                    hl_near: leg4(30.0, -30.0, 12.0, 10.0),
                    hl_far: leg4(22.0, -36.0, 12.0, 10.0),
                    tail_root: Some((50.0, 92.0)),
                    tail: [-35.0, -65.0, -90.0, -95.0],
                    tail_len: 13.0,
                    tail_thick: 8.0,
                    ..d(
                        "pet_droop",
                        "Droop: the sulk — flat to the floor, ears pinned, head hung, tail limp behind.",
                    )
                },
                Pose {
                    hy: 50.0,
                    hx: 131.0,
                    hrot: 12.0,
                    brot: -4.0,
                    ear_flat: 1.0,
                    eyes: "halflid",
                    mouth: "flat",
                    blush: false,
                    tail: [88.0, 84.0, 82.0, 80.0],
                    ..sat(
                        "pet_droop_sit",
                        "Droop, seated: the seat, head hung, ears pinned, tail flat forward.",
                    )
                },
                Pose {
                    ear_near: 70.0,
                    ..sat(
                        "pet_sit_ear",
                        "Sit, ear flick: the near ear swivelled out sideways.",
                    )
                },
                Pose {
                    ear_far: -60.0,
                    ..sat("pet_sit_ear_far", "Sit, ear flick: the far ear swung out.")
                },
                Pose {
                    ear_near: 70.0,
                    ..d(
                        "pet_stand_ear",
                        "Stand, ear flick: the near ear swivelled out sideways.",
                    )
                },
                Pose {
                    eyes: "happy",
                    ear_near: 55.0,
                    ..loaf_base(d(
                        "pet_loaf_ear",
                        "Loaf, ear flick: the near ear swivelled out.",
                    ))
                },
                Pose {
                    eyes: "closed",
                    ..sat("pet_sit_blink", "Sit, blink: the seat with both lids down.")
                },
                Pose {
                    eyes: "happy",
                    mouth: "yawn",
                    hrot: -18.0,
                    hy: 36.0,
                    hx: 133.0,
                    ear_flat: 0.35,
                    ..sat(
                        "pet_yawn",
                        "Yawn: head tipped back, eyes squeezed, the gape wide.",
                    )
                },
                Pose {
                    tail_root: Some((80.0, 108.0)),
                    tail: [-85.0, -90.0, -98.0, -110.0],
                    ..sat(
                        "pet_sit_tail_low",
                        "Sit, tail behind: swept back along the floor.",
                    )
                },
                Pose {
                    tail_root: Some((78.0, 106.0)),
                    tail: [-95.0, -115.0, -150.0, -178.0],
                    ..sat("pet_sit_tail_back", "Sit, tail behind, tip flicked up.")
                },
                Pose {
                    eyes: "open",
                    gaze: (0.6, 0.4),
                    ..loaf_base(d("pet_loaf_open", "Loaf, eyes open: the drowsy check."))
                },
                Pose {
                    hrot: -20.0,
                    hy: 36.0,
                    gaze: (0.6, -2.4),
                    ear_near: 4.0,
                    ear_far: 2.0,
                    ..sat(
                        "pet_sit_lookup",
                        "Sit, look up: chin lifted, pupils raised to the line above.",
                    )
                },
                Pose {
                    hrot: 16.0,
                    hy: 42.0,
                    gaze: (0.4, 2.2),
                    ..sat(
                        "pet_sit_lookdown",
                        "Sit, look down: chin tucked, pupils dropped to the line below.",
                    )
                },
                Pose {
                    hx: 112.0,
                    hy: 52.0,
                    hrot: 14.0,
                    eyes: "happy",
                    mouth: "open",
                    ..shoulder_seat(
                        "pet_groom_flank",
                        "Groom, flank: seated facing away, head turned back and down into the shoulder.",
                    )
                },
                Pose {
                    yaw: 0.6,
                    by: 72.0,
                    brx: 44.0,
                    bry: 20.0,
                    brot: 10.0,
                    hy: 50.0,
                    hx: 158.0,
                    hrot: -4.0,
                    eyes: "happy",
                    mouth: "smile",
                    fl_near: leg4(4.0, -2.0, 16.0, 12.0),
                    fl_far: leg4(-4.0, 4.0, 16.0, 12.0),
                    hl_near: leg4(-45.0, -42.0, 24.0, 22.0),
                    hl_far: leg4(-10.0, 10.0, 18.0, 14.0),
                    tail: [-140.0, -155.0, -165.0, -170.0],
                    tail_len: 14.0,
                    ..d(
                        "pet_stretch_hind",
                        "Stretch, hind: forepaws planted, rump up, a hind leg reaching straight back.",
                    )
                },
                Pose {
                    yaw: 1.0,
                    by: 63.0,
                    hy: 39.0,
                    hx: 152.0,
                    eyes: "wide",
                    ear_near: 6.0,
                    ear_far: 4.0,
                    fl_near: leg(2.0, -1.0),
                    fl_far: leg(-6.0, 2.0),
                    hl_near: leg(-6.0, 6.0),
                    hl_far: leg(4.0, -4.0),
                    tail: [-172.0, -182.0, -190.0, -198.0],
                    tail_len: 14.5,
                    ..d(
                        "pet_perk_turn",
                        "Alert, turned: the perk with the head yawed into the facing.",
                    )
                },
            ]
        }

        /// The supported edge seat the edge family departs from.
        fn edge_seat(p: Pose) -> Pose {
            Pose {
                by: 62.0,
                brx: 26.0,
                bry: 25.0,
                haunch_at: Some(Haunch::Circle(83.0, 88.0, 26.0)),
                fl_near: leg4(24.0, -24.0, 20.0, 18.0),
                fl_far: leg4(-7.0, 4.0, 20.0, 18.0),
                tail_root: Some((67.0, 92.0)),
                tail: [-158.0, -120.0, -68.0, -12.0],
                tail_len: 11.0,
                whisker_far: false,
                ..p
            }
        }

        fn contact() -> Vec<Pose> {
            let edge_perch = Pose {
                bx: 102.0,
                brot: -4.0,
                hx: 138.0,
                hy: 41.0,
                hrot: 6.0,
                yaw: 0.6,
                fl_root: Some((124.0, 77.0)),
                gaze: (1.0, 1.0),
                ..edge_seat(sat(
                    "pet_edge_perch",
                    "Safe block edge: a compact supported seat, forepaws together at the lip and tail counterbalancing behind.",
                ))
            };
            let look_up = Pose {
                ident: "pet_edge_look_up".to_string(),
                note: "Reading above: only the head lifts and pupils rise; the supported perch remains planted."
                    .to_string(),
                hrot: -20.0,
                gaze: (1.0, -2.5),
                ..edge_perch.clone()
            };
            let look_down = Pose {
                ident: "pet_edge_look_down".to_string(),
                note: "Reading below: only the head dips and pupils lower; the supported perch remains planted."
                    .to_string(),
                hrot: 25.0,
                gaze: (1.0, 2.5),
                whisker_near: false,
                ..edge_perch.clone()
            };
            vec![
                Pose {
                    yaw: 0.8,
                    bx: 92.0,
                    by: 76.0,
                    brx: 39.0,
                    bry: 22.0,
                    brot: -7.0,
                    hx: 139.0,
                    hy: 48.0,
                    hrot: -12.0,
                    eyes: "wide",
                    mouth: "oof",
                    ear_flat: 0.72,
                    fl_root: Some((121.0, 86.0)),
                    fl_near: leg4(48.0, 12.0, 19.0, 15.0),
                    fl_far: leg4(35.0, 8.0, 19.0, 15.0),
                    hl_near: leg4(-30.0, 12.0, 17.0, 14.0),
                    hl_far: leg4(-20.0, 6.0, 17.0, 14.0),
                    tail_root: Some((55.0, 73.0)),
                    tail: [20.0, 72.0, 140.0, 192.0],
                    tail_len: 10.5,
                    tail_thick: 8.0,
                    ..d(
                        "pet_contact_brace",
                        "Ink contact: chest recoils over spread planted paws, ears back, tail curled clear.",
                    )
                },
                Pose {
                    tail_root: Some((52.0, 77.0)),
                    tail: [18.0, 66.0, 133.0, 190.0],
                    tail_len: 11.5,
                    tail_thick: 8.0,
                    gaze: (-1.2, 0.5),
                    ear_far: -12.0,
                    ..d(
                        "pet_tail_tuck",
                        "Ink clearance: the standing cat draws its long tail into a tight hook under the flank.",
                    )
                },
                Pose {
                    yaw: 0.8,
                    by: 65.0,
                    brx: 39.0,
                    bry: 22.0,
                    hx: 148.0,
                    hy: 37.0,
                    hrot: -5.0,
                    fl_near: leg4(-25.0, 60.0, 17.0, 12.0),
                    fl_far: leg(2.0, -2.0),
                    hl_near: leg(-12.0, 18.0),
                    hl_far: leg(5.0, -8.0),
                    tail: [-105.0, -138.0, -177.0, -214.0],
                    tail_len: 12.0,
                    ..d(
                        "pet_contact_recover",
                        "Contact recovery: weight returns to three paws while the near forepaw gathers for one step.",
                    )
                },
                Pose {
                    hx: 145.0,
                    hy: 55.0,
                    hrot: 27.0,
                    yaw: 0.65,
                    gaze: (1.2, 2.5),
                    ear_near: 8.0,
                    ear_far: -12.0,
                    whisker_far: false,
                    whisker_near: false,
                    blush: false,
                    ..sat(
                        "pet_inspect_down",
                        "Inspect a local edit: neck extends down over the planted forepaws, pupils aimed at the vacancy.",
                    )
                },
                Pose {
                    hx: 139.0,
                    hy: 48.0,
                    hrot: 18.0,
                    yaw: 0.7,
                    gaze: (1.2, 2.0),
                    eyes: "open",
                    mouth: "smile",
                    fl_near: leg4(65.0, 78.0, 32.0, 28.0),
                    whisker_far: false,
                    whisker_near: false,
                    ..sat(
                        "pet_reach_paw",
                        "Validated edit vacancy: one long forepaw reaches forward and down while the other supports the same seat.",
                    )
                },
                Pose {
                    hx: 139.0,
                    hy: 48.0,
                    hrot: 18.0,
                    yaw: 0.7,
                    gaze: (1.2, 2.0),
                    eyes: "open",
                    mouth: "smile",
                    fl_near: leg4(-20.0, 100.0, 17.0, 15.0),
                    whisker_far: false,
                    whisker_near: false,
                    ..sat(
                        "pet_withdraw_paw",
                        "Replacement arrives: the reaching forepaw folds back under the chest; the supported seat stays put.",
                    )
                },
                edge_perch,
                Pose {
                    bx: 108.0,
                    by: 63.0,
                    brot: -15.0,
                    hx: 153.0,
                    hy: 57.0,
                    hrot: 24.0,
                    yaw: 0.8,
                    fl_root: Some((129.0, 77.0)),
                    gaze: (1.2, 2.5),
                    whisker_near: false,
                    blush: false,
                    ..edge_seat(sat(
                        "pet_edge_lean",
                        "Inspect below an edge: head and shoulder lean beyond the planted mitts, tail opposing the weight.",
                    ))
                },
                look_up,
                look_down,
            ]
        }

        fn cat_poses() -> Vec<Pose> {
            [
                vec![stand()],
                walk(),
                run(),
                settled(),
                sleep(),
                reactive(),
                address(),
                flight(),
                low(),
                roll(),
                lwalk(),
                loco(),
                idle(),
                contact(),
            ]
            .concat()
        }

        /// THE DOG IS THE SAME POSE SHEET, RE-SKINNED (owner, 2026-08-11:
        /// "make a dog like the walking cat … you can use the same code").
        fn dog_poses(cats: &[Pose]) -> Vec<Pose> {
            cats.iter()
                .map(|p| Pose {
                    ident: p.ident.replacen("pet_", "pet_dog_", 1),
                    species: "dog",
                    note: format!("{} (dog)", p.note),
                    ..p.clone()
                })
                .collect()
        }

        pub fn main(out: &std::path::Path) -> Result<(), String> {
            std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
            let cats = cat_poses();
            let dogs = dog_poses(&cats);
            let mut bad = 0;
            for p in cats.iter().chain(&dogs) {
                let text = emit(p);
                super::super::write(&out.join(format!("{}.toml", p.ident)), &text)?;
                let [x0, y0, x1, y1] = bbox(&build(p));
                let ok = x0 >= 0.0 && y0 >= 0.0 && x1 <= VB_W && y1 <= VB_H;
                if !ok {
                    bad += 1;
                }
                let cmds: usize = ["M ", "L ", "C ", " Z"]
                    .iter()
                    .map(|c| text.matches(c).count())
                    .sum();
                println!(
                    "{} {:<20} ({x0:6.1},{y0:6.1})-({x1:6.1},{y1:6.1}) layers={:2} cmds={cmds:3}",
                    if ok { "ok " } else { "OOB" },
                    p.ident,
                    text.matches("[[layer]]").count(),
                );
            }
            super::super::write(&out.join("sheet.toml"), &sheet(&cats, 5, "pet_sheet"))?;
            super::super::write(
                &out.join("dog_sheet.toml"),
                &sheet(&dogs, 5, "pet_dog_sheet"),
            )?;
            println!(
                "{} poses ({} cat + {} dog), {bad} out of box; sheets written",
                cats.len() + dogs.len(),
                cats.len(),
                dogs.len()
            );
            Ok(())
        }
    }
}
