// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Themes' rows.

use aterm_types::{Appearance, ColorScheme, Rgb};

use crate::native_ui::LogicalRect;
use crate::settings::Roles;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

/// Which section of the chooser a row belongs to, in list order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Section {
    /// Theme files that did not load.
    Attention,
    /// aterm's own signature themes (Aterm Night, Aterm Day, Hearth, Linen), in the registry's order.
    Signature,
    /// Every other built-in, Default included, in the registry's order.
    Builtin,
    /// The person's own theme files that loaded, by name.
    Yours,
    /// The community pack's themes not yet in the themes folder.
    Community,
}

impl Section {
    /// The section's caption, `None` for the signature themes.
    pub(crate) const fn caption(self) -> Option<&'static str> {
        match self {
            Self::Attention => Some("NEEDS ATTENTION"),
            Self::Signature => None,
            Self::Builtin => Some("BUILT IN"),
            Self::Yours => Some("YOURS"),
            Self::Community => Some("COMMUNITY"),
        }
    }

    /// The section's caption where other rows precede it (`after_rows`).
    pub(crate) const fn caption_after_rows(self, after_rows: bool) -> Option<&'static str> {
        match self {
            Self::Signature if after_rows => Some("ATERM"),
            _ => self.caption(),
        }
    }

    /// The section's key word (`settings/showcase/section/<word>`).
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Attention => "attention",
            Self::Signature => "signature",
            Self::Builtin => "builtin",
            Self::Yours => "yours",
            Self::Community => "community",
        }
    }
}

/// The format the parser names another terminal's theme by (`label`,
/// [`ForeignFormat::label`](aterm_types::scheme::ForeignFormat::label)), in the
/// one word a chooser row has room for: `JSON` for `JSON (Windows Terminal)`.
fn format_word(label: &str) -> &str {
    use aterm_types::scheme::ForeignFormat;
    for format in [
        ForeignFormat::Kitty,
        ForeignFormat::Ghostty,
        ForeignFormat::Plist,
        ForeignFormat::TerminalApp,
        ForeignFormat::Toml,
        ForeignFormat::WezTerm,
        ForeignFormat::Json,
    ] {
        if format.label() == label {
            return match format {
                ForeignFormat::Kitty => "kitty",
                ForeignFormat::Ghostty => "Ghostty",
                ForeignFormat::Plist | ForeignFormat::TerminalApp => "plist",
                ForeignFormat::Toml | ForeignFormat::WezTerm => "TOML",
                ForeignFormat::Json => "JSON",
            };
        }
    }
    label.split_whitespace().next().unwrap_or(label)
}

/// A theme file that did not load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttentionRow {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) reason: String,
    /// [`reason_summary`] of `reason`.
    pub(crate) summary: String,
}

/// A theme file's load failure (`reason`, as the catalog words it) in the few words a chooser row has room for.
pub(crate) fn reason_summary(reason: &str) -> String {
    let reason = reason.trim();
    let capitalized = |text: &str| {
        let mut chars = text.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect::<String>())
            .unwrap_or_default()
    };
    if let Some((_, format)) = reason.split_once(" (this looks like a ") {
        let format = format.split_once(" theme").map_or(format, |(name, _)| name);
        return format!("Import\u{2026} converts it ({})", format_word(format));
    }
    let (line, what) = reason
        .strip_prefix("theme parse error on line ")
        .and_then(|rest| rest.split_once(": "))
        .and_then(|(line, what)| line.parse::<usize>().ok().map(|line| (line, what)))
        .unwrap_or((0, reason));
    let unquote = |text: &str| text.trim().trim_matches(['"', '\'']).to_string();
    let before = |text: &str, end: &str| {
        text.split_once(end)
            .map_or(text, |(head, _)| head)
            .to_string()
    };
    let what = if let Some(rest) = what.strip_prefix("invalid #RRGGBB or #RGB colour for ") {
        format!("{} isn\u{2019}t a color", unquote(&before(rest, ": ")))
    } else if let Some(key) = what.strip_prefix("unknown key ") {
        format!("{} is no theme key", unquote(key))
    } else if let Some(key) = what.strip_prefix("empty value for key ") {
        format!("{} has no value", unquote(key))
    } else if what.starts_with("missing '=' in ") {
        "no = on the line".to_string()
    } else if let Some(rest) = what.strip_prefix("duplicate key ") {
        format!("{} is set twice", unquote(&before(rest, " (")))
    } else if let Some(rest) = what.strip_prefix("duplicate colour index ") {
        format!("color {} is set twice", before(rest, " (").trim())
    } else if let Some(key) = what.strip_prefix("missing required key ") {
        format!("no {}", unquote(key))
    } else if let Some(rest) = what.strip_prefix("invalid value for ") {
        format!(
            "{} has a value aterm can\u{2019}t read",
            unquote(&before(rest, ": "))
        )
    } else {
        what.to_string()
    };
    if line > 0 {
        format!("Line {line}: {what}")
    } else {
        capitalized(&what)
    }
}

