// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings control model and the chrome primitives the in-grid overlays share: the
//! control catalogue ([`SettingsState`]), the overlay geometry and colour roles
//! ([`SettingsGeom`], [`Roles`]), the row writers ([`blank_row`], [`write_str`]), and the
//! aurora and rainbow-banner painters. Native Settings pixels, input and accessibility
//! compile from [`crate::native_ui`].

use aterm_core::terminal::RenderCell;

use crate::app_config::Config;
use crate::chrome_band;
use crate::prefs::{self, EditField, EditKind};
use crate::widget::{DrawPrim, TextWeight, rgba};

/// Shared semantic state embedded in each native Settings tab: the editable control
/// catalogue plus the search fields the native view reads through `legacy`.
pub(crate) struct SettingsState {
    /// The editable controls for the live config, in row order. Native Settings rebuilds
    /// it from its config snapshot after every persisted change.
    pub(crate) fields: Vec<EditField>,
}

impl SettingsState {
    /// Build from the current config generation. This constructor is
    /// intentionally IO-free.
    pub(crate) fn from_config(cfg: &Config) -> Self {
        Self {
            fields: prefs::editable_fields(cfg),
        }
    }

    /// The displayed value for a row: the configured seed, else the effective placeholder
    /// (an unset key shows what is in effect, with its `"(default)"` / `"(follow OS)"`
    /// note). Enum aliases project onto their canonical option so the picker does not
    /// show a valid alias as a custom value; unknown and dynamic values stay verbatim.
    pub(crate) fn display_value(f: &EditField) -> &str {
        let raw = f.seed.as_deref().unwrap_or(f.placeholder.as_str());
        if let Some(seed) = f.seed.as_deref()
            && matches!(f.kind, EditKind::Enum { .. })
        {
            // An off-roster but live value normalizes to its canonical name too
            // — `glass bell` reads back as `music box`, the same way an
            // offered alias reads back as its picker row.
            let token = seed.trim();
            enum_recognized(f, token)
                .or_else(|| enum_offered_or_runtime(f.key, token))
                .unwrap_or(raw)
        } else {
            raw
        }
    }
}

/// Resolve a documented config alias to its canonical option. The loaders (`app_config`)
/// accept aliases outside the picker's option set (cursor_style `beam` == `bar`), which
/// native Settings would otherwise show as a custom value. `None` for a non-alias.
fn enum_alias(key: &str, token: &str) -> Option<&'static str> {
    // Trail-style aliases resolve through the shared `prefs` table that
    // `--validate-config` and the load-time warning also read, so they never disagree.
    let token = token.trim();
    if key == prefs::EDIT_CURSOR_TRAIL_STYLE {
        return prefs::cursor_trail_style_canonical(token);
    }
    // Typing-sound aliases (water → droplet, mech → mechanical, …) resolve through the
    // synth's own parser.
    if key == prefs::EDIT_TRAIL_SOUND_STYLE {
        return prefs::trail_sound_style_canonical(token);
    }
    Some(match (key, token.to_ascii_lowercase().as_str()) {
        (prefs::EDIT_CURSOR_STYLE, "beam") => "bar",
        (prefs::EDIT_BIDI, "off") => "disabled",
        (prefs::EDIT_BIDI, "on") => "implicit",
        (prefs::EDIT_AMBIGUOUS_WIDTH, "single") => "narrow",
        (prefs::EDIT_AMBIGUOUS_WIDTH, "double") => "wide",
        (prefs::EDIT_PREDICTIVE_ECHO, "auto" | "on" | "true") => "adaptive",
        (prefs::EDIT_PREDICTIVE_ECHO, "force") => "always",
        (prefs::EDIT_TEXT_BLENDING, "linear_corrected") => "linear-corrected",
        (prefs::EDIT_MOTION, "reduce") => "reduced",
        (prefs::EDIT_WINDOW_COLORSPACE, "displayp3" | "p3") => "display-p3",
        (prefs::EDIT_BACKGROUND_MATERIAL, "underwindow" | "under_window") => "under-window",
        (prefs::EDIT_BACKGROUND_MATERIAL, "") => "none",
        _ => return None,
    })
}

