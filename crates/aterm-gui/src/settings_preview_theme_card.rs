// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Theme Showcase's cards (Settings ▸ Themes): one card per theme the
//! catalog can draw, each a live miniature specimen painted in that theme's
//! own colours.
//!
//! The card list is built once per catalog generation ([`showcase_cards`])
//! from the schemes the config worker already parsed, so hovering, focusing
//! or scrolling the gallery resolves no theme and reads no file. A card's
//! colours are the theme as authored: the person's `aterm.toml` colour
//! overrides would make every card the same, so they appear only on the
//! page's stage and in the live trial.
//!
//! The specimen itself ([`paint_card_tile`]) is a handful of panels and
//! monospace text runs: a prompt on a git branch, `ls` colours, a short diff
//! with a selected word, a syntax-coloured line, the cursor, and all sixteen
//! ANSI swatches. It never builds a terminal or forks a font, so a page of
//! cards costs what a page of buttons costs. The faithful view, in the
//! terminal's own font, is the stage's specimen.

use aterm_types::{Appearance, ColorScheme, Rgb};

use crate::native_ui::LogicalRect;
use crate::settings::Roles;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

/// Which part of the gallery a card belongs to, in the order the gallery
/// lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ShowcaseTier {
    /// aterm's own signature themes (Aterm Night, Aterm Day, Hearth, Linen),
    /// in the registry's order.
    Signature,
    /// Every other built-in, Default included, in the registry's order.
    Builtin,
    /// The person's own theme files that loaded, by name.
    Yours,
    /// The community pack's themes not yet in the themes folder (Get More):
    /// a press adds the file, then tries it.
    Pack,
}

impl ShowcaseTier {
    /// The tier's heading in the gallery.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Signature => "SIGNATURE",
            Self::Builtin => "BUILT IN",
            Self::Yours => "YOURS",
            Self::Pack => "COMMUNITY PACK",
        }
    }
}

/// One theme in the gallery.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShowcaseCard {
    /// The catalog id: what `theme = "…"` names.
    pub(crate) id: String,
    /// A key- and wire-safe spelling of the id, unique in the gallery
    /// (`ctl act` takes a key as one word, and ids hold spaces).
    pub(crate) slug: String,
    pub(crate) tier: ShowcaseTier,
    /// What the person sees: a built-in's name, a file's own `name =`.
    pub(crate) label: String,
    /// The theme's description, its first sentence; may be empty.
    pub(crate) description: String,
    /// The appearance the theme is drawn for (declared or measured).
    pub(crate) appearance: Appearance,
    /// The theme's colours, packed `0x00RRGGBB`.
    pub(crate) fg: u32,
    pub(crate) bg: u32,
    pub(crate) cursor: u32,
    pub(crate) selection: u32,
    /// The selected text's colour; `None` keeps each cell's own.
    pub(crate) selection_fg: Option<u32>,
    pub(crate) ansi: [u32; 16],
    /// Text contrast, foreground on background.
    pub(crate) contrast: f64,
    /// The theme's type settings in one line ("Berkeley Mono · 1.15×"),
    /// `None` for a theme that names none.
    pub(crate) typography: Option<String>,
    /// The same in words for a screen reader ("sets font Berkeley Mono,
    /// line height 1.15").
    pub(crate) typography_spoken: Option<String>,
    /// How many type settings the theme names.
    pub(crate) type_keys: usize,
    /// The theme file was written for a newer theme schema and some of its
    /// settings were skipped.
    pub(crate) partial: bool,
    /// The label, id, description and type summary lowercased once, for the
    /// filter: a keystroke in the filter matches every card without
    /// allocating. A theme that names a font matches "font" too.
    search: [String; 4],
}

const fn pack(color: Rgb) -> u32 {
    ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32
}

const fn unpack(color: u32) -> [u8; 3] {
    [(color >> 16) as u8, (color >> 8) as u8, color as u8]
}

/// `text` as a key fragment: lowercase ASCII letters and digits, every other
/// run one `-`.
fn slug_of(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "theme".to_string()
    } else {
        slug.to_string()
    }
}

/// The first sentence of `text` (through its first `. `), trimmed.
fn first_sentence(text: &str) -> String {
    let text = text.trim();
    text.find(". ")
        .map_or(text, |end| &text[..=end])
        .trim()
        .to_string()
}

