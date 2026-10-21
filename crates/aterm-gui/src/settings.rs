// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings control model and the chrome primitives the in-grid overlays share.
//! Native Settings views reuse [`SettingsState`] preference metadata/projections, while
//! production pixels, input, accessibility, and machine inspection compile from
//! [`crate::native_ui`]. The retired card is gone; what remains here is the control
//! catalogue, the overlay geometry and colour roles ([`SettingsGeom`], [`Roles`]), the
//! row writers ([`blank_row`], [`write_str`]), and the aurora / rainbow-banner painters
//! native Settings draws.

use aterm_core::terminal::RenderCell;

use crate::app_config::Config;
use crate::chrome_band;
use crate::prefs::{self, EditField, EditKind};
use crate::widget::{DrawPrim, TextWeight, rgba};

/// Shared semantic state embedded in each native Settings tab: the editable control
/// catalogue plus the search fields the native view reads through `legacy`.
pub(crate) struct SettingsState {
    /// Snapshot of the editable controls for the live config, in row order. Native
    /// Settings rebuilds it from its config snapshot after every persisted change, so
    /// the displayed value tracks the file. OWNED — [`EditField`] is moved in.
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

    /// The CURRENT displayed value for a row: the configured seed, else the effective
    /// placeholder (so an unset key shows what is actually in effect, never blank).
    /// Authored Enum aliases are projected onto their canonical option so the native
    /// picker does not mislabel a runtime-valid alias as a custom value. Unknown and
    /// dynamic values remain verbatim, and unset placeholders retain their explanatory
    /// `"(default)"` / `"(follow OS)"` annotation.
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

/// Resolve a documented config ALIAS to its canonical option spelling. The config loaders
/// (`app_config`) accept aliases that are NOT in the picker's canonical option set (e.g.
/// cursor_style `beam` == `bar`); without this native Settings would misclassify the
/// authored value as a custom option. Returns `None` when `token` is not a known alias.
fn enum_alias(key: &str, token: &str) -> Option<&'static str> {
    // Trail-style aliases (nyan rainbow → rainbow kitty, ember → fire, …) resolve through the
    // shared table in `prefs` — the same source `--validate-config` and the
    // load-time unknown-style warning consult, so the native row, preview lane,
    // and the live effect can never disagree about an aliased spelling.
    let token = token.trim();
    if key == prefs::EDIT_CURSOR_TRAIL_STYLE {
        return prefs::cursor_trail_style_canonical(token);
    }
    // Typing-sound aliases (water → droplet, mech → mechanical, bell → glass
    // bell, …) resolve through the synth's own parser, so an authored alias
    // projects onto its picker row instead of showing as a custom entry.
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

/// The voice/style a row's configured value RESOLVES to at RUNTIME even when the
/// picker does not offer it — the third state the Enum layer was missing.
///
/// `enum_recognized` answers one question: is this value one of the options?
/// For every row but one that is the same as "does the engine accept it", and
/// the two were treated as interchangeable. The typing-sound row pulled them
/// apart: `SoundVoice::RainbowKittyV2` ("music box") is live and selectable by
/// config while `SoundVoice::ALL` — the picker AND v1's voice-agnostic sweep
/// list — deliberately withholds it until v2's migration completes. Read
/// through the options alone, that config reported as `auto`: Settings
/// contradicting the synth actually playing.
///
/// So resolve it the way the runtime does (`Config::trail_sound_voice` =
/// `SoundVoice::parse(..).unwrap_or_default()`) and let a recognized voice
/// answer with its own canonical name, whichever spelling was authored. `None`
/// for a genuinely unknown value, whose truthful answer is the row's default —
/// which for this row IS `auto`, `SoundVoice::default()`.
fn enum_offered_or_runtime(key: &str, token: &str) -> Option<&'static str> {
    (key == prefs::EDIT_TRAIL_SOUND_STYLE)
        .then(|| prefs::trail_sound_style_canonical(token))
        .flatten()
}

/// The canonical option an Enum row's authored `token` RESOLVES to (directly or via the
/// alias map), or `None` for a genuinely unrecognized spelling.
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