/// The canonical name of a typing-sound value the synth accepts, resolved as the runtime
/// does (`SoundVoice::parse`), whether or not the picker offers it, so the row shows the
/// voice that plays. `None` for an unknown value, which plays the default (`auto`).
fn enum_offered_or_runtime(key: &str, token: &str) -> Option<&'static str> {
    (key == prefs::EDIT_TRAIL_SOUND_STYLE)
        .then(|| prefs::trail_sound_style_canonical(token))
        .flatten()
}

/// The canonical option an Enum row's `token` resolves to (directly or via the alias map),
/// or `None` for an unrecognized spelling.
fn enum_recognized(f: &EditField, token: &str) -> Option<&'static str> {
    let EditKind::Enum { options } = f.kind else {
        return None;
    };
    options
        .iter()
        .find(|o| token.eq_ignore_ascii_case(o))
        .copied()
        .or_else(|| enum_alias(f.key, token).filter(|c| options.contains(c)))
}

/// Theme-derived colour roles shared by the overlay painters and native Settings
/// chrome: the native role vocabulary itself
/// ([`crate::native_appearance::NativeRoles`]), rebuilt from the live palette so
/// they re-tint without a hardcoded palette.
pub(crate) type Roles = crate::native_appearance::NativeRoles;

pub(crate) fn u32_rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

/// The width of `s` at `px` in the mono (terminal) face, from real advances in the chrome
/// font stack. Regular weight; measure bold runs with [`crate::tray_raster::measure_text`].
/// UI-face chrome measures with [`crate::tray_raster::ui_text_width`].
pub(crate) fn text_w(s: &str, px: f32) -> f32 {
    crate::tray_raster::measure_text(s, px, TextWeight::Regular)
}

/// `f32::clamp`, except that inverted bounds (where `clamp` panics), which overlay geometry
/// produces in extreme windows (a tiny font, an ultra-narrow grid), give `hi` floored at
/// zero: never negative, since a later `as u32` would wrap it.
#[inline]
pub(crate) fn fit(value: f32, lo: f32, hi: f32) -> f32 {
    if hi <= lo {
        hi.max(0.0)
    } else {
        value.clamp(lo, hi)
    }
}

/// Device-pixel geometry of the in-grid overlay card slot, handed to every
/// [`crate::overlay::OverlayModel::tray`]. Native Settings does not use it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SettingsGeom {
    pub cw: f32,
    pub ch: f32,
    pub font_px: f32,
    pub cols: usize,
    pub panel_rows: usize,
}

// Rainbow palette for the banner's arch and glints.

const BLOB_CORAL: [u8; 3] = [0xFF, 0x51, 0x48];
const BLOB_MARIGOLD: [u8; 3] = [0xFF, 0xC2, 0x2E];
const BLOB_LEAF: [u8; 3] = [0x2F, 0xAE, 0x5B];
const BLOB_COBALT: [u8; 3] = [0x2E, 0x5B, 0xFF];
const BLOB_VIOLET: [u8; 3] = [0xB0, 0x5C, 0xFF];

/// The rainbow arch's sixth stripe (the blob palette covers the other five).
const RAINBOW_TEAL: [u8; 3] = [0x21, 0xC2, 0xB7];

/// The aurora's tile grid: columns, rows.
const AURORA_GRID: (usize, usize) = (112, 64);
/// How far each aurora tile is poured from the surface toward its hue.
const AURORA_MIX: f32 = 0.30;
/// The contrast every aurora tile keeps against the page-chrome ink drawn on
/// it (WCAG 1.4.3 for body text).
pub(crate) const AURORA_INK_FLOOR: f32 = 4.5;
/// What a tile is solved to: a quarter step over [`AURORA_INK_FLOOR`], because
/// a small glyph's antialiased strokes never reach the full ink, and the text
/// should still measure at the floor on glass.
const AURORA_INK_TARGET: f32 = AURORA_INK_FLOOR + 0.25;