impl ShowcaseCard {
    fn new(
        id: &str,
        tier: ShowcaseTier,
        label: &str,
        description: Option<&str>,
        scheme: &ColorScheme,
    ) -> Self {
        let parts = scheme.to_theme_parts();
        let description = description.map(first_sentence).unwrap_or_default();
        Self {
            search: [
                label.to_lowercase(),
                id.to_lowercase(),
                description.to_lowercase(),
                String::new(),
            ],
            typography: None,
            typography_spoken: None,
            type_keys: 0,
            partial: false,
            id: id.to_string(),
            slug: String::new(),
            tier,
            label: label.to_string(),
            description,
            appearance: scheme.appearance,
            fg: parts.fg & 0x00ff_ffff,
            bg: parts.bg & 0x00ff_ffff,
            cursor: parts.cursor & 0x00ff_ffff,
            selection: parts.selection & 0x00ff_ffff,
            selection_fg: scheme.selection_fg.map(pack),
            ansi: scheme.ansi.map(pack),
            contrast: aterm_messages::ink::contrast(
                unpack(parts.fg & 0x00ff_ffff),
                unpack(parts.bg & 0x00ff_ffff),
            ),
        }
    }

    /// This card with the theme's type settings (a built-in names none).
    fn with_typography(
        mut self,
        typography: &aterm_types::scheme::ThemeTypography,
        partial: bool,
    ) -> Self {
        self.typography = typography.summary();
        self.typography_spoken = typography.spoken();
        self.type_keys = typography.keys().len();
        self.partial = partial;
        self.search[3] = self
            .typography
            .as_ref()
            .map(|summary| format!("{} font", summary.to_lowercase()))
            .unwrap_or_default();
        self
    }

    /// `dark` or `light`.
    pub(crate) const fn appearance_word(&self) -> &'static str {
        match self.appearance {
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        }
    }

    /// What assistive technology and `controls` read for the card:
    /// `<name>, dark|light, text contrast X:1`, then the type settings it
    /// names in words (`, sets font Berkeley Mono, line height 1.15`).
    pub(crate) fn accessible_label(&self) -> String {
        let mut label = format!(
            "{}, {}, text contrast {}:1",
            self.label,
            self.appearance_word(),
            contrast_figure(self.contrast)
        );
        if let Some(spoken) = &self.typography_spoken {
            label.push_str(", ");
            label.push_str(spoken);
        }
        label
    }

    /// Whether the gallery's filter `query` (lowercased, trimmed) admits the
    /// card: its name, id, description or appearance, by the ladder Settings
    /// search and the theme picker use. An empty query admits every card.
    pub(crate) fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let matches = |candidate: &str| {
            crate::native_config_language::candidate_match_score(candidate, query).is_some()
        };
        self.search.iter().any(|candidate| matches(candidate)) || matches(self.appearance_word())
    }

    /// The paint-only tile's value: every colour the specimen draws, then
    /// whether the compact specimen (fewer lines) is asked for. One string
    /// because a custom paint node carries one; [`paint_card_tile`] reads it
    /// back.
    pub(crate) fn tile_value(&self, compact: bool) -> String {
        let mut value = String::with_capacity(24 * 7 + 8);
        for color in [self.fg, self.bg, self.cursor, self.selection]
            .into_iter()
            .chain(std::iter::once(self.selection_fg.unwrap_or(u32::MAX)))
            .chain(self.ansi)
        {
            if color == u32::MAX {
                value.push_str("- ");
            } else {
                value.push_str(&format!("{color:06x} "));
            }
        }
        // The figure the label and badge print (rounded down), not a
        // rounding of its own.
        value.push_str(&format!("{} ", contrast_figure(self.contrast)));
        value.push_str(if compact { "compact" } else { "full" });
        if self.typography.is_some() {
            value.push_str(" type");
        }
        value
    }
}

/// The WCAG level a text contrast meets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContrastGrade {
    /// 7:1 or more.
    Aaa,
    /// 4.5:1 or more.
    Aa,
    /// Below 4.5:1.
    Low,
}

impl ContrastGrade {
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Aaa => "AAA",
            Self::Aa => "AA",
            Self::Low => "Low",
        }
    }
}

/// The grade `ratio` earns.
pub(crate) fn contrast_grade(ratio: f64) -> ContrastGrade {
    if ratio >= 7.0 {
        ContrastGrade::Aaa
    } else if ratio >= 4.5 {
        ContrastGrade::Aa
    } else {
        ContrastGrade::Low
    }
}