/// The width of `s` at size `px` in the MONO (terminal) face — REAL advances from
/// the chrome font stack (the user's terminal face, DejaVu strictly as coverage
/// fallback), replacing the old hardcoded `0.6 em × chars` estimate that drifted
/// from every non-DejaVu face. Regular weight; measure bold runs via
/// [`crate::tray_raster::measure_text`] directly. UI-face chrome measures with
/// [`ui_text_width`] instead.
pub(crate) fn text_w(s: &str, px: f32) -> f32 {
    crate::tray_raster::measure_text(s, px, TextWeight::Regular)
}

/// `f32::clamp(lo, hi)` panics when `lo > hi`. Overlay geometry can invert bounds in
/// extreme windows — a tiny font drops the max widget below its floor; an ultra-narrow
/// grid drops the available width below the desired minimum. Pin to the achievable
/// (upper) bound in that case instead of panicking.
#[inline]
pub(crate) fn fit(value: f32, lo: f32, hi: f32) -> f32 {
    // Inverted bounds (degenerate layout): pin to the achievable upper bound,
    // floored at zero so a too-narrow window yields a zero-width widget rather
    // than a NEGATIVE length (which would wrap on a later `as u32` downstream).
    if hi <= lo {
        hi.max(0.0)
    } else {
        value.clamp(lo, hi)
    }
}

/// Device-pixel geometry of the in-grid overlay card slot, handed to every
/// [`crate::overlay::OverlayModel::tray`]. Native Settings does not use this
/// terminal-grid geometry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SettingsGeom {
    pub cw: f32,
    pub ch: f32,
    pub font_px: f32,
    pub cols: usize,
    pub panel_rows: usize,
}

// ---- Landing page (design §L) -------------------------------------------------------

const BLOB_CORAL: [u8; 3] = [0xFF, 0x51, 0x48];
const BLOB_MARIGOLD: [u8; 3] = [0xFF, 0xC2, 0x2E];
const BLOB_LEAF: [u8; 3] = [0x2F, 0xAE, 0x5B];
const BLOB_COBALT: [u8; 3] = [0x2E, 0x5B, 0xFF];
const BLOB_VIOLET: [u8; 3] = [0xB0, 0x5C, 0xFF];

// ---- §L.5 The rainbow welcome ---------------------------------------------------
// The landing's welcome flourish: a pastel rainbow arch sweeping in behind the
// hero and a small twinkling constellation. Purely decorative.
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

/// The §L.5 composition as a NATIVE Settings hero banner: a pastel rainbow arch
/// cresting through a small constellation of star glints. Deliberately STATIC:
/// the native scheduler keeps idle routes at 0% and this banner never asks for a
/// frame. Painted through the audited custom-node lowering
/// (`native_ui::RAINBOW_BANNER_AUDIT`); everything clips to `rect`.
///
/// `sky` and `rim` arrive RESOLVED from the caller's role palette, exactly as
/// [`paint_settings_aurora`] takes `surface` — this banner is a Settings CARD,
/// and a card that does not follow the resolved chrome palette is a hole in the
/// page. It used to fill with the landing page's authored `#EDF6EC` mint and a
/// `#C9DDC8` hairline, byte-identical in all four appearance states, which put a
/// near-white slab in the middle of the forced-DARK Settings page (the 2026-08
/// cold visual audit's headline defect, and the last place config `window_theme`
/// was still a no-op).
///
/// The ARCH and the GLINTS keep their authored spectrum — they are the identity,
/// and a rainbow that re-tints per theme stops being a rainbow — but their alpha
/// is conditioned on the sky they land on: saturated trim at ~27 % over a pale
/// sky reads as a pastel wash, while the same alpha over a dark sky all but
/// vanishes, so the dark side gets the deeper pour that lands it in the same
/// place perceptually.
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

/// A full-width blank row of `cols` cells in `fg`/`bg` (the `seam` overline marks the
/// panel's top edge on row 0) — the renderer's (`aterm_render::band`, ruling 331).
pub(crate) use aterm_render::band::blank_row;

/// Write `s` into `row` starting at column `col`, clamped to the row width. Each glyph
/// becomes a `chrome_band::cell` in `fg`/`bg`. Multi-cell-wide glyphs are not expected here
/// (labels/values are ASCII + a few BMP arrows), so one char == one cell.
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

        // The typing-sound row: every synth alias (water → droplet, mech →
        // mechanical, bell / glass bell → music box, …) reads back the voice the
        // synth actually plays — including an alias of a voice the roster
        // (`SoundVoice::ALL`) deliberately withholds, which must never collapse to
        // the roster's first entry (`auto`).
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