/// Tile `(i, j)` of the aurora at its full pour, before the ink hold
/// ([`aurora_tile`]): a diagonal spectrum (`hue ∝ x + y`) with a gentle value
/// falloff toward the bottom, so the content zone is calmer than the sky.
#[cfg(test)]
pub(crate) fn aurora_tile_unheld(surface: [u8; 3], i: usize, j: usize) -> [u8; 3] {
    crate::native_appearance::mix_srgb(surface, aurora_hue(i, j), AURORA_MIX)
}

fn aurora_hue(i: usize, j: usize) -> [u8; 3] {
    let (cols, rows) = AURORA_GRID;
    let hue = (i as f32 / cols as f32 + j as f32 / rows as f32) * 0.5;
    let v = 0.92 - 0.16 * (j as f32 / rows as f32);
    crate::widget::hsv_to_rgb(hue, 0.86, v)
}

/// Tile `(i, j)` of the aurora as painted: the full pour where the page-chrome
/// `ink` keeps [`AURORA_INK_TARGET`] on it, otherwise the deepest pour that
/// does. The heading, subtitle, pager and empty line sit straight on the
/// aurora, so the wash yields to them rather than the text to the wash: on a
/// dark page the bright yellow-green band pours a little shallower.
pub(crate) fn aurora_tile(surface: [u8; 3], ink: [u8; 3], i: usize, j: usize) -> [u8; 3] {
    use crate::native_appearance::contrast_ratio;
    let hue = aurora_hue(i, j);
    let full = crate::native_appearance::mix_srgb(surface, hue, AURORA_MIX);
    if contrast_ratio(ink, full) >= AURORA_INK_TARGET
        || contrast_ratio(ink, surface) < AURORA_INK_FLOOR
    {
        // Either the full pour holds, or not even the bare surface reaches the
        // floor (a palette the role contract already failed), so pouring less
        // buys nothing. A surface between the floor and the target is not
        // that case: the search below then pours nothing and the tile keeps
        // the surface's own contrast.
        return full;
    }
    let (mut lo, mut hi) = (0.0_f32, AURORA_MIX);
    for _ in 0..12 {
        let mid = (lo + hi) * 0.5;
        if contrast_ratio(ink, crate::native_appearance::mix_srgb(surface, hue, mid))
            >= AURORA_INK_TARGET
        {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    crate::native_appearance::mix_srgb(surface, hue, lo)
}

/// One palette's aurora tiles, keyed by the `(surface, ink)` pair they wash.
type AuroraTiles = (([u8; 3], [u8; 3]), Vec<[u8; 3]>);

thread_local! {
    /// The last palette's tiles: the wash is a pure function of `(surface,
    /// ink)`, and the canvas repaints with every animated preview frame.
    static AURORA_TILES: std::cell::RefCell<Option<AuroraTiles>> =
        const { std::cell::RefCell::new(None) };
}

/// The Settings surface's aurora: the wallpaper treatment, procedurally — a
/// dim DIAGONAL spectrum wash over the whole canvas, exactly the hue ramp the
/// user's rainbow terminal wears (`hue ∝ x + y`). Painted as a coarse tile
/// grid of opaque Panels over the theme surface: adjacent tiles differ by a
/// whisper of hue, so the grid fuses into a smooth wash while staying a
/// bounded, cache-friendly prim count. Deliberately STATIC.
///
/// `ink` is the quietest text painted straight on the canvas (the subtitle,
/// pager and empty line; the heading's ink is stronger): every tile holds it
/// at [`AURORA_INK_FLOOR`] ([`aurora_tile`]).
pub(crate) fn paint_settings_aurora(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    surface: [u8; 3],
    ink: [u8; 3],
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let (cols, rows) = AURORA_GRID;
    let cw = w / cols as f32;
    let ch = h / rows as f32;
    AURORA_TILES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache
            .as_ref()
            .is_none_or(|(palette, _)| *palette != (surface, ink))
        {
            let tiles = (0..rows)
                .flat_map(|j| (0..cols).map(move |i| (i, j)))
                .map(|(i, j)| aurora_tile(surface, ink, i, j))
                .collect();
            *cache = Some(((surface, ink), tiles));
        }
        let Some((_, tiles)) = cache.as_ref() else {
            return;
        };
        for j in 0..rows {
            for i in 0..cols {
                // OPAQUE tiles, pre-mixed into the surface: translucent
                // neighbours double up wherever they overlap and etch a
                // lattice into the sky; opaque paint makes the half-pixel
                // seam bleed invisible.
                prims.push(DrawPrim::Panel {
                    x: x + i as f32 * cw,
                    y: y + j as f32 * ch,
                    w: cw + 0.75,
                    h: ch + 0.75,
                    radius: 0.0,
                    fill: rgba(tiles[j * cols + i], 0xFF),
                });
            }
        }
    });
}