/// A contrast ratio to one decimal, rounded down so a floor is never
/// overstated (2.96 reads 2.9), and without a trailing `.0`.
pub(crate) fn contrast_figure(ratio: f64) -> String {
    let tenths = (ratio * 10.0).floor() / 10.0;
    if tenths.fract() == 0.0 {
        format!("{tenths:.0}")
    } else {
        format!("{tenths:.1}")
    }
}

/// Every theme the catalog can draw, as gallery cards, in gallery order:
/// the signature themes (registry order), then every other built-in, Default
/// included (registry order), then the person's own theme files that loaded,
/// by name, then the community pack's themes the folder has no file for
/// (by id). Built from the parsed catalog and the embedded pack: no file is
/// read.
pub(crate) fn showcase_cards(themes: &crate::app_config::ThemeCatalog) -> Vec<ShowcaseCard> {
    let builtins = aterm_types::scheme::builtin_theme_list();
    let mut cards = Vec::with_capacity(builtins.len() + 8);
    for tier in [ShowcaseTier::Signature, ShowcaseTier::Builtin] {
        for builtin in builtins
            .iter()
            .filter(|builtin| builtin.signature == (tier == ShowcaseTier::Signature))
        {
            let Some(scheme) = aterm_types::scheme::builtin(builtin.name) else {
                continue;
            };
            cards.push(ShowcaseCard::new(
                builtin.name,
                tier,
                builtin.name,
                Some(builtin.description),
                &scheme,
            ));
        }
    }
    let mut yours: Vec<ShowcaseCard> = themes
        .ready_themes()
        .filter(|(id, _)| aterm_types::scheme::builtin_theme(id).is_none())
        .filter_map(|(id, meta)| {
            let package = themes.resolve_package(id).ok()?;
            Some(
                ShowcaseCard::new(
                    id,
                    ShowcaseTier::Yours,
                    &meta.display_name,
                    meta.description.as_deref(),
                    &package.colors,
                )
                .with_typography(&package.typography, !meta.skipped_keys.is_empty()),
            )
        })
        .collect();
    yours.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    cards.extend(yours);
    // Then every pack theme the folder has no file for at all: a file that
    // did not load keeps its Needs-attention row instead of a card here.
    for theme in aterm_theme_pack::themes() {
        if themes
            .listing()
            .any(|(id, _)| id.eq_ignore_ascii_case(theme.id))
        {
            continue;
        }
        let Ok(parsed) = aterm_types::scheme::parse_theme_str(theme.text) else {
            continue;
        };
        cards.push(
            ShowcaseCard::new(
                theme.id,
                ShowcaseTier::Pack,
                &parsed.scheme.name,
                parsed.description.as_deref(),
                &parsed.scheme,
            )
            .with_typography(&parsed.typography, parsed.skipped_count > 0),
        );
    }
    // Slugs last, over the final order, so a collision's suffix is stable.
    let mut seen = std::collections::BTreeSet::new();
    for card in &mut cards {
        let base = slug_of(&card.id);
        let mut slug = base.clone();
        let mut n = 2;
        while !seen.insert(slug.clone()) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        card.slug = slug;
    }
    cards
}

/// One run of specimen text: its characters and the slot it is inked in.
#[derive(Clone, Copy)]
enum Ink {
    Fg,
    Ansi(usize),
    /// The selection band, its text in the selection foreground (else fg).
    Selected,
}

/// The specimen's lines, as runs. `compact` drops the `ls` and code lines.
fn specimen_lines(compact: bool) -> Vec<Vec<(&'static str, Ink, bool)>> {
    let mut lines = vec![
        vec![
            ("~/aterm", Ink::Ansi(4), true),
            (" on ", Ink::Ansi(8), false),
            ("main", Ink::Ansi(5), true),
            (" *2", Ink::Ansi(3), false),
        ],
        vec![("$ ", Ink::Ansi(2), true), ("ls", Ink::Fg, false)],
    ];
    if !compact {
        lines.push(vec![
            ("src/", Ink::Ansi(4), true),
            (" run.sh", Ink::Ansi(2), true),
            (" logo.png", Ink::Ansi(5), false),
            (" notes.md", Ink::Fg, false),
        ]);
    }
    lines.push(vec![("@@ -12 +12 @@", Ink::Ansi(6), false)]);
    lines.push(vec![("- let n = 1;", Ink::Ansi(1), false)]);
    lines.push(vec![
        ("+ let ", Ink::Ansi(2), false),
        ("n", Ink::Selected, false),
        (" = 2;", Ink::Ansi(2), false),
    ]);
    if !compact {
        lines.push(vec![
            ("fn ", Ink::Ansi(5), false),
            ("main", Ink::Ansi(4), false),
            ("() { ", Ink::Fg, false),
            ("\"hi\"", Ink::Ansi(2), false),
            (" } ", Ink::Fg, false),
            ("// ok", Ink::Ansi(8), false),
        ]);
    }
    lines
}

