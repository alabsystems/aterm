// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Themes' rows: one per theme the catalog can draw or the
//! community pack offers, each with a 56×20 slab (40×20 on a narrow
//! chooser) painted in that theme's own colours.
//!
//! The row list is built once per catalog generation ([`showcase_cards`])
//! from the schemes the config worker already parsed and the embedded pack,
//! so hovering, focusing or scrolling the chooser resolves no theme and reads
//! no file. A row's colours are the theme as authored: the person's
//! `aterm.toml` colour overrides would make every slab the same, so they
//! appear only in the demo and in the live trial.
//!
//! The slab itself ([`paint_row_slab`]) is the theme's background, "Aa" in
//! its foreground, and dots of its red, green, blue and magenta — as many as
//! fit, two on the narrow slab. It never builds a terminal or forks a font, so a page
//! of rows costs what a page of buttons costs. The faithful view is the
//! demo.

use aterm_types::{Appearance, ColorScheme, Rgb};

use crate::native_ui::LogicalRect;
use crate::settings::Roles;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

/// Which section of the chooser a row belongs to, in list order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Section {
    /// Theme files that did not load: listed first, never tried.
    Attention,
    /// aterm's own signature themes (Aterm Night, Aterm Day, Hearth, Linen),
    /// in the registry's order. They lead the list with no caption — save
    /// under NEEDS ATTENTION, where they take one of their own
    /// ([`Section::caption_after_rows`]).
    Signature,
    /// Every other built-in, Default included, in the registry's order.
    Builtin,
    /// The person's own theme files that loaded, by name.
    Yours,
    /// The community pack's themes not yet in the themes folder: tried from
    /// the embedded pack, added on Keep.
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

    /// The section's caption where other rows precede it (`after_rows`):
    /// [`Self::caption`], and for the signature themes ATERM — leading the
    /// list they need none, but under NEEDS ATTENTION (a theme file that did
    /// not load) uncaptioned rows read as that section's, its caption
    /// repeated over whichever of them starts a screen (the review of the
    /// themes page, ox4-1: Linen, and Aterm Day in use, listed as needing
    /// attention).
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
/// [`ForeignFormat::label`]), in the one word a chooser row has room for:
/// `JSON` for `JSON (Windows Terminal)`. A label this does not know keeps
/// its first word.
fn format_word(label: &str) -> &str {
    use aterm_types::scheme::ForeignFormat;
    for format in [
        ForeignFormat::Kitty,
        ForeignFormat::Ghostty,
        ForeignFormat::Plist,
        ForeignFormat::Toml,
        ForeignFormat::Json,
    ] {
        if format.label() == label {
            return match format {
                ForeignFormat::Kitty => "kitty",
                ForeignFormat::Ghostty => "Ghostty",
                ForeignFormat::Plist => "plist",
                ForeignFormat::Toml => "TOML",
                ForeignFormat::Json => "JSON",
            };
        }
    }
    label.split_whitespace().next().unwrap_or(label)
}

/// A theme file that did not load: its id, a key-safe slug unique among the
/// rows, and why — whole, and in the few words its row has room for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttentionRow {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) reason: String,
    /// [`reason_summary`] of `reason`.
    pub(crate) summary: String,
}

/// A theme file's load failure (`reason`, as the catalog words it) in the
/// few words a chooser row has room for: where in the file, then what is
/// wrong there, the key first (`Line 2: background isn't a color`), so the
/// words a row's edge keeps are the ones that say where to look. Another
/// terminal's theme says what to do first, its format in a word after
/// (`Import… converts it (JSON)`): the whole of it fits a row at every
/// chooser width. A reason this does not know reads as written, its parser
/// preamble gone.
pub(crate) fn reason_summary(reason: &str) -> String {
    let reason = reason.trim();
    let capitalized = |text: &str| {
        let mut chars = text.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect::<String>())
            .unwrap_or_default()
    };
    // Another terminal's theme: the whole file is, wherever the parser
    // stopped.
    if let Some((_, format)) = reason.split_once(" (this looks like a ") {
        let format = format.split_once(" theme").map_or(format, |(name, _)| name);
        return format!("Import\u{2026} converts it ({})", format_word(format));
    }
    // `theme parse error on line N: <what>`, line 0 for the whole file.
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

