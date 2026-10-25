// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings control catalogue ([`SettingsState`]), overlay geometry and roles ([`SettingsGeom`],
//! [`Roles`]), the aurora, banner and System State village painters, and row writers
//! ([`blank_row`], [`write_str`]).

use aterm_core::terminal::RenderCell;

use crate::app_config::Config;
use crate::chrome_band;
use crate::prefs::{self, EditField, EditKind};
use crate::widget::{DrawPrim, TextWeight, rgba};

/// Shared semantic state embedded in each native Settings tab.
pub(crate) struct SettingsState {
    /// The editable controls for the live config, in row order.
    pub(crate) fields: Vec<EditField>,
}

impl SettingsState {
    /// Build from the current config generation.
    pub(crate) fn from_config(cfg: &Config) -> Self {
        Self {
            fields: prefs::editable_fields(cfg),
        }
    }

    /// The displayed value for a row.
    pub(crate) fn display_value(f: &EditField) -> &str {
        let raw = f.seed.as_deref().unwrap_or(f.placeholder.as_str());
        if let Some(seed) = f.seed.as_deref()
            && matches!(f.kind, EditKind::Enum { .. })
        {
            let token = seed.trim();
            enum_recognized(f, token)
                .or_else(|| enum_offered_or_runtime(f.key, token))
                .unwrap_or(raw)
        } else {
            raw
        }
    }
}

/// Resolve a documented config alias to its canonical option.
fn enum_alias(key: &str, token: &str) -> Option<&'static str> {
    let token = token.trim();
    if key == prefs::EDIT_CURSOR_TRAIL_STYLE {
        return prefs::cursor_trail_style_canonical(token);
    }
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

/// The canonical name of a typing-sound value the synth accepts.
fn enum_offered_or_runtime(key: &str, token: &str) -> Option<&'static str> {
    (key == prefs::EDIT_TRAIL_SOUND_STYLE)
        .then(|| prefs::trail_sound_style_canonical(token))
        .flatten()
}

/// The canonical option an Enum row's `token` resolves to.
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

/// Theme-derived colour roles shared by the overlay painters and native Settings chrome
/// ([`crate::native_appearance::NativeRoles`]).
pub(crate) type Roles = crate::native_appearance::NativeRoles;

pub(crate) const fn u32_rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

/// The width of `s` at `px` in the regular mono face ([`crate::tray_raster::measure_text`]);
/// UI-face text measures with [`crate::tray_raster::ui_text_width`].
pub(crate) fn text_w(s: &str, px: f32) -> f32 {
    crate::tray_raster::measure_text(s, px, TextWeight::Regular)
}

/// `f32::clamp`, except that inverted bounds give `hi` floored at zero instead of panicking.
#[inline]
pub(crate) fn fit(value: f32, lo: f32, hi: f32) -> f32 {
    if hi <= lo {
        hi.max(0.0)
    } else {
        value.clamp(lo, hi)
    }
}

/// Geometry of the in-grid overlay card slot ([`crate::overlay::OverlayModel::tray`]): device pixels on
/// the terminal route, points on the native route, whose transform scales them
/// ([`crate::OverlayCoordinateTransform`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SettingsGeom {
    pub cw: f32,
    pub ch: f32,
    pub font_px: f32,
    pub cols: usize,
    pub panel_rows: usize,
    /// This geometry's units per point: the window's backing scale where they are device pixels,
    /// 1 where they are points. A length meant in points (a card's width cap) is multiplied by it.
    pub px_per_pt: f32,
}

const BLOB_CORAL: [u8; 3] = [0xFF, 0x51, 0x48];
const BLOB_MARIGOLD: [u8; 3] = [0xFF, 0xC2, 0x2E];
const BLOB_LEAF: [u8; 3] = [0x2F, 0xAE, 0x5B];
const BLOB_COBALT: [u8; 3] = [0x2E, 0x5B, 0xFF];
const BLOB_VIOLET: [u8; 3] = [0xB0, 0x5C, 0xFF];