/// How many specimen lines a tile shows (the prompt with the cursor
/// follows them).
pub(crate) const fn specimen_line_count(compact: bool) -> usize {
    if compact { 5 } else { 7 }
}

/// The swatch band under the specimen: two rows of eight.
const SWATCH_SIZE: f32 = 8.0;
const SWATCH_GAP: f32 = 2.0;
const TILE_INSET: f32 = 10.0;

/// The tile height a specimen of `lines` lines needs at `scale`: its
/// padding, its lines (with the prompt line's cursor), and the swatch band.
pub(crate) fn tile_height(compact: bool, scale: f32) -> f32 {
    let pitch = (specimen_px(scale) * 1.32).round();
    let lines = specimen_line_count(compact) as f32 + 1.0;
    (2.0 * TILE_INSET + lines * pitch + 6.0 + 2.0 * SWATCH_SIZE + SWATCH_GAP).ceil()
}

/// The specimen's monospace size at `scale`, before a tile narrows it.
fn specimen_px(scale: f32) -> f32 {
    (10.0 * scale.clamp(0.85, 2.0)).round()
}

/// Read a tile value back ([`ShowcaseCard::tile_value`]): fg, bg, cursor,
/// selection, selection fg (absent as `None`), the sixteen ANSI colours, the
/// contrast, and whether the specimen is compact.
struct TileColors {
    fg: u32,
    bg: u32,
    cursor: u32,
    selection: u32,
    selection_fg: Option<u32>,
    ansi: [u32; 16],
    contrast: f64,
    compact: bool,
    /// The theme names type settings: the tile shows an "Aa" chip.
    typed: bool,
}

fn parse_tile(value: &str) -> Option<TileColors> {
    let mut words = value.split_whitespace();
    let mut color = || u32::from_str_radix(words.next()?, 16).ok();
    let fg = color()?;
    let bg = color()?;
    let cursor = color()?;
    let selection = color()?;
    // The fifth word may be `-`; the hex reader above cannot read it.
    let mut words = value.split_whitespace().skip(4);
    let selection_fg = match words.next()? {
        "-" => None,
        word => Some(u32::from_str_radix(word, 16).ok()?),
    };
    let mut ansi = [0_u32; 16];
    for slot in &mut ansi {
        *slot = u32::from_str_radix(words.next()?, 16).ok()?;
    }
    let contrast = words.next()?.parse().ok()?;
    let compact = words.next()? == "compact";
    let typed = words.next() == Some("type");
    Some(TileColors {
        fg,
        bg,
        cursor,
        selection,
        selection_fg,
        ansi,
        contrast,
        compact,
        typed,
    })
}