/// The native Settings hero banner: a pastel rainbow arch cresting through a few star
/// glints. Static (it never asks for a frame); painted through the audited custom-node
/// lowering (`native_ui::RAINBOW_BANNER_AUDIT`) and clipped to its rect.
///
/// `sky` and `rim` come resolved from the caller's role palette, so the card follows the
/// chrome appearance. The arch and glints keep their spectrum in every theme; only their
/// alpha follows the sky, deeper over a dark one so the wash reads the same.
pub(crate) fn paint_rainbow_banner(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    sky: [u8; 3],
    rim: [u8; 3],
) {
    let dark_sky = crate::native_appearance::surface_is_dark(sky);
    let (arch_alpha, glint_alpha) = if dark_sky { (0x86, 0xC4) } else { (0x46, 0x6E) };
    prims.push(DrawPrim::Panel {
        x,
        y,
        w,
        h,
        radius: 12.0,
        fill: rgba(sky, 0xFF),
    });
    prims.push(DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius: 12.0,
        width: 1.0,
        color: rgba(rim, 0xFF),
    });
    prims.push(DrawPrim::ClipPush { x, y, w, h });
    // The rainbow arch, cresting inside the banner from its bottom edge.
    let stripes = [
        BLOB_CORAL,
        BLOB_MARIGOLD,
        BLOB_LEAF,
        RAINBOW_TEAL,
        BLOB_COBALT,
        BLOB_VIOLET,
    ];
    let (acx, acy) = (x + w * 0.5, y + h * 1.12);
    let r0 = h * 0.58;
    let st = (h * 0.055).max(3.0);
    for (i, c) in stripes.iter().enumerate() {
        let r = (r0 + (stripes.len() - 1 - i) as f32 * st).max(1.0);
        let step = ((st * 0.8) / (std::f32::consts::PI * r)).max(1.0 / 512.0);
        let mut a = 0.0f32;
        while a <= 1.0 {
            let th = std::f32::consts::PI * (1.0 + a);
            prims.push(DrawPrim::Dot {
                cx: acx + th.cos() * r,
                cy: acy + th.sin() * r,
                r: st * 0.62,
                color: rgba(*c, arch_alpha),
            });
            a += step;
        }
    }
    // A small static constellation in the sky, both sides of the arch.
    let glints: [(f32, f32, f32, [u8; 3]); 6] = [
        (0.070, 0.30, 0.16, BLOB_MARIGOLD),
        (0.155, 0.62, 0.11, BLOB_VIOLET),
        (0.330, 0.24, 0.13, RAINBOW_TEAL),
        (0.660, 0.26, 0.12, BLOB_CORAL),
        (0.845, 0.58, 0.11, BLOB_LEAF),
        (0.930, 0.28, 0.15, BLOB_VIOLET),
    ];
    for (fx, fy, sc, c) in glints {
        let (sx, sy) = (x + fx * w, y + fy * h);
        let len = h * sc;
        prims.push(DrawPrim::Stroke {
            x: sx - len,
            y: sy - 0.75,
            w: len * 2.0,
            h: 1.5,
            radius: 0.75,
            width: 1.5,
            color: rgba(c, glint_alpha),
        });
        prims.push(DrawPrim::Stroke {
            x: sx - 0.75,
            y: sy - len,
            w: 1.5,
            h: len * 2.0,
            radius: 0.75,
            width: 1.5,
            color: rgba(c, glint_alpha),
        });
    }
    prims.push(DrawPrim::ClipPop);
}