/// The theme files in the folder that did not load, by name, each with a
/// slug no row of `cards` uses.
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
    /// The catalog id: what `theme = "…"` names.
    pub(crate) id: String,
    /// A key- and wire-safe spelling of the id, unique in the chooser
    /// (`ctl act` takes a key as one word, and ids hold spaces).
    pub(crate) slug: String,
    pub(crate) section: Section,
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
    /// The theme file was written for a newer theme schema and some of its
    /// settings were skipped.
    pub(crate) partial: bool,
    /// The label, id, description, type summary and section words lowercased
    /// once, for the filter: a keystroke in the filter matches every row
    /// without allocating. A theme that names a font matches "font" and
    /// "type"; a file of the person's "yours" and "mine"; a pack theme
    /// "community" and "pack".
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

    /// The row's spoken value: `dark|light, contrast X:1 AAA`, then
    /// `, in use` / `, trying`, the type it sets in words, and where a
    /// community theme comes from.
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

    /// The paint-only slab's value: the background, the foreground and the
    /// four hues it dots (red, green, blue, magenta), packed hex, then
    /// whether the narrow slab is asked for. One string because a custom
    /// paint node carries one; [`paint_row_slab`] reads it back.
    pub(crate) fn slab_value(&self, narrow: bool) -> String {
        let mut value = String::with_capacity(48);
        for color in [
            self.bg,
            self.fg,
            self.ansi[1],
            self.ansi[2],
            self.ansi[4],
            self.ansi[5],
        ] {
            value.push_str(&format!("{color:06x} "));
        }
        value.push_str(if narrow { "narrow" } else { "wide" });
        value
    }

    /// Whether the chooser's filter `query` (lowercased, trimmed) admits the
    /// row at `reach` (the filter's over the whole list:
    /// `ShowcaseState::reach_for`): its name, id, description, appearance or
    /// section words, by the ladder Settings search and the theme picker use.
    /// An empty query admits every row.
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

/// Every theme the catalog can draw, as chooser rows, in chooser order:
/// the signature themes (registry order), then every other built-in, Default
/// included (registry order), then the person's own theme files that loaded,
/// by name, then the community pack's themes the folder has no file for
/// (by id). Built from the parsed catalog and the embedded pack: no file is
/// read.
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
    // Then every pack theme the folder has no file for at all: a file that
    // did not load keeps its Needs-attention row instead of a card here.
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

/// Every pack theme parsed, in [`aterm_theme_pack::themes`]'s order, once per
/// process: the pack is embedded, so every Settings view built over it
/// ([`showcase_cards`]) would parse the same text into the same themes. `None`
/// for a pack file that does not parse.
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

/// The slab's size: wide ("Aa" and four hues), and narrow ("Aa" and two;
/// a chooser under 240pt).
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
    let mut color = || u32::from_str_radix(words.next()?, 16).ok();
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

/// Paint one row's slab into `rect`: the theme's background with a hairline
/// rim (so a background equal to the panel still reads as an object), "Aa"
/// in its foreground, and dots of its red, green, blue and magenta (two on a
/// narrow slab). Panels and text only, from the slab's value; a malformed
/// value paints nothing.
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
    // As many of the hues as fit beside "Aa" (two on the narrow slab).
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
mod tests {
    use super::*;

    /// A NEEDS ATTENTION ROW SAYS WHERE, THEN WHAT, IN A ROW'S FEW WORDS: each
    /// way the parser refuses a theme file reads as its line, its key and the
    /// trouble, never the parser's preamble; another terminal's theme says
    /// Import converts it, then its format. The reasons come from the parser
    /// itself, so a change to its words shows here. Negative control: a
    /// reason the summary has no words for keeps its own, after its line.
    #[test]
    fn a_load_failure_reads_as_its_line_and_key() {
        let dir = aterm_tempfile::tempdir().expect("a themes folder");
        let base = "foreground = #dddddd\nbackground = #101010\n";
        let files = [
            (
                "a",
                format!("{base}cursor = #zzz\n"),
                "Line 3: cursor isn\u{2019}t a color",
            ),
            (
                "b",
                format!("{base}wibble = #123456\n"),
                "Line 3: wibble is no theme key",
            ),
            (
                "c",
                format!("{base}cursor =\n"),
                "Line 3: cursor has no value",
            ),
            ("d", format!("{base}cursor\n"), "Line 3: no = on the line"),
            (
                "e",
                format!("{base}background = #000000\n"),
                "Line 3: background is set twice",
            ),
            (
                "f",
                format!("{base}color1 = #ff0000\ncolor1 = #00ff00\n"),
                "Line 4: color 1 is set twice",
            ),
            ("g", "background = #101010\n".to_string(), "No foreground"),
            (
                "h",
                "{\"background\": \"#101010\"}\n".to_string(),
                "Import\u{2026} converts it (JSON)",
            ),
            (
                "i",
                format!("{base}appearance = sideways\n"),
                "Line 3: appearance must be dark|light, got \"sideways\"",
            ),
        ];
        for (name, text, _) in &files {
            std::fs::write(dir.path().join(format!("{name}.conf")), text).unwrap();
        }
        let rows = attention_rows(
            &crate::app_config::ThemeCatalog::discover_in(dir.path()),
            &[],
        );
        for (name, _, summary) in files {
            let row = rows
                .iter()
                .find(|row| row.id == name)
                .unwrap_or_else(|| panic!("{name} needs attention"));
            assert_eq!(row.summary, summary, "{name}: {}", row.reason);
            assert!(
                row.reason.starts_with("theme parse error on line "),
                "{name}"
            );
        }
    }