/// Paint one card's specimen tile into `rect`: the theme's background, the
/// specimen lines and cursor in its colours, the contrast badge, and the
/// sixteen ANSI swatches. Panels and text only, from the tile's value; a
/// malformed value paints nothing.
pub(crate) fn paint_card_tile(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    let Some(tile) = value.and_then(parse_tile) else {
        return;
    };
    if rect.width < 24.0 || rect.height < 24.0 {
        return;
    }
    let ink = |color: u32| rgba(unpack(color), 255);
    let bg = unpack(tile.bg);
    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: rect.y,
        w: rect.width,
        h: rect.height,
        radius: 8.0,
        fill: rgba(bg, 255),
    });
    // A hairline in the host's separator: a tile whose background matches the
    // window's still reads as a card.
    prims.push(DrawPrim::Stroke {
        x: rect.x + 0.5,
        y: rect.y + 0.5,
        w: (rect.width - 1.0).max(0.0),
        h: (rect.height - 1.0).max(0.0),
        radius: 7.5,
        width: 1.0,
        color: rgba(roles.separator, 150),
    });
    let scale = crate::native_appearance::text_scale();
    let inner_w = (rect.width - 2.0 * TILE_INSET).max(0.0);
    let lines = specimen_lines(tile.compact);
    // The specimen is 26 columns wide: the size that fits them, never larger
    // than the tile's own scale asks.
    let base = specimen_px(scale);
    let fit = inner_w / (26.0 * 0.6);
    let size = TypeStep::Body.px_clamped(base, 7.0, base.min(fit).max(7.0));
    let px = size.get();
    let advance = crate::tray_raster::mono_advance(px).max(1.0);
    let pitch = (px * 1.32).round();
    let left = rect.x + TILE_INSET;
    let right = rect.right() - TILE_INSET;
    let columns = ((right - left) / advance).floor().max(0.0) as usize;
    // The contrast badge, top right, in the theme's own ink on a tinted chip
    // so it reads on any background.
    let badge = format!(
        "{} {}",
        contrast_grade(tile.contrast).word(),
        contrast_figure(tile.contrast)
    );
    let badge_px = TypeStep::Caption.px(11.0 * scale.clamp(0.85, 2.0));
    let badge_w =
        crate::tray_raster::ui_text_width_for(TextFace::Ui, &badge, badge_px.get()) + 10.0;
    let badge_h = (badge_px.get() + 6.0).round();
    let badge_color = match contrast_grade(tile.contrast) {
        ContrastGrade::Aaa | ContrastGrade::Aa => tile.fg,
        ContrastGrade::Low => tile.ansi[1],
    };
    let mut top = rect.y + TILE_INSET;
    // A theme that names type settings carries an "Aa" chip beside the
    // contrast badge, in the CHROME face: descriptive, not a sample of the
    // theme's font (no font is parsed on the gallery's paint path).
    let aa_w = if tile.typed {
        crate::tray_raster::ui_text_width_for(TextFace::Ui, "Aa", badge_px.get()) + 10.0
    } else {
        0.0
    };
    let first_line_room = right - badge_w - 6.0 - if tile.typed { aa_w + 4.0 } else { 0.0 };
    if tile.typed && badge_w + aa_w + 4.0 + 2.0 * TILE_INSET <= rect.width {
        let chip = crate::native_appearance::mix_srgb(bg, unpack(tile.fg), 0.12);
        let x = right - badge_w - 4.0 - aa_w;
        prims.push(DrawPrim::Panel {
            x,
            y: top,
            w: aa_w,
            h: badge_h,
            radius: badge_h / 2.0,
            fill: rgba(chip, 255),
        });
        prims.push(text_prim(
            x + 5.0,
            crate::tray_raster::row_baseline(top, badge_h, badge_px.get()),
            "Aa".to_string(),
            badge_px,
            TextWeight::Bold,
            TextFace::Ui,
            ink(tile.fg),
        ));
    }
    if badge_w + 2.0 * TILE_INSET <= rect.width {
        let chip = crate::native_appearance::mix_srgb(bg, unpack(tile.fg), 0.12);
        prims.push(DrawPrim::Panel {
            x: right - badge_w,
            y: top,
            w: badge_w,
            h: badge_h,
            radius: badge_h / 2.0,
            fill: rgba(chip, 255),
        });
        prims.push(text_prim(
            right - badge_w + 5.0,
            crate::tray_raster::row_baseline(top, badge_h, badge_px.get()),
            badge,
            badge_px,
            TextWeight::Bold,
            TextFace::Ui,
            ink(badge_color),
        ));
    }
    let swatch_band = 2.0 * SWATCH_SIZE + SWATCH_GAP;
    let text_bottom = rect.bottom() - TILE_INSET - swatch_band - 6.0;
    for (index, runs) in lines.iter().enumerate() {
        if top + pitch > text_bottom + 0.5 {
            break;
        }
        let baseline = crate::tray_raster::row_baseline(top, pitch, px).round();
        // The first line shares its row with the badge.
        let limit = if index == 0 {
            ((first_line_room - left) / advance).floor().max(0.0) as usize
        } else {
            columns
        };
        let mut column = 0usize;
        for (text, run_ink, bold) in runs {
            if column >= limit {
                break;
            }
            let room = limit - column;
            let shown: String = text.chars().take(room).collect();
            let count = shown.chars().count();
            let x = left + column as f32 * advance;
            let color = match run_ink {
                Ink::Fg => tile.fg,
                Ink::Ansi(slot) => tile.ansi[*slot],
                Ink::Selected => {
                    prims.push(DrawPrim::Panel {
                        x,
                        y: top + 1.0,
                        w: count as f32 * advance,
                        h: pitch - 2.0,
                        radius: 2.0,
                        fill: ink(tile.selection),
                    });
                    tile.selection_fg.unwrap_or(tile.fg)
                }
            };
            if !shown.trim().is_empty() {
                prims.push(text_prim(
                    x,
                    baseline,
                    shown,
                    size,
                    if *bold {
                        TextWeight::Bold
                    } else {
                        TextWeight::Regular
                    },
                    TextFace::Mono,
                    ink(color),
                ));
            }
            column += count;
        }
        top += pitch;
    }
    // The prompt waiting for input: `$ ` and a block cursor.
    if top + pitch <= text_bottom + 0.5 && columns >= 3 {
        let baseline = crate::tray_raster::row_baseline(top, pitch, px).round();
        prims.push(text_prim(
            left,
            baseline,
            "$".to_string(),
            size,
            TextWeight::Bold,
            TextFace::Mono,
            ink(tile.ansi[2]),
        ));
        prims.push(DrawPrim::Panel {
            x: left + 2.0 * advance,
            y: top + 1.0,
            w: advance,
            h: pitch - 2.0,
            radius: 1.0,
            fill: ink(tile.cursor),
        });
    }
    // All sixteen ANSI colours: normal above bright. A swatch that all but
    // vanishes into the background gets a hairline, so a missing colour is
    // visibly a colour the theme set, not a gap.
    let swatch_y = rect.bottom() - TILE_INSET - swatch_band;
    let cell = ((right - left + SWATCH_GAP) / 8.0 - SWATCH_GAP).clamp(4.0, 3.0 * SWATCH_SIZE);
    for (index, color) in tile.ansi.iter().copied().enumerate() {
        let x = left + (index % 8) as f32 * (cell + SWATCH_GAP);
        let y = swatch_y + (index / 8) as f32 * (SWATCH_SIZE + SWATCH_GAP);
        prims.push(DrawPrim::Panel {
            x,
            y,
            w: cell,
            h: SWATCH_SIZE,
            radius: 2.0,
            fill: ink(color),
        });
        if aterm_messages::ink::contrast(unpack(color), bg) < 1.15 {
            prims.push(DrawPrim::Stroke {
                x: x + 0.5,
                y: y + 0.5,
                w: (cell - 1.0).max(0.0),
                h: SWATCH_SIZE - 1.0,
                radius: 2.0,
                width: 1.0,
                color: rgba(unpack(tile.fg), 90),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signature themes lead, then every other built-in (Default among
    /// them), then the person's themes by name; tiers never interleave, and
    /// every slug is unique and key-safe.
    #[test]
    fn showcase_cards_put_signature_themes_first() {
        let mine = |name: &str, bg: Rgb| {
            let mut scheme = ColorScheme {
                name: name.to_string(),
                background: bg,
                ..ColorScheme::default()
            };
            scheme.appearance = Appearance::Dark;
            (name.to_string(), scheme)
        };
        let themes = crate::app_config::ThemeCatalog::from_schemes([
            mine("zebra", Rgb::new(1, 2, 3)),
            mine("Alpine Night", Rgb::new(4, 5, 6)),
        ]);
        let cards = showcase_cards(&themes);
        let signature: Vec<&str> = aterm_types::scheme::builtin_theme_list()
            .iter()
            .filter(|builtin| builtin.signature)
            .map(|builtin| builtin.name)
            .collect();
        assert!(!signature.is_empty(), "aterm ships signature themes");
        let leading: Vec<&str> = cards
            .iter()
            .take(signature.len())
            .map(|card| card.id.as_str())
            .collect();
        assert_eq!(leading, signature);
        let tiers: Vec<ShowcaseTier> = cards.iter().map(|card| card.tier).collect();
        let mut sorted = tiers.clone();
        sorted.sort();
        assert_eq!(tiers, sorted, "tiers are contiguous, in gallery order");
        assert!(
            cards
                .iter()
                .any(|card| card.id == "Default" && card.tier == ShowcaseTier::Builtin)
        );
        let yours: Vec<&str> = cards
            .iter()
            .filter(|card| card.tier == ShowcaseTier::Yours)
            .map(|card| card.id.as_str())
            .collect();
        assert_eq!(yours, ["Alpine Night", "zebra"]);
        let mut slugs = std::collections::BTreeSet::new();
        for card in &cards {
            assert!(
                card.slug
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
                "{}",
                card.slug
            );
            assert!(slugs.insert(card.slug.clone()), "duplicate {}", card.slug);
        }
        // Every built-in is in the gallery exactly once, and after them,
        // every pack theme the folder has no file for.
        assert_eq!(
            cards
                .iter()
                .filter(|card| card.tier < ShowcaseTier::Yours)
                .count(),
            aterm_types::scheme::builtin_theme_list().len()
        );
        assert_eq!(
            cards
                .iter()
                .filter(|card| card.tier == ShowcaseTier::Pack)
                .count(),
            aterm_theme_pack::themes().len()
        );
    }

    /// The accessible label is `<name>, dark|light, text contrast X:1` for
    /// every card, and its appearance word agrees with the one dark/light rule
    /// for a theme that measured it.
    #[test]
    fn showcase_card_label_is_accessible() {
        let cards = showcase_cards(&crate::app_config::ThemeCatalog::empty());
        for card in &cards {
            let label = card.accessible_label();
            let rest = label
                .strip_prefix(&format!("{}, ", card.label))
                .unwrap_or_else(|| panic!("{label}"));
            let (word, contrast) = rest.split_once(", text contrast ").expect("contrast");
            assert!(matches!(word, "dark" | "light"), "{label}");
            let ratio = contrast.strip_suffix(":1").expect(":1");
            let parsed: f64 = ratio.parse().expect("a number");
            assert!(parsed >= 1.0 && parsed <= card.contrast + 1e-9, "{label}");
            assert!(
                !ratio.contains('.') || ratio.split('.').nth(1).is_some_and(|d| d.len() == 1),
                "{label}"
            );
        }
    }

    /// A tile value reads back to the card's colours, and the painted tile
    /// draws all sixteen swatches and never a terminal specimen.
    #[test]
    fn card_tile_round_trips_its_colours_and_paints_without_a_terminal() {
        let cards = showcase_cards(&crate::app_config::ThemeCatalog::empty());
        let roles = Roles::from_theme(aterm_render::Theme::default());
        for card in &cards {
            for compact in [false, true] {
                let value = card.tile_value(compact);
                let tile = parse_tile(&value).expect("tile value parses");
                assert_eq!(
                    (tile.fg, tile.bg, tile.cursor, tile.selection),
                    (card.fg, card.bg, card.cursor, card.selection)
                );
                assert_eq!(tile.selection_fg, card.selection_fg);
                assert_eq!(tile.ansi, card.ansi);
                assert_eq!(tile.compact, compact);
                let mut prims = Vec::new();
                paint_card_tile(
                    &mut prims,
                    LogicalRect::new(0.0, 0.0, 240.0, tile_height(compact, 1.0)),
                    Some(&value),
                    &roles,
                );
                assert!(
                    !prims
                        .iter()
                        .any(|prim| matches!(prim, DrawPrim::TerminalSpecimen { .. }))
                );
                for color in card.ansi {
                    let fill = rgba(unpack(color), 255);
                    assert!(
                        prims.iter().any(|prim| matches!(
                            prim,
                            DrawPrim::Panel { fill: f, w, .. } if *f == fill && *w < 40.0
                        )),
                        "{}: swatch #{color:06x}",
                        card.id
                    );
                }
            }
        }
        let mut prims = Vec::new();
        paint_card_tile(
            &mut prims,
            LogicalRect::new(0.0, 0.0, 240.0, 160.0),
            Some("not a tile"),
            &roles,
        );
        assert!(prims.is_empty(), "a malformed value paints nothing");
    }

    #[test]
    fn contrast_figures_round_down_and_grade_by_wcag() {
        assert_eq!(contrast_figure(2.96), "2.9");
        assert_eq!(contrast_figure(7.0), "7");
        assert_eq!(contrast_grade(7.0), ContrastGrade::Aaa);
        assert_eq!(contrast_grade(6.99), ContrastGrade::Aa);
        assert_eq!(contrast_grade(4.5), ContrastGrade::Aa);
        assert_eq!(contrast_grade(4.49), ContrastGrade::Low);
    }
}