/// The System State animated hero: a Rainbow Kitty Village & Factory where 12 kitty cottages
/// (one per workstation/NUX stage) are assembled along a rainbow conveyor track while kitties
/// trot across the village.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_rainbow_kitty_village(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    sky: [u8; 3],
    rim: [u8; 3],
    spec: &str,
) {
    paint_rainbow_banner(prims, x, y, w, h, sky, rim);
    if w <= 40.0 || h <= 20.0 {
        return;
    }
    let mut phase_ms = 0u64;
    let mut busy = false;
    let mut states = ['G'; 12];
    for part in spec.split(';') {
        if let Some(val) = part.strip_prefix("phase=") {
            phase_ms = val.parse::<u64>().unwrap_or(0);
        } else if let Some(val) = part.strip_prefix("busy=") {
            busy = val == "1";
        } else if let Some(val) = part.strip_prefix("states=") {
            for (idx, ch) in val.chars().take(12).enumerate() {
                states[idx] = ch;
            }
        }
    }
    let dark_sky = crate::native_appearance::surface_is_dark(sky);
    let stripes = [
        BLOB_CORAL,
        BLOB_MARIGOLD,
        BLOB_LEAF,
        RAINBOW_TEAL,
        BLOB_COBALT,
        BLOB_VIOLET,
    ];
    prims.push(DrawPrim::ClipPush { x, y, w, h });

    // 1. Rainbow factory conveyor belt & village lane along the bottom edge.
    let ground_h = (h * 0.16).clamp(5.0, 9.0);
    let ground_y = y + h - ground_h;
    prims.push(DrawPrim::Panel {
        x: x + 4.0,
        y: ground_y,
        w: (w - 8.0).max(1.0),
        h: ground_h,
        radius: ground_h * 0.45,
        fill: rgba(rim, if dark_sky { 0xD8 } else { 0xB8 }),
    });
    let roller_step = 18.0_f32;
    let speed = if busy { 0.045 } else { 0.015 };
    let roller_shift = ((phase_ms as f32 * speed) % roller_step).max(0.0);
    let mut rx = x + 8.0 + roller_shift;
    let mut r_idx = 0usize;
    while rx < x + w - 8.0 {
        let c = stripes[r_idx % stripes.len()];
        prims.push(DrawPrim::Dot {
            cx: rx,
            cy: ground_y + ground_h * 0.5,
            r: (ground_h * 0.28).max(1.4),
            color: rgba(c, if dark_sky { 0xE0 } else { 0xC8 }),
        });
        rx += roller_step;
        r_idx += 1;
    }

    // 2. 12 Kitty Village Cottages / Factory Stations (one per Setting::ALL item).
    let pad_x = 14.0_f32;
    let usable_w = (w - pad_x * 2.0).max(48.0);
    let slot_w = usable_w / 12.0;
    let house_w = (slot_w * 0.62).clamp(8.0, 22.0);
    let max_house_h = (h - ground_h - 12.0).clamp(10.0, 22.0);

    for (i, state) in states.iter().copied().enumerate() {
        let hue = stripes[i % stripes.len()];
        let cx = x + pad_x + (i as f32 + 0.5) * slot_w;
        let pulse = ((phase_ms as f32 * 0.008 + i as f32 * 0.7).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
        let build_scale = match state {
            'G' => 1.0,
            'W' => 0.58 + 0.28 * pulse,
            'D' => 0.45,
            'E' => 0.75,
            _ => 0.30,
        };
        let hh = (max_house_h * build_scale).max(5.0);
        let hx = cx - house_w * 0.5;
        let hy = ground_y - hh;
        let body_alpha = match state {
            'G' => {
                if dark_sky {
                    0xEC
                } else {
                    0xDC
                }
            }
            'W' => {
                if dark_sky {
                    0xC8
                } else {
                    0xB4
                }
            }
            'E' => 0xE0,
            'D' => 0x78,
            _ => 0x42,
        };
        let body_color = if state == 'E' { BLOB_CORAL } else { hue };

        // Cottage body or blueprint frame.
        if matches!(state, 'G' | 'W' | 'E') {
            prims.push(DrawPrim::Panel {
                x: hx,
                y: hy + hh * 0.28,
                w: house_w,
                h: hh * 0.72,
                radius: 3.0,
                fill: rgba(body_color, body_alpha),
            });
            // Roof cap.
            prims.push(DrawPrim::Panel {
                x: hx - 1.5,
                y: hy + hh * 0.16,
                w: house_w + 3.0,
                h: (hh * 0.22).max(2.5),
                radius: 2.0,
                fill: rgba(
                    crate::native_appearance::mix_srgb(body_color, [0xFF, 0xFF, 0xFF], 0.22),
                    body_alpha,
                ),
            });
            // Kitty-ear roof peaks on left and right of the cottage roof!
            let ear_r = (house_w * 0.16).clamp(1.6, 3.2);
            for ex in [hx + ear_r * 1.1, hx + house_w - ear_r * 1.1] {
                prims.push(DrawPrim::Dot {
                    cx: ex,
                    cy: hy + hh * 0.14,
                    r: ear_r,
                    color: rgba(body_color, body_alpha),
                });
            }
            // Warm glowing kitty window inside the cottage.
            if hh >= 10.0 {
                let win_w = (house_w * 0.34).clamp(2.5, 6.0);
                let win_h = (hh * 0.28).clamp(2.5, 5.5);
                prims.push(DrawPrim::Panel {
                    x: cx - win_w * 0.5,
                    y: hy + hh * 0.48,
                    w: win_w,
                    h: win_h,
                    radius: 1.2,
                    fill: rgba(
                        if state == 'E' {
                            [0xFF, 0xEE, 0xEE]
                        } else {
                            BLOB_MARIGOLD
                        },
                        if state == 'G' { 0xF8 } else { 0xB8 },
                    ),
                });
            }
            // Chimney puff / assembly spark floating above active or completed cottages.
            if state == 'G' || (state == 'W' && busy) {
                let puff_phase =
                    ((phase_ms as f32 * 0.004 + i as f32 * 0.31) % 1.0).clamp(0.0, 1.0);
                let puff_y = (hy - 2.0 - puff_phase * 6.0).max(y + 2.5);
                let puff_a = ((1.0 - puff_phase) * 210.0) as u8;
                prims.push(DrawPrim::Dot {
                    cx: cx + house_w * 0.22,
                    cy: puff_y,
                    r: 1.6 + puff_phase * 1.1,
                    color: rgba(if state == 'W' { BLOB_MARIGOLD } else { hue }, puff_a),
                });
            }
        } else {
            // Blueprint outline waiting for assembly.
            prims.push(DrawPrim::Stroke {
                x: hx,
                y: hy + hh * 0.25,
                w: house_w,
                h: hh * 0.75,
                radius: 2.5,
                width: 1.0,
                color: rgba(body_color, body_alpha),
            });
        }
    }

    // 3. Animated Rainbow Kitties trotting across the village path!
    let kitty_colors: [[u8; 3]; 4] = [BLOB_CORAL, BLOB_MARIGOLD, RAINBOW_TEAL, BLOB_VIOLET];
    let kitty_speed = if busy { 0.055 } else { 0.018 };
    let path_w = (w - 36.0).max(24.0);
    for (k, &kcol) in kitty_colors.iter().enumerate() {
        let raw = (phase_ms as f32 * kitty_speed + (k as f32) * (path_w * 0.27)) % (path_w * 2.0);
        let (kx, dir) = if raw <= path_w {
            (x + 18.0 + raw, 1.0_f32)
        } else {
            (x + 18.0 + (path_w * 2.0 - raw), -1.0_f32)
        };
        let hop = ((phase_ms as f32 * (if busy { 0.022 } else { 0.009 }) + k as f32 * 1.7)
            .sin()
            .abs())
            * (if busy { 3.2 } else { 1.4 });
        let ky = ground_y - 4.2 - hop;
        let coat = crate::native_appearance::mix_srgb(kcol, [0xFF, 0xFF, 0xFF], 0.28);

        // Kitty body.
        prims.push(DrawPrim::Panel {
            x: kx - 4.5,
            y: ky - 2.5,
            w: 9.0,
            h: 5.2,
            radius: 2.5,
            fill: rgba(coat, 0xFA),
        });
        // Kitty head.
        let head_cx = kx + dir * 4.2;
        let head_cy = ky - 2.2;
        prims.push(DrawPrim::Dot {
            cx: head_cx,
            cy: head_cy,
            r: 3.4,
            color: rgba(coat, 0xFF),
        });
        // Two pointy kitty ears.
        for ear_dx in [-1.9_f32, 1.5_f32] {
            prims.push(DrawPrim::Dot {
                cx: head_cx + ear_dx,
                cy: head_cy - 3.1,
                r: 1.4,
                color: rgba(kcol, 0xFF),
            });
        }
        // Kitty eye & pink nose glint.
        prims.push(DrawPrim::Dot {
            cx: head_cx + dir * 1.4,
            cy: head_cy - 0.5,
            r: 0.85,
            color: rgba([0x1E, 0x1B, 0x24], 0xFF),
        });
        prims.push(DrawPrim::Dot {
            cx: head_cx + dir * 2.7,
            cy: head_cy + 0.4,
            r: 0.75,
            color: rgba(BLOB_CORAL, 0xFF),
        });
        // Wagging tail behind the kitty.
        let wag = (phase_ms as f32 * 0.016 + k as f32).sin() * 1.8;
        prims.push(DrawPrim::Dot {
            cx: kx - dir * 5.6,
            cy: ky - 1.8 + wag * 0.5,
            r: 1.3,
            color: rgba(kcol, 0xF0),
        });
        prims.push(DrawPrim::Dot {
            cx: kx - dir * 7.2,
            cy: ky - 3.4 + wag,
            r: 1.2,
            color: rgba(kcol, 0xE8),
        });
    }

    prims.push(DrawPrim::ClipPop);
}

/// A full-width blank row of `cols` cells in `fg`/`bg`; `seam` overlines row 0.
pub(crate) use aterm_render::band::blank_row;

/// Write `s` into `row` from column `col`, clamped to `cols`, one char per cell (labels
/// and values are ASCII plus a few BMP arrows, never wide glyphs).
pub(crate) fn write_str(
    row: &mut [RenderCell],
    cols: usize,
    mut col: usize,
    s: &str,
    fg: [u8; 3],
    bg: [u8; 3],
    bold: bool,
) {
    for ch in s.chars() {
        if col >= cols {
            break;
        }
        row[col] = chrome_band::cell(ch, fg, bg, bold, false);
        col += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ink that clears the floor on the bare surface but not the solve's
    /// quarter-step margin is still held: the wash pours less (down to none)
    /// rather than at full strength. Control: that ink fails the full pour.
    #[test]
    fn an_ink_between_the_floor_and_the_target_still_holds_every_tile() {
        use crate::native_appearance::contrast_ratio;
        let surface = [30, 30, 30];
        let ink = (0..=255u8)
            .rev()
            .map(|g| [g, g, g])
            .find(|&ink| {
                let ratio = contrast_ratio(ink, surface);
                (AURORA_INK_FLOOR..AURORA_INK_TARGET).contains(&ratio)
            })
            .expect("a grey between the floor and the target on this surface");
        let (cols, rows) = AURORA_GRID;
        let tiles = || (0..rows).flat_map(|j| (0..cols).map(move |i| (i, j)));
        assert!(
            tiles().any(|(i, j)| {
                contrast_ratio(ink, aurora_tile_unheld(surface, i, j)) < AURORA_INK_FLOOR
            }),
            "control: the full pour fails this ink somewhere"
        );
        for (i, j) in tiles() {
            let ratio = contrast_ratio(ink, aurora_tile(surface, ink, i, j));
            assert!(
                ratio >= AURORA_INK_FLOOR,
                "tile ({i}, {j}) holds the ink at {ratio:.2}:1"
            );
        }
    }

    #[test]
    fn enum_runtime_aliases_normalize_in_structured_display() {
        let field = |key: &'static str, seed: &str| EditField {
            label: key,
            key,
            kind: crate::prefs::edit_kind(key),
            seed: Some(seed.to_string()),
            placeholder: String::new(),
        };
        let aliases = [
            (crate::prefs::EDIT_CURSOR_STYLE, "beam", "bar"),
            (crate::prefs::EDIT_BIDI, "off", "disabled"),
            (crate::prefs::EDIT_BIDI, "on", "implicit"),
            (crate::prefs::EDIT_AMBIGUOUS_WIDTH, "single", "narrow"),
            (crate::prefs::EDIT_AMBIGUOUS_WIDTH, "double", "wide"),
            (crate::prefs::EDIT_PREDICTIVE_ECHO, "auto", "adaptive"),
            (crate::prefs::EDIT_PREDICTIVE_ECHO, "on", "adaptive"),
            (crate::prefs::EDIT_PREDICTIVE_ECHO, "true", "adaptive"),
            (crate::prefs::EDIT_PREDICTIVE_ECHO, "force", "always"),
            (
                crate::prefs::EDIT_TEXT_BLENDING,
                "linear_corrected",
                "linear-corrected",
            ),
            (crate::prefs::EDIT_MOTION, "reduce", "reduced"),
            (
                crate::prefs::EDIT_WINDOW_COLORSPACE,
                "displayp3",
                "display-p3",
            ),
            (crate::prefs::EDIT_WINDOW_COLORSPACE, "p3", "display-p3"),
            (
                crate::prefs::EDIT_BACKGROUND_MATERIAL,
                "underwindow",
                "under-window",
            ),
            (
                crate::prefs::EDIT_BACKGROUND_MATERIAL,
                "under_window",
                "under-window",
            ),
            (crate::prefs::EDIT_BACKGROUND_MATERIAL, "", "none"),
        ];
        for (key, alias, canonical) in aliases {
            let f = field(key, alias);
            assert_eq!(
                SettingsState::display_value(&f),
                canonical,
                "structured display for {key}={alias:?}"
            );
            assert!(
                matches!(f.kind, EditKind::Enum { .. }),
                "alias key {key} must remain an enum"
            );
        }

        for &(alias, canonical) in crate::prefs::CURSOR_TRAIL_STYLE_ALIASES {
            let f = field(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, alias);
            assert_eq!(SettingsState::display_value(&f), canonical, "{alias}");
        }

        // Every synth alias reads back the voice the synth plays.
        for &(alias, voice) in aterm_effects::trail_sound::SoundVoice::ALIASES {
            let canonical = voice.name();
            let f = field(crate::prefs::EDIT_TRAIL_SOUND_STYLE, alias);
            assert_eq!(SettingsState::display_value(&f), canonical, "{alias}");
        }
    }

    #[test]
    fn display_value_keeps_annotated_defaults_and_full_custom_values() {
        let trail = EditField {
            label: "Trail style",
            key: crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            kind: EditKind::Enum {
                options: crate::prefs::CURSOR_TRAIL_STYLES,
            },
            seed: None,
            placeholder: "rainbow kitty (default)".to_string(),
        };
        assert_eq!(
            SettingsState::display_value(&trail),
            "rainbow kitty (default)",
            "native display keeps the explanatory default annotation"
        );

        let custom = EditField {
            seed: Some("future multi word value".to_string()),
            ..trail
        };
        assert_eq!(
            SettingsState::display_value(&custom),
            "future multi word value"
        );
    }
}