    /// Signature themes lead, then every other built-in (Default among
    /// them), then the person's themes by name, then the community pack's;
    /// sections never interleave, and every slug is unique and key-safe.
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
        let tiers: Vec<Section> = cards.iter().map(|card| card.section).collect();
        let mut sorted = tiers.clone();
        sorted.sort();
        assert_eq!(tiers, sorted, "sections are contiguous, in list order");
        assert!(
            cards
                .iter()
                .any(|card| card.id == "Default" && card.section == Section::Builtin)
        );
        let yours: Vec<&str> = cards
            .iter()
            .filter(|card| card.section == Section::Yours)
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
        // Every built-in is in the chooser exactly once, and after them,
        // every pack theme the folder has no file for.
        assert_eq!(
            cards
                .iter()
                .filter(|card| card.section < Section::Yours)
                .count(),
            aterm_types::scheme::builtin_theme_list().len()
        );
        assert_eq!(
            cards
                .iter()
                .filter(|card| card.section == Section::Community)
                .count(),
            aterm_theme_pack::themes().len()
        );
    }

    /// A row's value leads with its appearance and its contrast figure and
    /// grade, rounded down as the figure always is, and says in use and
    /// trying only when they hold.
    #[test]
    fn a_rows_value_reads_its_appearance_contrast_and_state() {
        let cards = showcase_cards(&crate::app_config::ThemeCatalog::empty());
        for card in &cards {
            let value = card.row_value(false, false);
            let (word, rest) = value.split_once(", contrast ").expect("contrast");
            assert_eq!(word, card.appearance_word());
            let (figure, grade) = rest.split_once(":1 ").expect(":1");
            assert_eq!(figure, contrast_figure(card.contrast));
            assert!(grade.starts_with(contrast_grade(card.contrast).word()));
            assert!(!value.contains("in use") && !value.contains("trying"));
            let both = card.row_value(true, true);
            assert!(
                both.contains(", in use") && both.contains(", trying"),
                "{both}"
            );
        }
    }

    /// The slab paints the row's own colours: its background fills the slab,
    /// "Aa" is inked in its foreground, and its four hues are dots — for
    /// three themes of different looks. A card with foreground and
    /// background swapped paints the other way round (the negative control:
    /// the audit tells the two apart), and a malformed value paints nothing.
    #[test]
    fn the_row_slab_paints_the_cards_own_colours() {
        let cards = showcase_cards(&crate::app_config::ThemeCatalog::empty());
        let roles = Roles::from_theme(aterm_render::Theme::default());
        let rect = LogicalRect::new(0.0, 0.0, SLAB_WIDTH, SLAB_HEIGHT);
        let painted = |value: &str| {
            let mut prims = Vec::new();
            paint_row_slab(&mut prims, rect, Some(value), &roles);
            let fill = prims.iter().find_map(|prim| match prim {
                DrawPrim::Panel { fill, w, .. } if *w == SLAB_WIDTH => Some(*fill),
                _ => None,
            });
            let ink = prims.iter().find_map(|prim| match prim {
                DrawPrim::Text { s, color, .. } if s == "Aa" => Some(*color),
                _ => None,
            });
            let dots: Vec<_> = prims
                .iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Panel { fill, w, .. } if *w < 10.0 => Some(*fill),
                    _ => None,
                })
                .collect();
            (fill, ink, dots)
        };
        let picks = ["Aterm Night", "Aterm Day", "Dracula"];
        for id in picks {
            let card = cards.iter().find(|card| card.id == id).expect("a built-in");
            let (fill, ink, dots) = painted(&card.slab_value(false));
            assert_eq!(fill, Some(rgba(unpack(card.bg), 255)), "{id}: background");
            assert_eq!(ink, Some(rgba(unpack(card.fg), 255)), "{id}: Aa");
            assert_eq!(
                dots,
                [1, 2, 4, 5].map(|slot| rgba(unpack(card.ansi[slot]), 255)),
                "{id}: hues"
            );
            let (_, _, narrow) = painted(&card.slab_value(true));
            assert_eq!(narrow.len(), 2, "{id}: a narrow slab dots two hues");
            let mut swapped = card.clone();
            std::mem::swap(&mut swapped.fg, &mut swapped.bg);
            let (fill, ink, _) = painted(&swapped.slab_value(false));
            assert_ne!(fill, Some(rgba(unpack(card.bg), 255)), "{id}: swapped");
            assert_ne!(ink, Some(rgba(unpack(card.fg), 255)), "{id}: swapped");
        }
        let mut prims = Vec::new();
        paint_row_slab(&mut prims, rect, Some("not a slab"), &roles);
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