/// The rainbow arch's sixth stripe (the blob palette covers the other five).
const RAINBOW_TEAL: [u8; 3] = [0x21, 0xC2, 0xB7];

/// The aurora's tile grid.
const AURORA_GRID: (usize, usize) = (112, 64);
/// How far each aurora tile is poured from the surface toward its hue.
const AURORA_MIX: f32 = 0.30;
/// The contrast every aurora tile keeps against the page-chrome ink drawn on it.
pub(crate) const AURORA_INK_FLOOR: f32 = 4.5;
/// What a tile is solved to: a quarter step over [`AURORA_INK_FLOOR`].
const AURORA_INK_TARGET: f32 = AURORA_INK_FLOOR + 0.25;

/// Tile `(i, j)` of the aurora at its full pour, before the ink hold ([`aurora_tile`]).
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

/// Tile `(i, j)` of the aurora as painted ([`AURORA_INK_TARGET`]).
pub(crate) fn aurora_tile(surface: [u8; 3], ink: [u8; 3], i: usize, j: usize) -> [u8; 3] {
    use crate::native_appearance::contrast_ratio;
    let hue = aurora_hue(i, j);
    let full = crate::native_appearance::mix_srgb(surface, hue, AURORA_MIX);
    if contrast_ratio(ink, full) >= AURORA_INK_TARGET
        || contrast_ratio(ink, surface) < AURORA_INK_FLOOR
    {
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
    /// The last palette's tiles.
    static AURORA_TILES: std::cell::RefCell<Option<AuroraTiles>> =
        const { std::cell::RefCell::new(None) };
}

/// The Settings surface's aurora: the wallpaper treatment ([`AURORA_INK_FLOOR`], [`aurora_tile`]).
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

/// The native Settings hero banner.
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

/// Parse the extensible `phase=...;busy=...;done=...;total=...;states=...` village spec.
pub(crate) fn parse_rainbow_kitty_village_spec(spec: &str) -> (u64, bool, usize, usize, Vec<char>) {
    let mut phase_ms = 0u64;
    let mut busy = false;
    let mut done = None;
    let mut total = None;
    let mut states = Vec::new();
    for part in spec.split(';') {
        if let Some(val) = part.strip_prefix("phase=") {
            phase_ms = val.parse::<u64>().unwrap_or(0);
        } else if let Some(val) = part.strip_prefix("busy=") {
            busy = val == "1";
        } else if let Some(val) = part.strip_prefix("done=") {
            done = val.parse::<usize>().ok();
        } else if let Some(val) = part.strip_prefix("total=") {
            total = val.parse::<usize>().ok();
        } else if let Some(val) = part.strip_prefix("states=") {
            states.extend(val.chars());
        }
    }
    if states.is_empty() {
        states.resize(12, 'G');
    }
    let total = total
        .filter(|&t| t > 0)
        .unwrap_or_else(|| states.iter().filter(|&&c| c != '_').count().max(1));
    let done = done.unwrap_or_else(|| states.iter().filter(|&&c| c == 'G').count());
    (phase_ms, busy, done, total, states)
}

/// Paint the village ground, progress bar, paths, synergy connectors, gardens and buildings. Only
/// an assembling station (`'W'`) reads `phase_ms`; with none, the drawing changes only with
/// `states`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_rainbow_kitty_village_architecture(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    sky: [u8; 3],
    rim: [u8; 3],
    phase_ms: u64,
    busy: bool,
    done: usize,
    total: usize,
    states: &[char],
) {
    let dark_sky = crate::native_appearance::surface_is_dark(sky);
    let stripes = [
        BLOB_CORAL,
        BLOB_MARIGOLD,
        BLOB_LEAF,
        RAINBOW_TEAL,
        BLOB_COBALT,
        BLOB_VIOLET,
    ];
    let n = states.len().max(1);
    let ground_h = (h * 0.16).clamp(5.0, 9.0);
    let ground_y = y + h - ground_h;
    let track_w = (w - 8.0).max(1.0);

    // Base meadow & path bed.
    prims.push(DrawPrim::Panel {
        x: x + 4.0,
        y: ground_y,
        w: track_w,
        h: ground_h,
        radius: ground_h * 0.45,
        fill: rgba(rim, if dark_sky { 0xD8 } else { 0xB8 }),
    });

    // The progress bar inside the ground, advancing as stations go D -> W -> G.
    let assembling = states.iter().filter(|&&c| c == 'W').count();
    let progress =
        ((done as f32 + assembling as f32 * 0.45) / (total.max(1) as f32)).clamp(0.0, 1.0);
    if progress > 0.0 {
        let fill_w = (track_w * progress).max(ground_h);
        prims.push(DrawPrim::Panel {
            x: x + 4.0,
            y: ground_y + 1.0,
            w: fill_w.min(track_w),
            h: (ground_h - 2.0).max(2.0),
            radius: (ground_h - 2.0).max(2.0) * 0.45,
            fill: rgba(
                crate::native_appearance::mix_srgb(BLOB_LEAF, RAINBOW_TEAL, 0.45),
                if dark_sky { 0xB8 } else { 0x98 },
            ),
        });
    }

    // Cobblestones along the path, fixed in place.
    let stone_step = 16.0_f32;
    let mut sx = x + 10.0;
    let mut s_idx = 0usize;
    while sx < x + w - 10.0 {
        let c = stripes[s_idx % stripes.len()];
        prims.push(DrawPrim::Dot {
            cx: sx,
            cy: ground_y + ground_h * 0.5,
            r: (ground_h * 0.24).max(1.2),
            color: rgba(c, if dark_sky { 0xC8 } else { 0xA8 }),
        });
        sx += stone_step;
        s_idx += 1;
    }

    let pad_x = 14.0_f32;
    let usable_w = (w - pad_x * 2.0).max(48.0);
    let slot_w = usable_w / (n as f32);
    let house_w = (slot_w * 0.62).clamp(6.0, 22.0);
    let max_house_h = (h - ground_h - 12.0).clamp(10.0, 22.0);

    // 1. Paint inter-building paths, gardens, and neighbor/district synergies first.
    for i in 0..n.saturating_sub(1) {
        let s_left = states[i];
        let s_right = states[i + 1];
        let cx_left = x + pad_x + (i as f32 + 0.5) * slot_w;
        let cx_right = x + pad_x + ((i + 1) as f32 + 0.5) * slot_w;
        let mid_x = (cx_left + cx_right) * 0.5;
        let gap_left = cx_left + house_w * 0.48;
        let gap_right = cx_right - house_w * 0.48;
        let gap_w = (gap_right - gap_left).max(2.0);

        if s_left == 'G' && s_right == 'G' {
            // Connected golden cobblestone path linking two verified neighbor houses.
            prims.push(DrawPrim::Panel {
                x: cx_left,
                y: ground_y - 1.5,
                w: (cx_right - cx_left).max(2.0),
                h: 2.6,
                radius: 1.2,
                fill: rgba(BLOB_MARIGOLD, if dark_sky { 0xE4 } else { 0xCC }),
            });

            // Three verified houses in a row: a courtyard tree between the last two.
            if i >= 1 && states[i - 1] == 'G' {
                let trunk_h = (max_house_h * 0.48).clamp(5.0, 11.0);
                prims.push(DrawPrim::Panel {
                    x: mid_x - 1.0,
                    y: ground_y - trunk_h,
                    w: 2.0,
                    h: trunk_h,
                    radius: 0.8,
                    fill: rgba(BLOB_MARIGOLD, 0xD0),
                });
                let crown_y = ground_y - trunk_h - 1.5;
                prims.push(DrawPrim::Dot {
                    cx: mid_x,
                    cy: crown_y,
                    r: (house_w * 0.24).clamp(2.2, 4.2),
                    color: rgba(BLOB_LEAF, 0xEC),
                });
                prims.push(DrawPrim::Dot {
                    cx: mid_x - 1.4,
                    cy: crown_y - 0.8,
                    r: 1.2,
                    color: rgba(BLOB_CORAL, 0xF8),
                });
            }

            // Pairwise synergy connector between adjacent verified houses.
            match i % 3 {
                0 => {
                    // Synergy 0: Shared botanical garden hedge, blossoms, and vine pergola arch.
                    let garden_h = (max_house_h * 0.22).clamp(2.6, 4.8);
                    prims.push(DrawPrim::Panel {
                        x: gap_left,
                        y: ground_y - garden_h,
                        w: gap_w,
                        h: garden_h,
                        radius: 1.6,
                        fill: rgba(BLOB_LEAF, 0xEA),
                    });
                    for (f_idx, frac) in [0.25_f32, 0.5, 0.75].into_iter().enumerate() {
                        prims.push(DrawPrim::Dot {
                            cx: gap_left + gap_w * frac,
                            cy: ground_y - garden_h - 0.8,
                            r: 1.3,
                            color: rgba(stripes[(i + f_idx + 1) % stripes.len()], 0xFA),
                        });
                    }
                    prims.push(DrawPrim::Stroke {
                        x: gap_left,
                        y: ground_y - max_house_h * 0.62,
                        w: gap_w,
                        h: max_house_h * 0.42,
                        radius: (gap_w * 0.45).max(2.0),
                        width: 1.1,
                        color: rgba(BLOB_LEAF, 0xD8),
                    });
                }
                1 => {
                    // Synergy 1: Arched kitty skybridge connecting upper stories.
                    let bridge_y = ground_y - max_house_h * 0.52;
                    prims.push(DrawPrim::Panel {
                        x: gap_left - 1.0,
                        y: bridge_y,
                        w: gap_w + 2.0,
                        h: 2.2,
                        radius: 1.0,
                        fill: rgba(RAINBOW_TEAL, 0xE6),
                    });
                    prims.push(DrawPrim::Stroke {
                        x: gap_left,
                        y: bridge_y - 3.2,
                        w: gap_w,
                        h: 3.4,
                        radius: 2.0,
                        width: 1.0,
                        color: rgba(BLOB_MARIGOLD, 0xDC),
                    });
                    prims.push(DrawPrim::Dot {
                        cx: mid_x,
                        cy: bridge_y - 3.8,
                        r: 1.4,
                        color: rgba(BLOB_MARIGOLD, 0xFA),
                    });
                }
                _ => {
                    // Synergy 2: Overhead festival lantern garland & courtyard plaza fountain.
                    let cord_y = ground_y - max_house_h * 0.74;
                    for (l_idx, frac) in [0.25_f32, 0.5, 0.75].into_iter().enumerate() {
                        let sag = if l_idx == 1 { 1.6 } else { 0.6 };
                        prims.push(DrawPrim::Dot {
                            cx: gap_left + gap_w * frac,
                            cy: cord_y + sag,
                            r: 1.35,
                            color: rgba(stripes[(i + l_idx) % stripes.len()], 0xF6),
                        });
                    }
                    let basin_w = (gap_w * 0.68).clamp(3.0, 9.0);
                    prims.push(DrawPrim::Panel {
                        x: mid_x - basin_w * 0.5,
                        y: ground_y - 3.2,
                        w: basin_w,
                        h: 3.2,
                        radius: 1.4,
                        fill: rgba(RAINBOW_TEAL, 0xEA),
                    });
                    prims.push(DrawPrim::Dot {
                        cx: mid_x,
                        cy: ground_y - 4.8,
                        r: 1.5,
                        color: rgba(BLOB_COBALT, 0xF2),
                    });
                }
            }
        } else if (s_left == 'G' && s_right == 'W') || (s_left == 'W' && s_right == 'G') {
            // Emerging stepping-stone path between a completed house and an assembling neighbor.
            for frac in [0.3_f32, 0.65] {
                prims.push(DrawPrim::Dot {
                    cx: gap_left + gap_w * frac,
                    cy: ground_y - 0.8,
                    r: 1.1,
                    color: rgba(BLOB_MARIGOLD, 0xB8),
                });
            }
        }
    }

    // 2. Paint each station's building archetype and side garden.
    for (i, state) in states.iter().copied().enumerate() {
        let hue = stripes[i % stripes.len()];
        let cx = x + pad_x + (i as f32 + 0.5) * slot_w;
        // The archetype, and so the height, follows the station index alone.
        let archetype = i % 6;
        let arch_tallness = match archetype {
            0 => 0.96, // Observatory Dome
            1 => 0.90, // Artisan Forge Workshop
            2 => 0.86, // Cozy Bakery & Cafe
            3 => 1.00, // Clocktower Library
            4 => 0.94, // Zen Pagoda Dojo
            _ => 0.92, // Botanical Glass Greenhouse
        };
        let build_scale = match state {
            'G' => arch_tallness,
            'W' => 0.68 * arch_tallness,
            'D' => 0.42,
            'E' => 0.76,
            _ => 0.28,
        };
        let hh = (max_house_h * build_scale).max(5.0);
        let hx = cx - house_w * 0.5;
        let hy = ground_y - hh;
        let body_alpha = match state {
            'G' => {
                if dark_sky {
                    0xF0
                } else {
                    0xE2
                }
            }
            'W' => {
                if dark_sky {
                    0xCC
                } else {
                    0xB8
                }
            }
            'E' => 0xE6,
            'D' => 0x7E,
            _ => 0x44,
        };
        let body_color = if state == 'E' { BLOB_CORAL } else { hue };

        if matches!(state, 'G' | 'W' | 'E') {
            // Main building wall.
            prims.push(DrawPrim::Panel {
                x: hx,
                y: hy + hh * 0.28,
                w: house_w,
                h: hh * 0.72,
                radius: 3.0,
                fill: rgba(body_color, body_alpha),
            });
            // Roof eave / cornice.
            let roof_color =
                crate::native_appearance::mix_srgb(body_color, [0xFF, 0xFF, 0xFF], 0.24);
            prims.push(DrawPrim::Panel {
                x: hx - 1.5,
                y: hy + hh * 0.16,
                w: house_w + 3.0,
                h: (hh * 0.22).max(2.5),
                radius: 2.0,
                fill: rgba(roof_color, body_alpha),
            });
            // Signature kitty-ear roof finials.
            let ear_r = (house_w * 0.16).clamp(1.5, 3.1);
            for ex in [hx + ear_r * 1.1, hx + house_w - ear_r * 1.1] {
                prims.push(DrawPrim::Dot {
                    cx: ex,
                    cy: hy + hh * 0.14,
                    r: ear_r,
                    color: rgba(body_color, body_alpha),
                });
            }

            // Distinct architectural crowning details per archetype.
            match archetype {
                0 => {
                    // Observatory dome + star-gazing telescope.
                    prims.push(DrawPrim::Dot {
                        cx,
                        cy: hy + hh * 0.12,
                        r: (house_w * 0.26).clamp(2.0, 4.4),
                        color: rgba(RAINBOW_TEAL, body_alpha),
                    });
                    prims.push(DrawPrim::Panel {
                        x: cx + 1.0,
                        y: hy - 1.5,
                        w: (house_w * 0.28).clamp(2.2, 4.5),
                        h: 1.8,
                        radius: 0.8,
                        fill: rgba(BLOB_MARIGOLD, body_alpha),
                    });
                }
                1 => {
                    // Forge chimney stack + anvil hearth glow.
                    prims.push(DrawPrim::Panel {
                        x: hx + house_w * 0.68,
                        y: hy - 2.0,
                        w: (house_w * 0.20).clamp(2.0, 3.6),
                        h: hh * 0.28,
                        radius: 1.0,
                        fill: rgba(BLOB_CORAL, body_alpha),
                    });
                }
                2 => {
                    // Cozy Bakery striped cafe awning over the front window.
                    prims.push(DrawPrim::Panel {
                        x: hx - 1.0,
                        y: hy + hh * 0.44,
                        w: house_w + 2.0,
                        h: (hh * 0.16).clamp(2.0, 3.4),
                        radius: 1.4,
                        fill: rgba(BLOB_MARIGOLD, body_alpha),
                    });
                }
                3 => {
                    // Clocktower Library central spire & clock roundel.
                    prims.push(DrawPrim::Panel {
                        x: cx - 1.1,
                        y: hy - 2.8,
                        w: 2.2,
                        h: hh * 0.26,
                        radius: 1.0,
                        fill: rgba(roof_color, body_alpha),
                    });
                    prims.push(DrawPrim::Dot {
                        cx,
                        cy: hy + hh * 0.27,
                        r: (house_w * 0.15).clamp(1.4, 2.6),
                        color: rgba([0xFF, 0xFA, 0xE8], body_alpha),
                    });
                }
                4 => {
                    // Zen Pagoda upper tier eave.
                    prims.push(DrawPrim::Panel {
                        x: hx + house_w * 0.12,
                        y: hy + hh * 0.02,
                        w: house_w * 0.76,
                        h: (hh * 0.16).max(2.0),
                        radius: 1.6,
                        fill: rgba(roof_color, body_alpha),
                    });
                }
                _ => {
                    // Botanical Greenhouse crystal skylight frame.
                    prims.push(DrawPrim::Stroke {
                        x: hx + house_w * 0.14,
                        y: hy + hh * 0.04,
                        w: house_w * 0.72,
                        h: hh * 0.22,
                        radius: 2.0,
                        width: 1.0,
                        color: rgba(RAINBOW_TEAL, body_alpha),
                    });
                }
            }

            // Windows and cozy doorway.
            if hh >= 9.0 {
                let win_w = (house_w * 0.34).clamp(2.5, 6.0);
                let win_h = (hh * 0.24).clamp(2.2, 5.0);
                prims.push(DrawPrim::Panel {
                    x: cx - win_w * 0.5,
                    y: hy + hh * 0.46,
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
                let door_w = (house_w * 0.24).clamp(2.0, 4.2);
                let door_h = (hh * 0.24).clamp(2.4, 4.8);
                prims.push(DrawPrim::Panel {
                    x: cx - door_w * 0.5,
                    y: ground_y - door_h,
                    w: door_w,
                    h: door_h,
                    radius: 1.0,
                    fill: rgba(
                        crate::native_appearance::mix_srgb(body_color, [0x1A, 0x16, 0x22], 0.48),
                        body_alpha,
                    ),
                });
            }

            if state == 'G' {
                // A completed station's flower planter.
                let planter_w = (house_w * 0.28).clamp(2.4, 4.8);
                let planter_x = hx + house_w - planter_w * 0.35;
                prims.push(DrawPrim::Panel {
                    x: planter_x,
                    y: ground_y - 2.6,
                    w: planter_w,
                    h: 2.6,
                    radius: 1.0,
                    fill: rgba(BLOB_LEAF, 0xF0),
                });
                prims.push(DrawPrim::Dot {
                    cx: planter_x + planter_w * 0.5,
                    cy: ground_y - 3.4,
                    r: 1.25,
                    color: rgba(stripes[(i + 2) % stripes.len()], 0xFC),
                });
            } else if state == 'W' {
                // Assembling station: timber scaffolding + animated crane boom & rising puff.
                prims.push(DrawPrim::Stroke {
                    x: hx - 1.2,
                    y: hy + hh * 0.14,
                    w: house_w + 2.4,
                    h: hh * 0.86,
                    radius: 2.0,
                    width: 1.0,
                    color: rgba(BLOB_MARIGOLD, 0xD8),
                });
                let puff_phase = ((phase_ms as f64 * 0.004 + i as f64 * 0.31).rem_euclid(1.0)
                    as f32)
                    .clamp(0.0, 1.0);
                let puff_y = (hy - 2.0 - puff_phase * 6.0).max(y + 2.5);
                let puff_a = ((1.0 - puff_phase) * 210.0) as u8;
                prims.push(DrawPrim::Dot {
                    cx: cx + house_w * 0.22,
                    cy: puff_y,
                    r: 1.6 + puff_phase * 1.1,
                    color: rgba(BLOB_MARIGOLD, puff_a),
                });
            }
        } else if state == 'D' {
            // Pending / drift station: blueprint foundation outline & corner surveyor stakes.
            prims.push(DrawPrim::Stroke {
                x: hx,
                y: hy + hh * 0.25,
                w: house_w,
                h: hh * 0.75,
                radius: 2.5,
                width: 1.0,
                color: rgba(body_color, body_alpha),
            });
            for stake_x in [hx + 1.0, hx + house_w - 1.8] {
                prims.push(DrawPrim::Panel {
                    x: stake_x,
                    y: ground_y - 3.5,
                    w: 1.2,
                    h: 3.5,
                    radius: 0.5,
                    fill: rgba(BLOB_MARIGOLD, 0xA8),
                });
            }
        } else {
            // Skipped station ('_'): quiet low garden stone along the path.
            prims.push(DrawPrim::Stroke {
                x: hx,
                y: hy + hh * 0.45,
                w: house_w,
                h: hh * 0.55,
                radius: 2.0,
                width: 1.0,
                color: rgba(body_color, body_alpha),
            });
        }
        let _ = busy;
    }
}

/// Paint the wandering village kitties that stroll along paths and visit gardens.
pub(crate) fn paint_rainbow_kitty_village_kitties(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    w: f32,
    ground_y: f32,
    phase_ms: u64,
    busy: bool,
) {
    let kitty_colors: [[u8; 3]; 4] = [BLOB_CORAL, BLOB_MARIGOLD, RAINBOW_TEAL, BLOB_VIOLET];
    let kitty_speed = if busy { 0.052_f64 } else { 0.018_f64 };
    let path_w = (w - 36.0).max(24.0);
    let path_w64 = f64::from(path_w);
    let phase_f64 = phase_ms as f64;
    for (k, &kcol) in kitty_colors.iter().enumerate() {
        let kf = k as f64;
        // Gentle garden-sniffing pause modulation as kitties stroll between village houses.
        let stroll = phase_f64 * kitty_speed
            + (phase_f64 * 0.003 + kf * 1.9).sin() * 4.5
            + kf * (path_w64 * 0.27);
        let raw = stroll.rem_euclid(path_w64 * 2.0) as f32;
        let (kx, dir) = if raw <= path_w {
            (x + 18.0 + raw, 1.0_f32)
        } else {
            (x + 18.0 + (path_w * 2.0 - raw), -1.0_f32)
        };
        let hop = (((phase_f64 * (if busy { 0.022 } else { 0.009 }) + kf * 1.7)
            .sin()
            .abs())
            * (if busy { 3.0 } else { 1.3 })) as f32;
        let ky = ground_y - 4.2 - hop;
        let coat = crate::native_appearance::mix_srgb(kcol, [0xFF, 0xFF, 0xFF], 0.28);

        prims.push(DrawPrim::Panel {
            x: kx - 4.5,
            y: ky - 2.5,
            w: 9.0,
            h: 5.2,
            radius: 2.5,
            fill: rgba(coat, 0xFA),
        });
        let head_cx = kx + dir * 4.2;
        let head_cy = ky - 2.2;
        prims.push(DrawPrim::Dot {
            cx: head_cx,
            cy: head_cy,
            r: 3.4,
            color: rgba(coat, 0xFF),
        });
        for ear_dx in [-1.9_f32, 1.5_f32] {
            prims.push(DrawPrim::Dot {
                cx: head_cx + ear_dx,
                cy: head_cy - 3.1,
                r: 1.4,
                color: rgba(kcol, 0xFF),
            });
        }
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
        let wag = ((phase_f64 * 0.016 + kf).sin() * 1.8) as f32;
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
}

/// The System State animated hero.
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
    let (phase_ms, busy, done, total, states) = parse_rainbow_kitty_village_spec(spec);
    let ground_h = (h * 0.16).clamp(5.0, 9.0);
    let ground_y = y + h - ground_h;

    prims.push(DrawPrim::ClipPush { x, y, w, h });
    paint_rainbow_kitty_village_architecture(
        prims, x, y, w, h, sky, rim, phase_ms, busy, done, total, &states,
    );
    paint_rainbow_kitty_village_kitties(prims, x, w, ground_y, phase_ms, busy);
    prims.push(DrawPrim::ClipPop);
}

/// A full-width blank row of `cols` cells in `fg`/`bg`.
pub(crate) use aterm_render::band::blank_row;

/// Write `s` into `row` from column `col`.
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

    #[test]
    fn rainbow_kitty_village_preserves_smooth_motion_at_large_phase_ms() {
        let mut prims_a = Vec::new();
        let mut prims_b = Vec::new();
        let mut prims_max = Vec::new();
        paint_rainbow_kitty_village_kitties(&mut prims_a, 10.0, 320.0, 50.0, 100_000_000, false);
        paint_rainbow_kitty_village_kitties(&mut prims_b, 10.0, 320.0, 50.0, 100_000_120, false);
        paint_rainbow_kitty_village_kitties(&mut prims_max, 10.0, 320.0, 50.0, u64::MAX, true);
        assert_ne!(
            format!("{prims_a:?}"),
            format!("{prims_b:?}"),
            "kitties continue moving smoothly past 2^24 ms"
        );
        for p in &prims_max {
            if let DrawPrim::Dot { cx, cy, r, .. } = p {
                assert!(cx.is_finite() && cy.is_finite() && r.is_finite());
            }
        }
    }

    fn assert_village_kitties_cross_a_week_smoothly(busy: bool) {
        let bodies = |phase_ms| {
            let mut prims = Vec::new();
            paint_rainbow_kitty_village_kitties(&mut prims, 10.0, 320.0, 50.0, phase_ms, busy);
            prims
                .into_iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Panel { x, y, .. } => Some((x, y)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let before = bodies(604_799_999);
        let after = bodies(604_800_000);
        assert_eq!(before.len(), 4, "each kitty has a body");
        assert_eq!(after.len(), before.len());
        for (kitty, ((x0, y0), (x1, y1))) in before.into_iter().zip(after).enumerate() {
            // At either speed, one millisecond moves the stroll and the hop under 0.066 px.
            assert!(
                (x1 - x0).abs() < 0.1 && (y1 - y0).abs() < 0.1,
                "kitty {kitty}: ({x0}, {y0}) jumped to ({x1}, {y1})"
            );
        }
    }

    #[test]
    fn rainbow_kitty_village_idle_motion_crosses_a_week_smoothly() {
        assert_village_kitties_cross_a_week_smoothly(false);
    }

    #[test]
    fn rainbow_kitty_village_busy_motion_crosses_a_week_smoothly() {
        assert_village_kitties_cross_a_week_smoothly(true);
    }

    #[test]
    fn rainbow_kitty_village_architecture_is_invariant_across_phase_ms_when_stations_are_stable() {
        let states = ['G', 'G', 'G', 'D', 'E', '_', 'G', 'G'];
        let arch = |phase_ms| {
            let mut prims = Vec::new();
            paint_rainbow_kitty_village_architecture(
                &mut prims,
                10.0,
                10.0,
                320.0,
                50.0,
                [24, 24, 32],
                [60, 60, 72],
                phase_ms,
                false,
                5,
                8,
                &states,
            );
            format!("{prims:?}")
        };
        let baseline = arch(0);
        for phase in [1, 500, 86_399_999, 86_400_000, 604_800_000, u64::MAX] {
            assert_eq!(arch(phase), baseline, "phase_ms={phase}");
        }
    }
}