/// The theme files in the folder that did not load, by name, each with a slug no row of `cards` uses.
pub(crate) fn attention_rows(
    themes: &crate::app_config::ThemeCatalog,
    cards: &[ShowcaseCard],
) -> Vec<AttentionRow> {
    let mut taken: std::collections::BTreeSet<String> =
        cards.iter().map(|card| card.slug.clone()).collect();
    let mut rows: Vec<AttentionRow> = themes
        .listing()
        .filter_map(|(id, status)| {
            status.reason().map(|reason| AttentionRow {
                id: id.to_string(),
                slug: String::new(),
                summary: reason_summary(&reason),
                reason,
            })
        })
        .collect();
    rows.sort_by_key(|row| row.id.to_lowercase());
    for row in &mut rows {
        let base = slug_of(&row.id);
        let mut slug = base.clone();
        let mut n = 2;
        while !taken.insert(slug.clone()) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        row.slug = slug;
    }
    rows
}

/// One theme in the chooser.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShowcaseCard {
    /// The catalog id.
    pub(crate) id: String,
    /// A key- and wire-safe spelling of the id, unique in the chooser (`ctl act` takes a key as one word, and ids hold spaces).
    pub(crate) slug: String,
    pub(crate) section: Section,
    /// What the person sees.
    pub(crate) label: String,
    /// The theme's description, its first sentence.
    pub(crate) description: String,
    /// The appearance the theme is drawn for (declared or measured).
    pub(crate) appearance: Appearance,
    /// The theme's colours, packed `0x00RRGGBB`.
    pub(crate) fg: u32,
    pub(crate) bg: u32,
    pub(crate) cursor: u32,
    pub(crate) selection: u32,
    /// The selected text's colour.
    pub(crate) selection_fg: Option<u32>,
    pub(crate) ansi: [u32; 16],
    /// Text contrast, foreground on background.
    pub(crate) contrast: f64,
    /// The theme's type settings in one line ("Berkeley Mono · 1.15×"), `None` for a theme that names none.
    pub(crate) typography: Option<String>,
    /// The same in words for a screen reader ("sets font Berkeley Mono, line height 1.15").
    pub(crate) typography_spoken: Option<String>,
    /// The theme file was written for a newer theme schema and some of its settings were skipped.
    pub(crate) partial: bool,
    /// The label, id, description, type summary and section words lowercased
    /// once, for the filter: a keystroke in the filter matches every row
    /// without allocating.
    search: [String; 5],
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
        section: Section,
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
                match section {
                    Section::Yours => "yours mine".to_string(),
                    Section::Community => "community pack".to_string(),
                    _ => String::new(),
                },
            ],
            typography: None,
            typography_spoken: None,
            partial: false,
            id: id.to_string(),
            slug: String::new(),
            section,
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
        self.partial = partial;
        self.search[3] = self
            .typography
            .as_ref()
            .map(|summary| format!("{} font type", summary.to_lowercase()))
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

    /// The row's spoken value.
    pub(crate) fn row_value(&self, in_use: bool, trying: bool) -> String {
        let mut value = format!(
            "{}, contrast {}:1 {}",
            self.appearance_word(),
            contrast_figure(self.contrast),
            contrast_grade(self.contrast).word()
        );
        if in_use {
            value.push_str(", in use");
        }
        if trying {
            value.push_str(", trying");
        }
        if let Some(spoken) = &self.typography_spoken {
            value.push_str(", ");
            value.push_str(spoken);
        }
        if self.section == Section::Community {
            value.push_str(", community: from the theme pack, added to your themes folder on Keep");
        }
        value
    }

    /// The paint-only slab's value.
    pub(crate) fn slab_value(&self, narrow: bool) -> String {
        let mut value = Vec::with_capacity(60);
        for color in [
            self.bg,
            self.fg,
            self.ansi[1],
            self.ansi[2],
            self.ansi[4],
            self.ansi[5],
        ] {
            // A colour as `{:06x}` spells it, in ASCII with no formatter: a page builds one a row.
            let mut at = (8 - color.leading_zeros() as usize / 4).max(6);
            while at > 0 {
                at -= 1;
                value.push(b"0123456789abcdef"[(color >> (4 * at)) as usize & 0xf]);
            }
            value.push(b' ');
        }
        value.extend_from_slice(if narrow { b"narrow" } else { b"wide" });
        String::from_utf8(value).unwrap_or_default()
    }

    /// Whether the lowercased, trimmed filter `query` admits the row at `reach` (`ShowcaseState::reach_for`).
    pub(crate) fn matches(
        &self,
        query: &str,
        reach: crate::native_config_language::MatchReach,
    ) -> bool {
        if query.is_empty() {
            return true;
        }
        let matches = |candidate: &str| {
            crate::native_config_language::candidate_match_score(candidate, query, reach).is_some()
        };
        self.search.iter().any(|candidate| matches(candidate)) || matches(self.appearance_word())
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

/// A contrast ratio to one decimal, rounded down so a floor is never overstated (2.96 reads 2.9), and without a trailing `.0`.
pub(crate) fn contrast_figure(ratio: f64) -> String {
    let tenths = (ratio * 10.0).floor() / 10.0;
    if tenths.fract() == 0.0 {
        format!("{tenths:.0}")
    } else {
        format!("{tenths:.1}")
    }
}

/// Every theme the catalog can draw, as chooser rows, in chooser order.
pub(crate) fn showcase_cards(themes: &crate::app_config::ThemeCatalog) -> Vec<ShowcaseCard> {
    let builtins = aterm_types::scheme::builtin_theme_list();
    let mut cards = Vec::with_capacity(builtins.len() + 8);
    for section in [Section::Signature, Section::Builtin] {
        for builtin in builtins
            .iter()
            .filter(|builtin| builtin.signature == (section == Section::Signature))
        {
            let Some(scheme) = aterm_types::scheme::builtin(builtin.name) else {
                continue;
            };
            cards.push(ShowcaseCard::new(
                builtin.name,
                section,
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
                    Section::Yours,
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
    for (theme, parsed) in aterm_theme_pack::themes().iter().zip(parsed_pack()) {
        if themes.lists(theme.id) {
            continue;
        }
        let Some(parsed) = parsed else {
            continue;
        };
        cards.push(
            ShowcaseCard::new(
                theme.id,
                Section::Community,
                &parsed.scheme.name,
                parsed.description.as_deref(),
                &parsed.scheme,
            )
            .with_typography(&parsed.typography, parsed.skipped_count > 0),
        );
    }
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

/// Every pack theme parsed, in [`aterm_theme_pack::themes`]'s order, once per
/// process: the pack is embedded, so every Settings view built over it
/// ([`showcase_cards`]) would parse the same text into the same themes.
fn parsed_pack() -> &'static [Option<aterm_types::scheme::ParsedTheme>] {
    static PARSED: std::sync::OnceLock<Box<[Option<aterm_types::scheme::ParsedTheme>]>> =
        std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        aterm_theme_pack::themes()
            .iter()
            .map(|theme| aterm_types::scheme::parse_theme_str(theme.text).ok())
            .collect()
    })
}

/// The slab's size.
pub(crate) const SLAB_WIDTH: f32 = 56.0;
pub(crate) const SLAB_NARROW_WIDTH: f32 = 40.0;
pub(crate) const SLAB_HEIGHT: f32 = 20.0;

/// A slab value read back ([`ShowcaseCard::slab_value`]).
struct SlabColors {
    bg: u32,
    fg: u32,
    dots: [u32; 4],
    narrow: bool,
}

fn parse_slab(value: &str) -> Option<SlabColors> {
    let mut words = value.split_whitespace();
    let mut color = || {
        let w = words.next()?;
        w.bytes()
            .all(|b| b.is_ascii_hexdigit())
            .then(|| u32::from_str_radix(w, 16).ok())
            .flatten()
    };
    let bg = color()?;
    let fg = color()?;
    let dots = [color()?, color()?, color()?, color()?];
    let narrow = value.split_whitespace().nth(6)? == "narrow";
    Some(SlabColors {
        bg,
        fg,
        dots,
        narrow,
    })
}

/// Paint one row's slab into `rect`.
pub(crate) fn paint_row_slab(
    prims: &mut Vec<DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &Roles,
) {
    let Some(slab) = value.and_then(parse_slab) else {
        return;
    };
    if rect.width < 16.0 || rect.height < 12.0 {
        return;
    }
    let ink = |color: u32| rgba(unpack(color), 255);
    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: rect.y,
        w: rect.width,
        h: rect.height,
        radius: 4.0,
        fill: ink(slab.bg),
    });
    prims.push(DrawPrim::Stroke {
        x: rect.x + 0.5,
        y: rect.y + 0.5,
        w: (rect.width - 1.0).max(0.0),
        h: (rect.height - 1.0).max(0.0),
        radius: 3.5,
        width: 1.0,
        color: rgba(roles.separator, 200),
    });
    let px = TypeStep::Caption.px(11.0);
    prims.push(text_prim(
        rect.x + 4.0,
        crate::tray_raster::row_baseline(rect.y, rect.height, px.get()),
        "Aa".to_string(),
        px,
        TextWeight::Bold,
        TextFace::Ui,
        ink(slab.fg),
    ));
    const DOT: f32 = 5.0;
    const DOT_GAP: f32 = 3.0;
    let words = crate::tray_raster::ui_text_width("Aa", px.get());
    let room = rect.width - 4.0 - words - 4.0 - 4.0;
    let fit = (((room + DOT_GAP) / (DOT + DOT_GAP)).floor().max(0.0) as usize)
        .min(if slab.narrow { 2 } else { slab.dots.len() });
    let dots: &[u32] = &slab.dots[..fit];
    let span = (dots.len() as f32 * (DOT + DOT_GAP) - DOT_GAP).max(0.0);
    let mut x = rect.right() - 4.0 - span;
    let y = rect.y + (rect.height - DOT) / 2.0;
    for color in dots {
        prims.push(DrawPrim::Panel {
            x,
            y,
            w: DOT,
            h: DOT,
            radius: DOT / 2.0,
            fill: ink(*color),
        });
        x += DOT + DOT_GAP;
    }
}

#[cfg(test)]
#[path = "settings_preview_theme_card_tests.rs"]
mod tests;
