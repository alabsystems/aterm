// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Offline converters from foreign terminal theme formats into aterm's native theme format.

use crate::Rgb;
use crate::scheme::{
    APPEARANCE_VALUES, Appearance, ChromeColors, ColorScheme, ForeignFormat, MAX_THEME_TEXT_CHARS,
    MAX_USER_THEME_FILE_BYTES, ParsedTheme, THEME_KEYS, ThemeKey, ThemeKeyGroup, parse_hex,
    parse_theme_str, sniff_foreign_format,
};

/// Maximum theme file size in bytes read by an import.
pub const MAX_IMPORT_BYTES: usize = MAX_USER_THEME_FILE_BYTES;

const MAX_NESTING: usize = 16;

const MAX_DROPPED: usize = 128;

const MAX_NOTES: usize = 32;

const MAX_QUOTED_CHARS: usize = 64;

/// Theme formats supported by `aterm theme import`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportFormat {
    /// aterm's own theme format (copied through the native parser).
    Native,
    /// kitty: `key value` lines.
    Kitty,
    /// Ghostty: `key = value` lines with hyphenated keys and `palette = N=#hex`.
    Ghostty,
    /// Alacritty: TOML with `[colors.primary]`, `[colors.normal]`, ….
    Alacritty,
    /// WezTerm: a TOML colour scheme with `[colors]` (`foreground`, `ansi = [ … ]`, `brights`)
    /// and `[metadata]`.
    WezTerm,
    /// iTerm2: an `.itermcolors` XML property list.
    ITerm,
    /// Terminal.app: a `.terminal` profile, an XML property list whose colours are archived
    /// `NSColor`s.
    TerminalApp,
    /// Windows Terminal: a JSON colour scheme object.
    WindowsTerminal,
    /// base16 / base24: a YAML scheme with `base00` … `base0F` (… `base17`).
    Base16,
}

impl ImportFormat {
    /// Every supported format in detection preference order.
    pub const ALL: &'static [ImportFormat] = &[
        ImportFormat::Native,
        ImportFormat::Kitty,
        ImportFormat::Ghostty,
        ImportFormat::Alacritty,
        ImportFormat::WezTerm,
        ImportFormat::ITerm,
        ImportFormat::TerminalApp,
        ImportFormat::WindowsTerminal,
        ImportFormat::Base16,
    ];

    /// User-facing name of the format.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ImportFormat::Native => "aterm",
            ImportFormat::Kitty => "kitty",
            ImportFormat::Ghostty => "Ghostty",
            ImportFormat::Alacritty => "Alacritty",
            ImportFormat::WezTerm => "WezTerm",
            ImportFormat::ITerm => "iTerm2",
            ImportFormat::TerminalApp => "Terminal.app",
            ImportFormat::WindowsTerminal => "Windows Terminal",
            ImportFormat::Base16 => "base16",
        }
    }
}

/// Converted theme colours, metadata, and dropped-key diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// The format the source was read as.
    pub format: ImportFormat,
    /// The converted colours.
    pub scheme: ColorScheme,
    /// Whether the source said whether it is dark or light.
    pub appearance_declared: bool,
    /// The source's description, if it carries one.
    pub description: Option<String>,
    /// The source's author, if it carries one.
    pub author: Option<String>,
    /// The source's keys that have no aterm equivalent, in source order and without repeats.
    pub dropped: Vec<String>,
    /// What the conversion assumed or filled in, one sentence each.
    pub notes: Vec<String>,
}

impl ImportReport {
    /// Serialize this report as a native theme file with `header` comment lines.
    #[must_use]
    pub fn to_native_conf(&self, header: &[&str]) -> String {
        to_native_conf(
            &self.scheme,
            self.appearance_declared,
            self.description.as_deref(),
            self.author.as_deref(),
            header,
        )
    }
}

/// Why a theme import failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The file is larger than [`MAX_IMPORT_BYTES`].
    TooLarge {
        /// The file's size in bytes.
        bytes: usize,
    },
    /// The file is not UTF-8 text.
    NotUtf8,
    /// The file is not in any format an import reads.
    UnknownFormat,
    /// The file is in `format` but is not well formed.
    Malformed {
        /// The format the file was read as.
        format: ImportFormat,
        /// The 1-based line of the problem, `0` when it is not one line's.
        line: usize,
        /// What is wrong.
        reason: String,
    },
    /// The file sets no colour for `key`, which every theme needs.
    MissingColour {
        /// The format the file was read as.
        format: ImportFormat,
        /// The colour it lacks (`foreground` or `background`, or a base16 slot).
        key: &'static str,
    },
}

impl core::fmt::Display for ImportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ImportError::TooLarge { bytes } => {
                f.write_str("the file is ")?;
                f.write_str(&bytes.to_string())?;
                f.write_str(" bytes; a theme file is at most ")?;
                f.write_str(&MAX_IMPORT_BYTES.to_string())?;
                f.write_str(" bytes")
            }
            ImportError::NotUtf8 => f.write_str("the file is not UTF-8 text"),
            ImportError::UnknownFormat => f.write_str(
                "the file is not a theme format aterm imports (kitty, Ghostty, Alacritty or \
                 WezTerm TOML, iTerm2 .itermcolors, Terminal.app .terminal, Windows Terminal \
                 JSON, base16 YAML)",
            ),
            ImportError::Malformed {
                format,
                line,
                reason,
            } => {
                f.write_str("not a readable ")?;
                f.write_str(format.label())?;
                f.write_str(" theme")?;
                if *line > 0 {
                    f.write_str(" (line ")?;
                    f.write_str(&line.to_string())?;
                    f.write_str(")")?;
                }
                f.write_str(": ")?;
                f.write_str(reason)
            }
            ImportError::MissingColour { format, key } => {
                f.write_str("the ")?;
                f.write_str(format.label())?;
                f.write_str(" theme sets no ")?;
                f.write_str(key)?;
                f.write_str(" colour")
            }
        }
    }
}

impl std::error::Error for ImportError {}

/// Detect the format of `bytes` and convert it into an [`ImportReport`].
/// # Errors
/// Returns [`ImportError`] if `bytes` is too large, not UTF-8, in no format an import reads, or
/// fails conversion.
pub fn import_theme(
    file_name: Option<&str>,
    bytes: &[u8],
    pick: Option<&str>,
) -> Result<ImportReport, ImportError> {
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err(ImportError::TooLarge { bytes: bytes.len() });
    }
    let text = core::str::from_utf8(bytes).map_err(|_| ImportError::NotUtf8)?;
    let format = detect_format(file_name, text).ok_or(ImportError::UnknownFormat)?;
    convert(format, text, pick)
}

/// Detect which [`ImportFormat`] `text` (from `file_name`) uses: by the extension, where a
/// `.plist` or `.toml` file's contents then tell Terminal.app from iTerm2 and WezTerm from
/// Alacritty; else by the contents.
#[must_use]
pub fn detect_format(file_name: Option<&str>, text: &str) -> Option<ImportFormat> {
    let extension = file_name
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase());
    let text = strip_bom(text);
    let sniffed = |format| sniff_foreign_format(text) == Some(format);
    match extension.as_deref() {
        Some("itermcolors") => return Some(ImportFormat::ITerm),
        Some("terminal") => return Some(ImportFormat::TerminalApp),
        Some("plist") if sniffed(ForeignFormat::TerminalApp) => {
            return Some(ImportFormat::TerminalApp);
        }
        Some("plist") => return Some(ImportFormat::ITerm),
        Some("json" | "jsonc") => return Some(ImportFormat::WindowsTerminal),
        Some("toml") if sniffed(ForeignFormat::WezTerm) => return Some(ImportFormat::WezTerm),
        Some("toml") => return Some(ImportFormat::Alacritty),
        Some("yaml" | "yml") => return Some(ImportFormat::Base16),
        _ => {}
    }
    if parse_theme_str(text).is_ok() {
        return Some(ImportFormat::Native);
    }
    match sniff_foreign_format(text) {
        Some(ForeignFormat::Kitty) => Some(ImportFormat::Kitty),
        Some(ForeignFormat::Ghostty) => Some(ImportFormat::Ghostty),
        Some(ForeignFormat::Plist) => Some(ImportFormat::ITerm),
        Some(ForeignFormat::TerminalApp) => Some(ImportFormat::TerminalApp),
        Some(ForeignFormat::Toml) => Some(ImportFormat::Alacritty),
        Some(ForeignFormat::WezTerm) => Some(ImportFormat::WezTerm),
        Some(ForeignFormat::Json) => Some(ImportFormat::WindowsTerminal),
        None if looks_like_base16(text) => Some(ImportFormat::Base16),
        None => None,
    }
}

/// Convert `text` using the given `format`.
/// # Errors
/// Returns [`ImportError`] if `text` is too large, malformed, or missing required colours.
pub fn convert(
    format: ImportFormat,
    text: &str,
    pick: Option<&str>,
) -> Result<ImportReport, ImportError> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err(ImportError::TooLarge { bytes: text.len() });
    }
    let text = strip_bom(text);
    match format {
        ImportFormat::Native => convert_native(text),
        ImportFormat::Kitty => convert_kitty(text),
        ImportFormat::Ghostty => convert_ghostty(text),
        ImportFormat::Alacritty => convert_toml(ImportFormat::Alacritty, text, alacritty_assign),
        ImportFormat::WezTerm => convert_toml(ImportFormat::WezTerm, text, wezterm_assign),
        ImportFormat::ITerm => convert_iterm(text),
        ImportFormat::TerminalApp => convert_terminal_app(text),
        ImportFormat::WindowsTerminal => convert_windows_terminal(text, pick),
        ImportFormat::Base16 => convert_base16(text),
    }
}

/// Serialize a [`ColorScheme`] as an `aterm` native theme file.
#[must_use]
pub fn to_native_conf(
    scheme: &ColorScheme,
    appearance_declared: bool,
    description: Option<&str>,
    author: Option<&str>,
    header: &[&str],
) -> String {
    let theme = ParsedTheme::colours_only(
        scheme.clone(),
        appearance_declared,
        description.map(str::to_string),
        author.map(str::to_string),
    );
    write_native_conf(&theme, header, false)
}

/// Serialize a whole [`ParsedTheme`] as an `aterm` native theme file.
#[must_use]
pub fn to_native_conf_package(theme: &ParsedTheme, header: &[&str]) -> String {
    write_native_conf(theme, header, false)
}

/// Serialize a whole [`ParsedTheme`] with descriptive comments and commented-out examples.
#[must_use]
pub fn to_native_conf_package_commented(theme: &ParsedTheme, header: &[&str]) -> String {
    write_native_conf(theme, header, true)
}

/// Serialize a [`ColorScheme`] with descriptive comments and commented-out examples.
#[must_use]
pub fn to_native_conf_commented(
    scheme: &ColorScheme,
    appearance_declared: bool,
    description: Option<&str>,
    author: Option<&str>,
    header: &[&str],
) -> String {
    let theme = ParsedTheme::colours_only(
        scheme.clone(),
        appearance_declared,
        description.map(str::to_string),
        author.map(str::to_string),
    );
    write_native_conf(&theme, header, true)
}

const ANSI_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

fn write_native_conf(theme: &ParsedTheme, header: &[&str], commented: bool) -> String {
    let scheme = &theme.scheme;
    let appearance_declared = theme.appearance_declared;
    let description = theme.description.as_deref();
    let author = theme.author.as_deref();
    let typography = &theme.typography;
    let mut out = String::new();
    for line in header {
        out.push('#');
        let line: String = line
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        if !line.trim().is_empty() {
            out.push(' ');
            out.push_str(line.trim_end());
        }
        out.push('\n');
    }
    let hex = |colour: Rgb| {
        let mut text = String::with_capacity(7);
        push_hex_colour(&mut text, colour);
        text
    };
    let mut chrome_header = false;
    let mut type_header = false;
    for spec in &THEME_KEYS {
        if spec.group == ThemeKeyGroup::Typography
            && !type_header
            && (commented || !typography.is_empty())
        {
            type_header = true;
            if !commented {
                out.push_str("# Typography: aterm.toml keys of the same name win\n");
            }
        }
        if commented {
            if !out.is_empty() {
                out.push('\n');
            }
            if spec.group == ThemeKeyGroup::Chrome && !chrome_header {
                chrome_header = true;
                out.push_str(
                    "# Chrome (optional): colours for aterm's own tabs, seams and highlights.\n\n",
                );
            }
            if spec.group == ThemeKeyGroup::Typography
                && type_header
                && spec.key == ThemeKey::FontFamily
            {
                out.push_str(
                    "# Typography (optional): fonts by family name (never a file or a path) and type\n\
                     # settings. aterm.toml keys of the same name win; a font not installed falls back\n\
                     # to your font_family, else aterm's default.\n\n",
                );
            }
            out.push_str("# ");
            out.push_str(spec.description);
            out.push_str("\n# ");
            out.push_str(if spec.required {
                "Required."
            } else {
                "Optional; when omitted, "
            });
            if !spec.required {
                out.push_str(spec.when_omitted);
                out.push('.');
            }
            out.push('\n');
        }
        if spec.key == ThemeKey::Palette {
            for (index, colour) in (0u8..).zip(scheme.ansi.iter()) {
                out.push_str(&spec.spelling(Some(index)));
                out.push_str(" = ");
                out.push_str(&hex(*colour));
                if commented {
                    let name = ANSI_NAMES[usize::from(index % 8)];
                    out.push_str(if index < 8 { "  # " } else { "  # bright " });
                    out.push_str(name);
                }
                out.push('\n');
            }
            continue;
        }
        let value = match spec.key {
            ThemeKey::Name => sanitize_text(&scheme.name),
            ThemeKey::Description => description.and_then(sanitize_text),
            ThemeKey::Author => author.and_then(sanitize_text),
            ThemeKey::Appearance => appearance_declared.then(|| {
                match scheme.appearance {
                    Appearance::Dark => APPEARANCE_VALUES[0],
                    Appearance::Light => APPEARANCE_VALUES[1],
                }
                .to_string()
            }),
            ThemeKey::Foreground => Some(hex(scheme.foreground)),
            ThemeKey::Background => Some(hex(scheme.background)),
            ThemeKey::Cursor => scheme.cursor.map(hex),
            ThemeKey::Selection => scheme.selection.map(hex),
            ThemeKey::SelectionForeground => scheme.selection_fg.map(hex),
            ThemeKey::Palette => None,
            ThemeKey::AccentColor
            | ThemeKey::ActiveTabColor
            | ThemeKey::BorderColor
            | ThemeKey::SearchMatchColor => scheme.chrome.slot(spec.key).flatten().map(hex),
            ThemeKey::Schema => theme.schema.map(|n| n.to_string()),
            ThemeKey::PaletteGenerate => scheme.palette_generate.then(|| "true".to_string()),
            ThemeKey::BoldIsBright => theme
                .bold_is_bright
                .map(|b| if b { "true" } else { "false" }.to_string()),
            ThemeKey::FontFamily
            | ThemeKey::FontFamilyBold
            | ThemeKey::FontFamilyItalic
            | ThemeKey::FontFamilyBoldItalic
            | ThemeKey::FallbackFonts
            | ThemeKey::SymbolFont
            | ThemeKey::EmojiFont
            | ThemeKey::LineHeight
            | ThemeKey::Ligatures
            | ThemeKey::FontFeatures
            | ThemeKey::FontSyntheticStyle
            | ThemeKey::FontWeight
            | ThemeKey::FontSizeScale => typography.value_text(spec.key),
        };
        match value {
            Some(value) => {
                out.push_str(spec.name);
                out.push_str(" = ");
                out.push_str(&value);
                out.push('\n');
            }
            None if commented => {
                out.push_str("# ");
                out.push_str(spec.name);
                out.push_str(" = ");
                out.push_str(spec.example);
                out.push('\n');
            }
            None => {}
        }
    }
    out
}

fn push_hex_colour(out: &mut String, colour: Rgb) {
    out.push('#');
    for byte in [colour.r, colour.g, colour.b] {
        for nibble in [byte >> 4, byte & 0xF] {
            out.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
}

/// Sanitize `text` for use as a native `name`, `description`, or `author` value.
#[must_use]
pub fn sanitize_text(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut count = 0usize;
    for ch in text.chars() {
        if count >= MAX_THEME_TEXT_CHARS {
            break;
        }
        let ch = if ch.is_control() { ' ' } else { ch };
        if ch == '#' || ch == ';' {
            while out.ends_with(char::is_whitespace) {
                out.pop();
                count = count.saturating_sub(1);
            }
        }
        out.push(ch);
        count = count.saturating_add(1);
    }
    let trimmed = out.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// `text` without a leading byte-order mark.
#[inline]
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn quoted(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_QUOTED_CHARS)
        .collect()
}

fn cat(parts: &[&str]) -> String {
    let mut out = String::new();
    for part in parts {
        out.push_str(part);
    }
    out
}

fn parse_foreign_colour(value: &str) -> Option<Rgb> {
    let value = unquote(value.trim());
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return if hex.len() == 6 { parse_hex(hex) } else { None };
    }
    if value.len() == 3 {
        return parse_hex(&format!("#{value}"));
    }
    parse_hex(value)
}

/// `value` without one pair of matching surrounding `"` or `'` quotes.
#[inline]
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

fn indexed_key(key: &str, prefix: &str) -> Option<usize> {
    let digits = key.strip_prefix(prefix)?;
    if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

const INCLUDE_NOTE: &str = "include and import lines are not followed: import the included file \
                            on its own if it holds colours";
const PALETTE_NOTE: &str =
    "palette entries above 15 are dropped: an aterm theme sets the 16 ANSI colours";

/// Where a converted colour goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Foreground,
    Background,
    Cursor,
    Selection,
    SelectionForeground,
    Ansi(usize),
    /// A chrome key ([`ThemeKey::AccentColor`] and its kin).
    Chrome(ThemeKey),
}

/// The colour names of the 8 ANSI slots, as Alacritty and Windows Terminal
/// spell them (`magenta` / `purple` both name slot 5).
#[inline]
fn ansi_name_index(name: &str) -> Option<usize> {
    Some(match name {
        "black" => 0,
        "red" => 1,
        "green" => 2,
        "yellow" => 3,
        "blue" => 4,
        "magenta" | "purple" => 5,
        "cyan" => 6,
        "white" => 7,
        _ => return None,
    })
}

struct Draft {
    format: ImportFormat,
    name: Option<String>,
    description: Option<String>,
    author: Option<String>,
    appearance: Option<Appearance>,
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    cursor: Option<Rgb>,
    selection: Option<Rgb>,
    selection_fg: Option<Rgb>,
    ansi: [Option<Rgb>; 16],
    chrome: ChromeColors,
    palette_generate: bool,
    dropped: Vec<String>,
    dropped_unlisted: usize,
    notes: Vec<String>,
}

impl Draft {
    fn new(format: ImportFormat) -> Self {
        Self {
            format,
            name: None,
            description: None,
            author: None,
            appearance: None,
            foreground: None,
            background: None,
            cursor: None,
            selection: None,
            selection_fg: None,
            ansi: [None; 16],
            chrome: ChromeColors::default(),
            palette_generate: false,
            dropped: Vec::new(),
            dropped_unlisted: 0,
            notes: Vec::new(),
        }
    }

    fn set(&mut self, slot: Slot, colour: Rgb) {
        let target = match slot {
            Slot::Foreground => &mut self.foreground,
            Slot::Background => &mut self.background,
            Slot::Cursor => &mut self.cursor,
            Slot::Selection => &mut self.selection,
            Slot::SelectionForeground => &mut self.selection_fg,
            Slot::Ansi(index) => match self.ansi.get_mut(index) {
                Some(target) => target,
                None => return,
            },
            Slot::Chrome(key) => match self.chrome.slot_mut(key) {
                Some(target) => target,
                None => return,
            },
        };
        *target = Some(colour);
    }

    fn colour(&mut self, slot: Slot, key: &str, value: &str) {
        match parse_foreign_colour(value) {
            Some(colour) => self.set(slot, colour),
            None => {
                self.drop_key(key);
                self.note(cat(&[
                    "\"",
                    &quoted(key),
                    "\" is not a hex colour (\"",
                    &quoted(value.trim()),
                    "\"), so it was dropped",
                ]));
            }
        }
    }

    fn flag_generate(&mut self, key: &str, value: &str) {
        match crate::scheme_typography::parse_flag(value) {
            Ok(flag) => self.palette_generate = flag,
            Err(_) => self.drop_key(key),
        }
    }

    fn drop_key(&mut self, key: &str) {
        let key = quoted(key.trim());
        if key.is_empty() || self.dropped.contains(&key) {
            return;
        }
        if self.dropped.len() >= MAX_DROPPED {
            self.dropped_unlisted = self.dropped_unlisted.saturating_add(1);
            return;
        }
        self.dropped.push(key);
    }

    fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        if self.notes.len() < MAX_NOTES && !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    fn finish(mut self) -> Result<ImportReport, ImportError> {
        let format = self.format;
        let foreground = self.foreground.ok_or(ImportError::MissingColour {
            format,
            key: "foreground",
        })?;
        let background = self.background.ok_or(ImportError::MissingColour {
            format,
            key: "background",
        })?;
        let missing: Vec<String> = self
            .ansi
            .iter()
            .enumerate()
            .filter(|(_, colour)| colour.is_none())
            .map(|(index, _)| index.to_string())
            .collect();
        if !missing.is_empty() {
            let (noun, rest) = if missing.len() == 1 {
                (
                    "ANSI colour ",
                    " is not in the file: aterm's default fills it",
                )
            } else {
                (
                    "ANSI colours ",
                    " are not in the file: aterm's defaults fill them",
                )
            };
            let note = cat(&[noun, &missing.join(", "), rest]);
            self.note(note);
        }
        if self.dropped_unlisted > 0 {
            let count = self.dropped_unlisted.to_string();
            self.note(cat(&["and ", &count, " more dropped keys not listed"]));
        }
        let mut ansi = ColorScheme::default().ansi;
        for (target, colour) in ansi.iter_mut().zip(self.ansi) {
            if let Some(colour) = colour {
                *target = colour;
            }
        }
        Ok(ImportReport {
            format,
            scheme: ColorScheme {
                name: self
                    .name
                    .as_deref()
                    .and_then(sanitize_text)
                    .unwrap_or_default(),
                appearance: self.appearance.unwrap_or_default(),
                foreground,
                background,
                cursor: self.cursor,
                selection: self.selection,
                selection_fg: self.selection_fg,
                ansi,
                chrome: self.chrome,
                palette_generate: self.palette_generate,
            },
            appearance_declared: self.appearance.is_some(),
            description: self.description.as_deref().and_then(sanitize_text),
            author: self.author.as_deref().and_then(sanitize_text),
            dropped: self.dropped,
            notes: self.notes,
        })
    }
}

fn malformed(format: ImportFormat, line: usize, reason: &str) -> ImportError {
    ImportError::Malformed {
        format,
        line,
        reason: reason.to_string(),
    }
}

fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()
        .iter()
        .take(offset)
        .filter(|&&b| b == b'\n')
        .count()
        .saturating_add(1)
}

fn convert_native(text: &str) -> Result<ImportReport, ImportError> {
    match parse_theme_str(text) {
        Ok(parsed) => {
            let mut dropped: Vec<String> = Vec::new();
            if parsed.schema.is_some() {
                dropped.push("schema".to_string());
            }
            if parsed.bold_is_bright.is_some() {
                dropped.push("bold_is_bright".to_string());
            }
            dropped.extend(parsed.typography.keys().into_iter().map(str::to_string));
            dropped.extend(parsed.skipped_keys.iter().cloned());
            let mut notes = Vec::new();
            if !dropped.is_empty() {
                notes.push(
                    "an import keeps colours only; copy the file into the themes folder to keep its other settings"
                        .to_string(),
                );
            }
            Ok(ImportReport {
                format: ImportFormat::Native,
                scheme: parsed.scheme,
                appearance_declared: parsed.appearance_declared,
                description: parsed.description,
                author: parsed.author,
                dropped,
                notes,
            })
        }
        Err(crate::scheme::ThemeError::Parse { line, reason, .. }) => Err(ImportError::Malformed {
            format: ImportFormat::Native,
            line,
            reason,
        }),
    }
}

fn convert_kitty(text: &str) -> Result<ImportReport, ImportError> {
    let mut draft = Draft::new(ImportFormat::Kitty);
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(meta) = line.strip_prefix("##") {
            if let Some((key, value)) = meta.split_once(':') {
                let value = value.trim();
                match key.trim().to_ascii_lowercase().as_str() {
                    "name" => draft.name = Some(value.to_string()),
                    "author" => draft.author = Some(value.to_string()),
                    "blurb" => draft.description = Some(value.to_string()),
                    _ => {}
                }
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(char::is_whitespace) {
            Some((key, value)) => (key, value.trim()),
            None => (line, ""),
        };
        let lower = key.to_ascii_lowercase();
        let none = value.eq_ignore_ascii_case("none");
        match lower.as_str() {
            "foreground" => draft.colour(Slot::Foreground, key, value),
            "background" => draft.colour(Slot::Background, key, value),
            "cursor" if none => {
                draft.drop_key(key);
                draft.note(
                    "kitty's `cursor none` (a reverse-video cursor) has no aterm equivalent: \
                     the cursor follows the text colour",
                );
            }
            "cursor" => draft.colour(Slot::Cursor, key, value),
            "selection_background" if none => {
                draft.drop_key(key);
                draft.note(
                    "kitty's `selection_background none` (reverse video) has no aterm \
                     equivalent: aterm's selection colour is used",
                );
            }
            "selection_background" => draft.colour(Slot::Selection, key, value),
            "selection_foreground" if none => {}
            "selection_foreground" => draft.colour(Slot::SelectionForeground, key, value),
            "active_tab_background" => {
                draft.colour(Slot::Chrome(ThemeKey::ActiveTabColor), key, value);
            }
            "include" | "globinclude" | "envinclude" | "geninclude" => {
                draft.drop_key(key);
                draft.note(INCLUDE_NOTE);
            }
            _ => match indexed_key(&lower, "color") {
                Some(index) if index < 16 => draft.colour(Slot::Ansi(index), key, value),
                Some(_) => {
                    draft.drop_key(key);
                    draft.note(PALETTE_NOTE);
                }
                None => draft.drop_key(key),
            },
        }
    }
    draft.finish()
}

fn convert_ghostty(text: &str) -> Result<ImportReport, ImportError> {
    let mut draft = Draft::new(ImportFormat::Ghostty);
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            draft.drop_key(line);
            continue;
        };
        let key = key.trim();
        let value = unquote(value.trim());
        match key.to_ascii_lowercase().as_str() {
            "foreground" => draft.colour(Slot::Foreground, key, value),
            "background" => draft.colour(Slot::Background, key, value),
            "cursor-color" => draft.colour(Slot::Cursor, key, value),
            "selection-background" => draft.colour(Slot::Selection, key, value),
            "selection-foreground" => draft.colour(Slot::SelectionForeground, key, value),
            "split-divider-color" => draft.colour(Slot::Chrome(ThemeKey::BorderColor), key, value),
            "palette-generate" => draft.flag_generate(key, value),
            "palette" => {
                let Some((index, colour)) = value.split_once('=') else {
                    draft.drop_key(key);
                    continue;
                };
                let index = index.trim();
                let entry = cat(&["palette ", index]);
                match indexed_key(index, "") {
                    Some(index) if index < 16 => draft.colour(Slot::Ansi(index), &entry, colour),
                    Some(_) => {
                        draft.drop_key(&entry);
                        draft.note(PALETTE_NOTE);
                    }
                    None => draft.drop_key(&entry),
                }
            }
            "config-file" => {
                draft.drop_key(key);
                draft.note(INCLUDE_NOTE);
            }
            _ => draft.drop_key(key),
        }
    }
    draft.finish()
}

fn toml_comment_start(line: &str) -> usize {
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        match quote {
            Some(b'"') if escaped => escaped = false,
            Some(b'"') if byte == b'\\' => escaped = true,
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'#' => return index,
            None => {}
        }
    }
    line.len()
}

fn toml_split(s: &str, separator: u8) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, byte) in s.bytes().enumerate() {
        match quote {
            Some(b'"') if escaped => escaped = false,
            Some(b'"') if byte == b'\\' => escaped = true,
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'[' | b'{' => depth = depth.saturating_add(1),
                b']' | b'}' => depth = depth.saturating_sub(1),
                _ if byte == separator && depth == 0 => {
                    parts.push(s.get(start..index)?);
                    start = index.saturating_add(1);
                }
                _ => {}
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    parts.push(s.get(start..)?);
    Some(parts)
}

fn toml_bracket_delta(line: &str) -> isize {
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut delta = 0isize;
    for byte in line.bytes() {
        match quote {
            Some(b'"') if escaped => escaped = false,
            Some(b'"') if byte == b'\\' => escaped = true,
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'[' | b'{' => delta = delta.saturating_add(1),
                b']' | b'}' => delta = delta.saturating_sub(1),
                _ => {}
            },
        }
    }
    delta
}

fn toml_key(raw: &str) -> Option<Vec<String>> {
    let parts = toml_split(raw, b'.')?;
    if parts.len() > MAX_NESTING {
        return None;
    }
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        let part = part.trim();
        if part.starts_with('"') || part.starts_with('\'') {
            out.push(toml_string(part)?);
        } else if !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            out.push(part.to_string());
        } else {
            return None;
        }
    }
    Some(out)
}

fn toml_string(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('\'') {
        let inner = inner.strip_suffix('\'')?;
        return (!inner.contains('\'')).then(|| inner.to_string());
    }
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return None,
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'e' => out.push('\u{1b}'),
                code @ ('u' | 'U') => {
                    let width = if code == 'u' { 4 } else { 8 };
                    let mut value = 0u32;
                    for _ in 0..width {
                        value = value
                            .checked_mul(16)?
                            .checked_add(chars.next()?.to_digit(16)?)?;
                    }
                    out.push(char::from_u32(value)?);
                }
                _ => return None,
            },
            _ => out.push(ch),
        }
    }
    Some(out)
}

/// Read the TOML `text` as `format`, handing `assign` each key's full path and its value's text:
/// an array whole (joined when it spans lines), an inline table entry by entry (a nested one's
/// too), and `None` for a multi-line string.
fn convert_toml(
    format: ImportFormat,
    text: &str,
    assign: fn(&mut Draft, &[String], Option<&str>),
) -> Result<ImportReport, ImportError> {
    let mut draft = Draft::new(format);
    let mut table: Vec<String> = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((index, raw)) = lines.next() {
        let lineno = index.saturating_add(1);
        let line = raw.get(..toml_comment_start(raw)).unwrap_or(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let inner = line
                .strip_prefix("[[")
                .and_then(|s| s.strip_suffix("]]"))
                .or_else(|| line.strip_prefix('[').and_then(|s| s.strip_suffix(']')))
                .ok_or_else(|| malformed(format, lineno, "unterminated table header"))?;
            table = toml_key(inner).ok_or_else(|| malformed(format, lineno, "bad table name"))?;
            continue;
        }
        let parts = toml_split(line, b'=')
            .ok_or_else(|| malformed(format, lineno, "unterminated string"))?;
        let (Some(key), Some(_)) = (parts.first(), parts.get(1)) else {
            return Err(malformed(format, lineno, "expected `key = value`"));
        };
        let value = line.get(key.len().saturating_add(1)..).unwrap_or("").trim();
        let mut path = table.clone();
        path.extend(toml_key(key).ok_or_else(|| malformed(format, lineno, "bad key"))?);
        if path.len() > MAX_NESTING {
            return Err(malformed(format, lineno, "keys nest too deeply"));
        }
        if value.starts_with("\"\"\"") || value.starts_with("'''") {
            let fence = value.get(..3).unwrap_or("\"\"\"");
            if value.len() < 6 || !value.get(3..).unwrap_or("").contains(fence) {
                loop {
                    let Some((_, next)) = lines.next() else {
                        return Err(malformed(format, lineno, "unterminated multi-line string"));
                    };
                    if next.contains(fence) {
                        break;
                    }
                }
            }
            assign(&mut draft, &path, None);
            continue;
        }
        let mut depth = toml_bracket_delta(value);
        let mut joined = String::new();
        if depth > 0 {
            joined.push_str(value);
        }
        while depth > 0 {
            let Some((_, next)) = lines.next() else {
                return Err(malformed(format, lineno, "unterminated array"));
            };
            let next = next.get(..toml_comment_start(next)).unwrap_or(next);
            depth = depth.saturating_add(toml_bracket_delta(next));
            joined.push(' ');
            joined.push_str(next.trim());
        }
        let value = if joined.is_empty() { value } else { &joined };
        toml_assign(&mut draft, assign, &path, value, lineno)?;
    }
    draft.finish()
}

/// Hand `assign` the value `value` of the key at `path` (on line `lineno`), or, when it is an
/// inline table, each of its entries at their full paths, nested tables included.
fn toml_assign(
    draft: &mut Draft,
    assign: fn(&mut Draft, &[String], Option<&str>),
    path: &[String],
    value: &str,
    lineno: usize,
) -> Result<(), ImportError> {
    let format = draft.format;
    let Some(inner) = value
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        assign(draft, path, Some(value));
        return Ok(());
    };
    let pairs = toml_split(inner, b',').ok_or_else(|| malformed(format, lineno, "bad table"))?;
    for pair in pairs.into_iter().filter(|p| !p.trim().is_empty()) {
        let (sub_key, sub_value) = pair
            .split_once('=')
            .ok_or_else(|| malformed(format, lineno, "expected `key = value`"))?;
        let mut sub_path = path.to_vec();
        sub_path.extend(toml_key(sub_key).ok_or_else(|| malformed(format, lineno, "bad key"))?);
        if sub_path.len() > MAX_NESTING {
            return Err(malformed(format, lineno, "keys nest too deeply"));
        }
        toml_assign(draft, assign, &sub_path, sub_value.trim(), lineno)?;
    }
    Ok(())
}

fn alacritty_assign(draft: &mut Draft, path: &[String], value: Option<&str>) {
    let dotted = path.join(".");
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    let slot = match parts.as_slice() {
        ["colors", "primary", "foreground"] => Slot::Foreground,
        ["colors", "primary", "background"] => Slot::Background,
        ["colors", "cursor", "cursor"] => Slot::Cursor,
        ["colors", "selection", "background"] => Slot::Selection,
        ["colors", "selection", "text"] => Slot::SelectionForeground,
        ["colors", "search", "matches", "background"] => Slot::Chrome(ThemeKey::SearchMatchColor),
        ["colors", group @ ("normal" | "bright"), name] => match ansi_name_index(name) {
            Some(index) if *group == "normal" => Slot::Ansi(index),
            Some(index) => Slot::Ansi(index.saturating_add(8)),
            None => return draft.drop_key(&dotted),
        },
        ["import"] | ["general", "import"] => {
            draft.drop_key(&dotted);
            return draft.note(INCLUDE_NOTE);
        }
        _ => return draft.drop_key(&dotted),
    };
    let Some(value) = value.and_then(toml_string) else {
        return draft.drop_key(&dotted);
    };
    match value.as_str() {
        "CellForeground" if slot == Slot::SelectionForeground => {}
        "CellForeground" | "CellBackground" => {
            draft.drop_key(&dotted);
            draft.note(
                "Alacritty's CellForeground / CellBackground (the cell's own colour) has no \
                 aterm theme key: aterm's default is used",
            );
        }
        _ => draft.colour(slot, &dotted, &value),
    }
}

fn wezterm_assign(draft: &mut Draft, path: &[String], value: Option<&str>) {
    let dotted = path.join(".");
    let parts: Vec<&str> = path.iter().map(String::as_str).collect();
    let slot = match parts.as_slice() {
        ["colors", "foreground"] => Slot::Foreground,
        ["colors", "background"] => Slot::Background,
        ["colors", "cursor_bg"] => Slot::Cursor,
        ["colors", "selection_bg"] => Slot::Selection,
        ["colors", "selection_fg"] => Slot::SelectionForeground,
        ["colors", "split"] => Slot::Chrome(ThemeKey::BorderColor),
        ["colors", "tab_bar", "active_tab", "bg_color"] => Slot::Chrome(ThemeKey::ActiveTabColor),
        ["colors", group @ ("ansi" | "brights")] => {
            let offset = if *group == "ansi" { 0 } else { 8 };
            return wezterm_palette(draft, &dotted, offset, value);
        }
        ["colors", "indexed", index] => match indexed_key(index, "") {
            Some(index) if index < 16 => Slot::Ansi(index),
            Some(_) => {
                draft.drop_key(&dotted);
                return draft.note(PALETTE_NOTE);
            }
            None => return draft.drop_key(&dotted),
        },
        ["metadata", field @ ("name" | "author")] => {
            match value.and_then(toml_string) {
                Some(text) if *field == "name" => draft.name = Some(text),
                Some(text) => draft.author = Some(text),
                None => draft.drop_key(&dotted),
            }
            return;
        }
        _ => return draft.drop_key(&dotted),
    };
    match value.and_then(toml_string) {
        Some(value) => draft.colour(slot, &dotted, &value),
        None => draft.drop_key(&dotted),
    }
}

/// WezTerm's `ansi` or `brights` array (`dotted`): its 8 colours fill the ANSI slots from
/// `offset`; an entry past the 8th, or one that is not a string, is dropped as `dotted[N]`.
fn wezterm_palette(draft: &mut Draft, dotted: &str, offset: usize, value: Option<&str>) {
    let entries = value
        .and_then(|value| value.strip_prefix('[')?.strip_suffix(']'))
        .and_then(|inner| toml_split(inner, b','));
    let Some(entries) = entries else {
        return draft.drop_key(dotted);
    };
    let entries = entries.into_iter().filter(|entry| !entry.trim().is_empty());
    for (index, entry) in entries.enumerate() {
        let key = cat(&[dotted, "[", &index.to_string(), "]"]);
        match toml_string(entry) {
            Some(colour) if index < 8 => {
                draft.colour(Slot::Ansi(offset.saturating_add(index)), &key, &colour);
            }
            _ => draft.drop_key(&key),
        }
    }
}

/// A property-list value, reduced to what a colour theme needs.
#[derive(Debug, Clone, PartialEq)]
enum PlistValue {
    Dict(Vec<(String, PlistValue)>),
    Number(f64),
    Text(String),
    /// A `<data>` element's base64 text, undecoded.
    Data(String),
    Other,
}

struct PlistScanner<'a> {
    text: &'a str,
    pos: usize,
    /// The format a malformed document is reported as.
    format: ImportFormat,
}

/// What kind of tag a scanned tag is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    Open,
    Close,
    Empty,
}

impl<'a> PlistScanner<'a> {
    fn rest(&self) -> &'a str {
        self.text.get(self.pos..).unwrap_or("")
    }

    fn error(&self, reason: &str) -> ImportError {
        malformed(self.format, line_of(self.text, self.pos), reason)
    }

    fn skip_misc(&mut self) -> Result<(), ImportError> {
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.pos = self
                .pos
                .saturating_add(rest.len().saturating_sub(trimmed.len()));
            let (open, close) = if trimmed.starts_with("<?") {
                ("<?", "?>")
            } else if trimmed.starts_with("<!--") {
                ("<!--", "-->")
            } else if trimmed.starts_with("<!") {
                ("<!", ">")
            } else {
                return Ok(());
            };
            let end = trimmed
                .get(open.len()..)
                .and_then(|after| after.find(close))
                .ok_or_else(|| self.error("unterminated markup"))?;
            self.pos = self
                .pos
                .saturating_add(open.len())
                .saturating_add(end)
                .saturating_add(close.len());
        }
    }

    fn tag(&mut self) -> Result<(&'a str, TagKind), ImportError> {
        self.skip_misc()?;
        let rest = self.rest();
        if !rest.starts_with('<') {
            return Err(self.error("expected a tag"));
        }
        let end = rest
            .find('>')
            .ok_or_else(|| self.error("unterminated tag"))?;
        let inner = rest.get(1..end).unwrap_or("");
        self.pos = self.pos.saturating_add(end).saturating_add(1);
        let (kind, inner) = if let Some(name) = inner.strip_prefix('/') {
            (TagKind::Close, name)
        } else if let Some(name) = inner.strip_suffix('/') {
            (TagKind::Empty, name)
        } else {
            (TagKind::Open, inner)
        };
        let name = inner.split_whitespace().next().unwrap_or("");
        Ok((name, kind))
    }

    fn text_until_close(&mut self, name: &str) -> Result<String, ImportError> {
        let rest = self.rest();
        let end = rest
            .find('<')
            .ok_or_else(|| self.error("unterminated element"))?;
        let text = rest.get(..end).unwrap_or("");
        self.pos = self.pos.saturating_add(end);
        let (close, kind) = self.tag()?;
        if kind != TagKind::Close || close != name {
            return Err(self.error("unexpected tag inside a text element"));
        }
        Ok(decode_entities(text))
    }

    fn value(&mut self, depth: usize) -> Result<PlistValue, ImportError> {
        if depth > MAX_NESTING {
            return Err(self.error("the property list nests too deeply"));
        }
        let (name, kind) = self.tag()?;
        match (name, kind) {
            ("dict" | "array", TagKind::Empty) => Ok(if name == "dict" {
                PlistValue::Dict(Vec::new())
            } else {
                PlistValue::Other
            }),
            ("dict", TagKind::Open) => {
                let mut entries = Vec::new();
                loop {
                    let (name, kind) = self.tag()?;
                    match (name, kind) {
                        ("dict", TagKind::Close) => return Ok(PlistValue::Dict(entries)),
                        ("key", TagKind::Open) => {
                            let key = self.text_until_close("key")?;
                            let value = self.value(depth.saturating_add(1))?;
                            entries.push((key, value));
                        }
                        _ => return Err(self.error("expected <key> in <dict>")),
                    }
                }
            }
            ("array", TagKind::Open) => loop {
                self.skip_misc()?;
                if self.rest().starts_with("</") {
                    let (name, _) = self.tag()?;
                    if name != "array" {
                        return Err(self.error("mismatched </array>"));
                    }
                    return Ok(PlistValue::Other);
                }
                self.value(depth.saturating_add(1))?;
            },
            ("real" | "integer", TagKind::Open) => {
                let text = self.text_until_close(name)?;
                let trimmed = text.trim();
                Ok(if trimmed.starts_with('+') {
                    PlistValue::Other
                } else {
                    trimmed
                        .parse::<f64>()
                        .map_or(PlistValue::Other, PlistValue::Number)
                })
            }
            ("string", TagKind::Open) => Ok(PlistValue::Text(self.text_until_close("string")?)),
            ("string", TagKind::Empty) => Ok(PlistValue::Text(String::new())),
            ("data", TagKind::Open) => Ok(PlistValue::Data(self.text_until_close("data")?)),
            ("date", TagKind::Open) => {
                self.text_until_close(name)?;
                Ok(PlistValue::Other)
            }
            ("true" | "false", TagKind::Empty) => Ok(PlistValue::Other),
            _ => Err(self.error("unexpected element in the property list")),
        }
    }

    fn document(&mut self) -> Result<PlistValue, ImportError> {
        self.skip_misc()?;
        let wrapped = self.rest().starts_with("<plist");
        if wrapped {
            let (_, kind) = self.tag()?;
            if kind != TagKind::Open {
                return Err(self.error("empty <plist>"));
            }
        }
        let value = self.value(1)?;
        if wrapped {
            let (name, kind) = self.tag()?;
            if name != "plist" || kind != TagKind::Close {
                return Err(self.error("expected </plist>"));
            }
        }
        Ok(value)
    }
}

fn decode_entities(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or(""));
        let after = rest.get(amp..).unwrap_or("");
        let Some(semi) = after.find(';').filter(|&semi| semi <= 10) else {
            out.push('&');
            rest = after.get(1..).unwrap_or("");
            continue;
        };
        let entity = after.get(1..semi).unwrap_or("");
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .filter(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .filter(|dec| !dec.is_empty() && dec.bytes().all(|b| b.is_ascii_digit()))
                        .map(str::parse::<u32>)
                })
                .and_then(Result::ok)
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = after.get(semi.saturating_add(1)..).unwrap_or("");
            }
            None => {
                out.push('&');
                rest = after.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    out
}

fn iterm_slot(key: &str) -> Option<Slot> {
    Some(match key {
        "Foreground Color" => Slot::Foreground,
        "Background Color" => Slot::Background,
        "Cursor Color" => Slot::Cursor,
        "Selection Color" => Slot::Selection,
        "Selected Text Color" => Slot::SelectionForeground,
        _ => {
            let index = key.strip_prefix("Ansi ")?.strip_suffix(" Color")?;
            match indexed_key(index, "") {
                Some(index) if index < 16 => Slot::Ansi(index),
                _ => return None,
            }
        }
    })
}

/// A colour component from 0 to 1 (clamped) as a byte.
fn unit_byte(component: f64) -> u8 {
    (component.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn iterm_colour(entries: &[(String, PlistValue)]) -> Option<(Rgb, Option<String>)> {
    let component = |name: &str| {
        entries.iter().find_map(|(key, value)| match value {
            PlistValue::Number(n) if key == name && n.is_finite() => Some(unit_byte(*n)),
            _ => None,
        })
    };
    let rgb = Rgb::new(
        component("Red Component")?,
        component("Green Component")?,
        component("Blue Component")?,
    );
    let space = entries.iter().find_map(|(key, value)| match value {
        PlistValue::Text(space) if key == "Color Space" => Some(space.clone()),
        _ => None,
    });
    Some((
        rgb,
        space.filter(|space| !space.eq_ignore_ascii_case("srgb")),
    ))
}

fn convert_iterm(text: &str) -> Result<ImportReport, ImportError> {
    let format = ImportFormat::ITerm;
    let mut scanner = PlistScanner {
        text,
        pos: 0,
        format,
    };
    let PlistValue::Dict(entries) = scanner.document()? else {
        return Err(malformed(
            format,
            0,
            "the property list is not a dictionary",
        ));
    };
    let mut draft = Draft::new(format);
    let plain: Vec<Slot> = entries
        .iter()
        .filter_map(|(key, _)| iterm_slot(key))
        .collect();
    for (key, value) in &entries {
        let (slot, variant) = if let Some(slot) = iterm_slot(key) {
            (slot, false)
        } else if let Some(slot) = key
            .strip_suffix(" (Dark)")
            .and_then(iterm_slot)
            .filter(|slot| !plain.contains(slot))
        {
            (slot, true)
        } else {
            draft.drop_key(key);
            continue;
        };
        let colour = match value {
            PlistValue::Dict(colour) => iterm_colour(colour),
            _ => None,
        };
        let Some((rgb, space)) = colour else {
            draft.drop_key(key);
            draft.note(cat(&[
                "\"",
                &quoted(key),
                "\" is not a colour dictionary, so it was dropped",
            ]));
            continue;
        };
        draft.set(slot, rgb);
        if variant {
            draft.note("the file has only (Dark) variants of some colours; those were used");
        }
        if let Some(space) = space {
            draft.note(cat(&[
                "colours in the \"",
                &quoted(&space),
                "\" colour space were read as sRGB and may look slightly different",
            ]));
        }
    }
    draft.finish()
}

fn terminal_app_slot(key: &str) -> Option<Slot> {
    Some(match key {
        "TextColor" => Slot::Foreground,
        "BackgroundColor" => Slot::Background,
        "CursorColor" => Slot::Cursor,
        "SelectionColor" => Slot::Selection,
        _ => {
            let name = key.strip_prefix("ANSI")?.strip_suffix("Color")?;
            let (offset, name) = name
                .strip_prefix("Bright")
                .map_or((0, name), |name| (8, name));
            let index = ANSI_NAMES
                .iter()
                .position(|ansi| ansi.eq_ignore_ascii_case(name))?;
            Slot::Ansi(index.saturating_add(offset))
        }
    })
}

/// The colour space an archived colour's components were read in.
enum ArchivedSpace {
    Srgb,
    /// A space the archive names: a legacy `NSColorSpace` (`calibrated RGB`, …) or the
    /// description of a custom space's ICC profile (`Display P3`).
    Named(String),
    /// A custom space that is not sRGB and has no ICC description aterm reads.
    Unnamed,
}

/// The colour a Terminal.app `<data>` value archives, as read.
struct ArchivedColour {
    rgb: Rgb,
    space: ArchivedSpace,
    /// For a named colour (`NSColorSpace` 6, such as `textColor`): the name it gives itself
    /// when that is ASCII, else empty. `rgb` is then the archived fallback, not the live
    /// system colour an app that opens the archive shows.
    named: Option<String>,
    /// Whether a component lay far enough below 0 or above 1 that clipping it to that range
    /// changed its byte.
    clipped: bool,
}

/// An `NSKeyedArchiver` archive: a binary plist whose `$objects` array holds each object a
/// `Uid` names.
struct KeyedArchive<'a> {
    plist: Bplist<'a>,
    objects: Vec<usize>,
}

impl<'a> KeyedArchive<'a> {
    /// The archive `bytes` hold and its root object's dictionary.
    fn open(bytes: &'a [u8]) -> Option<(Self, Vec<(usize, usize)>)> {
        let plist = Bplist::new(bytes)?;
        let Bvalue::Dict(top) = plist.object(plist.top)? else {
            return None;
        };
        let Bvalue::Array(objects) = plist.lookup(&top, "$objects")? else {
            return None;
        };
        let Bvalue::Dict(roots) = plist.lookup(&top, "$top")? else {
            return None;
        };
        let archive = Self { plist, objects };
        let root = archive.dict(&roots, "root")?;
        Some((archive, root))
    }

    /// The plist object the `Uid` that `dict` holds under `key` names.
    fn target(&self, dict: &[(usize, usize)], key: &str) -> Option<usize> {
        let Bvalue::Uid(uid) = self.plist.lookup(dict, key)? else {
            return None;
        };
        self.objects.get(uid).copied()
    }

    /// The dictionary the `Uid` that `dict` holds under `key` names.
    fn dict(&self, dict: &[(usize, usize)], key: &str) -> Option<Vec<(usize, usize)>> {
        match self.plist.object(self.target(dict, key)?)? {
            Bvalue::Dict(entries) => Some(entries),
            _ => None,
        }
    }
}

/// The colour a Terminal.app `<data>` value (`base64`) archives: an `NSKeyedArchiver` binary
/// plist whose root is an RGB or grey `NSColor` (read by [`archived_components`]) or a named
/// colour whose fallback is one. Any other archive is `None`.
fn terminal_app_colour(base64: &str) -> Option<ArchivedColour> {
    let bytes = base64_decode(base64)?;
    let (archive, root) = KeyedArchive::open(&bytes)?;
    let (colour, named) = match archive.plist.lookup(&root, "NSColorSpace")? {
        Bvalue::Int(6) => {
            let name = archive
                .target(&root, "NSColorName")
                .and_then(|name| archive.plist.ascii(name))
                .and_then(|name| core::str::from_utf8(name).ok())
                .map(quoted)
                .unwrap_or_default();
            (archive.dict(&root, "NSColor")?, Some(name))
        }
        _ => (root, None),
    };
    Some(ArchivedColour {
        named,
        ..archived_components(&archive, &colour)?
    })
}

/// The colour an archived `NSColor` (`colour`) in an RGB or grey space holds, alpha ignored,
/// the space it was read in and whether it was clipped: its own components (`NSComponents`)
/// in the custom space (`NSCustomColorSpace`) it names when it has both, else its legacy
/// calibrated or device components (`NSRGB`, `NSWhite`). Its `named` is `None`.
fn archived_components(
    archive: &KeyedArchive<'_>,
    colour: &[(usize, usize)],
) -> Option<ArchivedColour> {
    let Bvalue::Int(space) = archive.plist.lookup(colour, "NSColorSpace")? else {
        return None;
    };
    let (rgb, legacy_key, legacy_space) = match space {
        1 => (true, "NSRGB", "calibrated RGB"),
        2 => (true, "NSRGB", "device RGB"),
        3 => (false, "NSWhite", "calibrated white"),
        4 => (false, "NSWhite", "device white"),
        _ => return None,
    };
    let own = archive
        .plist
        .lookup(colour, "NSComponents")
        .zip(archive.dict(colour, "NSCustomColorSpace"));
    let (components, space) = match own {
        Some((Bvalue::Data(components), custom)) => (components, custom_space(archive, &custom)),
        _ => {
            let Bvalue::Data(components) = archive.plist.lookup(colour, legacy_key)? else {
                return None;
            };
            (components, ArchivedSpace::Named(legacy_space.to_string()))
        }
    };
    let mut components = core::str::from_utf8(components)
        .ok()?
        .trim_end_matches('\0')
        .split_ascii_whitespace()
        .map(|component| {
            (!component.starts_with('+'))
                .then(|| component.parse::<f64>().ok())
                .flatten()
                .filter(|n| n.is_finite())
        });
    let mut next = || components.next().flatten();
    let read = if rgb {
        [next()?, next()?, next()?]
    } else {
        [next()?; 3]
    };
    let [red, green, blue] = read.map(unit_byte);
    Some(ArchivedColour {
        rgb: Rgb::new(red, green, blue),
        space,
        named: None,
        clipped: read
            .iter()
            .any(|&component| f64::from(unit_byte(component)) != (component * 255.0).round()),
    })
}

/// The space an archived `NSColorSpace` (`space`) is: sRGB when its `NSID` is 7 (sRGB) or its
/// `NSSpaceID` is 14 (extended sRGB, which is sRGB from 0 to 1 and goes on past it), else the
/// space its ICC profile (`NSICC`) describes.
fn custom_space(archive: &KeyedArchive<'_>, space: &[(usize, usize)]) -> ArchivedSpace {
    let srgb = matches!(archive.plist.lookup(space, "NSID"), Some(Bvalue::Int(7)))
        || matches!(
            archive.plist.lookup(space, "NSSpaceID"),
            Some(Bvalue::Int(14))
        );
    if srgb {
        return ArchivedSpace::Srgb;
    }
    let icc = archive
        .target(space, "NSICC")
        .and_then(|icc| archive.plist.object(icc));
    match icc {
        Some(Bvalue::Data(icc)) => {
            icc_description(icc).map_or(ArchivedSpace::Unnamed, ArchivedSpace::Named)
        }
        _ => ArchivedSpace::Unnamed,
    }
}

/// The description an ICC profile gives itself: its `desc` tag, as ICC v2 text or ICC v4
/// multi-localized Unicode (the first English record, else the first). `None` when the
/// profile has no such tag or it is damaged.
fn icc_description(icc: &[u8]) -> Option<String> {
    if icc.get(36..40)? != b"acsp" {
        return None;
    }
    let tags = be_usize(icc.get(128..132)?)?;
    let tag = icc
        .get(132..)?
        .as_chunks::<12>()
        .0
        .iter()
        .take(tags)
        .find(|entry| entry.starts_with(b"desc"))?;
    let start = be_usize(tag.get(4..8)?)?;
    let body = icc.get(start..start.checked_add(be_usize(tag.get(8..12)?)?)?)?;
    let text = match body.get(..4)? {
        b"desc" => {
            let len = be_usize(body.get(8..12)?)?;
            let ascii = body.get(12..len.checked_add(12)?)?;
            core::str::from_utf8(ascii.split(|&byte| byte == 0).next()?)
                .ok()?
                .to_string()
        }
        b"mluc" => {
            let count = be_usize(body.get(8..12)?)?;
            let size = be_usize(body.get(12..16)?)?;
            if size < 12 {
                return None;
            }
            let mut records = body.get(16..)?.chunks_exact(size).take(count);
            let record = records
                .clone()
                .find(|record| record.starts_with(b"en"))
                .or_else(|| records.next())?;
            let len = be_usize(record.get(4..8)?)?;
            let at = be_usize(record.get(8..12)?)?;
            let units = body
                .get(at..at.checked_add(len)?)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair));
            char::decode_utf16(units)
                .collect::<Result<String, _>>()
                .ok()?
        }
        _ => return None,
    };
    let text = quoted(text.trim());
    (!text.is_empty()).then_some(text)
}

/// The rest of the note on a named colour.
const TERMINAL_APP_NAMED_NOTE: &str = "Terminal.app shows the system's current colour for it, \
                                       which can differ (in Dark Mode, say); the fallback saved \
                                       with it was used";

fn convert_terminal_app(text: &str) -> Result<ImportReport, ImportError> {
    let format = ImportFormat::TerminalApp;
    let mut scanner = PlistScanner {
        text,
        pos: 0,
        format,
    };
    let PlistValue::Dict(entries) = scanner.document()? else {
        return Err(malformed(
            format,
            0,
            "the property list is not a dictionary",
        ));
    };
    let mut draft = Draft::new(format);
    for (key, value) in &entries {
        if let ("name", PlistValue::Text(name)) = (key.as_str(), value) {
            draft.name = Some(name.clone());
            continue;
        }
        let Some(slot) = terminal_app_slot(key) else {
            draft.drop_key(key);
            continue;
        };
        let colour = match value {
            PlistValue::Data(archive) => terminal_app_colour(archive),
            _ => None,
        };
        let Some(colour) = colour else {
            draft.drop_key(key);
            draft.note(cat(&[
                "\"",
                &quoted(key),
                "\" is not an archived RGB or grey colour, so it was dropped",
            ]));
            continue;
        };
        draft.set(slot, colour.rgb);
        match colour.named.as_deref() {
            Some("") => draft.note(cat(&[
                "\"",
                &quoted(key),
                "\" is a named colour: ",
                TERMINAL_APP_NAMED_NOTE,
            ])),
            Some(name) => draft.note(cat(&[
                "\"",
                &quoted(key),
                "\" is the named colour \"",
                name,
                "\": ",
                TERMINAL_APP_NAMED_NOTE,
            ])),
            None => {}
        }
        if colour.clipped {
            draft.note(cat(&[
                "\"",
                &quoted(key),
                "\" lies outside sRGB (a component is below 0 or above 1), so it was clipped \
                 to fit",
            ]));
        }
        match &colour.space {
            ArchivedSpace::Srgb => {}
            ArchivedSpace::Named(space) => draft.note(cat(&[
                "colours in the \"",
                space,
                "\" colour space were read as sRGB and may look slightly different",
            ])),
            ArchivedSpace::Unnamed => draft.note(
                "colours in a colour space aterm could not name were read as sRGB and may \
                 look slightly different",
            ),
        }
    }
    for (set, key) in [
        (draft.foreground.is_some(), "TextColor"),
        (draft.background.is_some(), "BackgroundColor"),
    ] {
        if !set && entries.iter().any(|(name, _)| name == key) {
            return Err(malformed(
                format,
                0,
                &cat(&["\"", key, "\" is not a colour aterm can read"]),
            ));
        }
    }
    draft.finish()
}

/// The bytes the base64 `text` encodes (whitespace skipped, `=` ends it), or `None` when it
/// holds any other character.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut pending: u32 = 0;
    let mut bits: u32 = 0;
    for byte in text.bytes() {
        let sextet = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ if byte.is_ascii_whitespace() => continue,
            _ => return None,
        };
        pending = pending << 6 | u32::from(sextet);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((pending >> bits) as u8);
            pending &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A binary property list (`bplist00`), read only as far as an archived colour needs: every
/// read is bounded by the bytes, so a damaged archive reads as `None`.
struct Bplist<'a> {
    bytes: &'a [u8],
    /// Bytes per offset-table entry.
    offset_size: usize,
    /// Bytes per object reference in an array or dictionary.
    ref_size: usize,
    /// Number of objects.
    count: usize,
    /// Where the offset table starts.
    table: usize,
    /// The root object.
    top: usize,
}

/// A binary property-list object, reduced to what an archived colour needs.
enum Bvalue<'a> {
    Int(u64),
    Data(&'a [u8]),
    Uid(usize),
    Array(Vec<usize>),
    Dict(Vec<(usize, usize)>),
    Other,
}

impl<'a> Bplist<'a> {
    fn new(bytes: &'a [u8]) -> Option<Self> {
        let trailer_at = bytes.len().checked_sub(32)?;
        if !bytes.starts_with(b"bplist00") || trailer_at < 8 {
            return None;
        }
        let trailer = bytes.get(trailer_at..)?;
        let offset_size = usize::from(*trailer.get(6)?);
        let ref_size = usize::from(*trailer.get(7)?);
        let count = be_usize(trailer.get(8..16)?)?;
        let top = be_usize(trailer.get(16..24)?)?;
        let table = be_usize(trailer.get(24..32)?)?;
        let table_end = count.checked_mul(offset_size)?.checked_add(table)?;
        let sizes = 1..=8;
        if !sizes.contains(&offset_size)
            || !sizes.contains(&ref_size)
            || table_end > trailer_at
            || top >= count
        {
            return None;
        }
        Some(Self {
            bytes,
            offset_size,
            ref_size,
            count,
            table,
            top,
        })
    }

    fn slice(&self, start: usize, len: usize) -> Option<&'a [u8]> {
        self.bytes.get(start..start.checked_add(len)?)
    }

    /// Object `index`'s marker byte and where the bytes after it start.
    fn locate(&self, index: usize) -> Option<(u8, usize)> {
        if index >= self.count {
            return None;
        }
        let entry = index
            .checked_mul(self.offset_size)?
            .checked_add(self.table)?;
        let offset = be_usize(self.slice(entry, self.offset_size)?)?;
        Some((*self.bytes.get(offset)?, offset.checked_add(1)?))
    }

    fn object(&self, index: usize) -> Option<Bvalue<'a>> {
        let (marker, body) = self.locate(index)?;
        let info = marker & 0x0f;
        Some(match marker >> 4 {
            0x1 => Bvalue::Int(be_u64(self.slice(body, 1usize.checked_shl(info.into())?)?)?),
            0x4 => {
                let (start, len) = self.extent(info, body)?;
                Bvalue::Data(self.slice(start, len)?)
            }
            0x8 => Bvalue::Uid(be_usize(
                self.slice(body, usize::from(info).checked_add(1)?)?,
            )?),
            0xa => {
                let (start, len) = self.extent(info, body)?;
                Bvalue::Array(self.refs(start, len)?)
            }
            0xd => {
                let (start, len) = self.extent(info, body)?;
                let keys = self.refs(start, len)?;
                let values_at = len.checked_mul(self.ref_size)?.checked_add(start)?;
                let values = self.refs(values_at, len)?;
                Bvalue::Dict(keys.into_iter().zip(values).collect())
            }
            _ => Bvalue::Other,
        })
    }

    /// Where an object's contents start and how many there are: the marker's low nibble, or
    /// for `0xf` the integer object after the marker.
    fn extent(&self, info: u8, body: usize) -> Option<(usize, usize)> {
        if info != 0x0f {
            return Some((body, usize::from(info)));
        }
        let marker = *self.bytes.get(body)?;
        if marker >> 4 != 0x1 {
            return None;
        }
        let width = 1usize.checked_shl((marker & 0x0f).into())?;
        let start = body.checked_add(1)?;
        let len = be_usize(self.slice(start, width)?)?;
        Some((start.checked_add(width)?, len))
    }

    fn refs(&self, start: usize, count: usize) -> Option<Vec<usize>> {
        let bytes = self.slice(start, count.checked_mul(self.ref_size)?)?;
        bytes.chunks_exact(self.ref_size).map(be_usize).collect()
    }

    /// Object `index` when it is an ASCII string; no other kind is read, so a key that names a
    /// large container costs no more than one that names a string.
    fn ascii(&self, index: usize) -> Option<&'a [u8]> {
        let (marker, body) = self.locate(index)?;
        if marker >> 4 != 0x5 {
            return None;
        }
        let (start, len) = self.extent(marker & 0x0f, body)?;
        self.slice(start, len)
    }

    /// The value `dict` holds under the ASCII key `key`.
    fn lookup(&self, dict: &[(usize, usize)], key: &str) -> Option<Bvalue<'a>> {
        let (_, value) = dict
            .iter()
            .find(|(name, _)| self.ascii(*name) == Some(key.as_bytes()))?;
        self.object(*value)
    }
}

/// A big-endian unsigned integer of at most 8 bytes.
fn be_u64(bytes: &[u8]) -> Option<u64> {
    (bytes.len() <= 8).then(|| {
        bytes
            .iter()
            .fold(0, |value, &byte| value << 8 | u64::from(byte))
    })
}

fn be_usize(bytes: &[u8]) -> Option<usize> {
    usize::try_from(be_u64(bytes)?).ok()
}

/// A JSON value, reduced to what a colour scheme needs.
#[derive(Debug, Clone, PartialEq)]
enum JsonValue {
    Object(Vec<(String, JsonValue)>),
    Array(Vec<JsonValue>),
    Text(String),
    Other,
}

struct JsonReader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl JsonReader<'_> {
    fn error(&self, reason: &str) -> ImportError {
        malformed(
            ImportFormat::WindowsTerminal,
            line_of(self.text, self.pos),
            reason,
        )
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    const fn bump(&mut self) {
        self.pos = self.pos.saturating_add(1);
    }

    fn skip_space(&mut self) -> Result<(), ImportError> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.bump(),
                Some(b'/') => {
                    let rest = self.text.get(self.pos..).unwrap_or("");
                    if rest.starts_with("//") {
                        let end = rest.find('\n').unwrap_or(rest.len());
                        self.pos = self.pos.saturating_add(end);
                    } else if rest.starts_with("/*") {
                        let end = rest
                            .get(2..)
                            .and_then(|after| after.find("*/"))
                            .ok_or_else(|| self.error("unterminated comment"))?;
                        self.pos = self.pos.saturating_add(end).saturating_add(4);
                    } else {
                        return Err(self.error("unexpected '/'"));
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn expect(&mut self, byte: u8, reason: &str) -> Result<(), ImportError> {
        self.skip_space()?;
        if self.peek() == Some(byte) {
            self.bump();
            Ok(())
        } else {
            Err(self.error(reason))
        }
    }

    fn string(&mut self) -> Result<String, ImportError> {
        self.expect(b'"', "expected a string")?;
        let mut out = String::new();
        loop {
            let rest = self.text.get(self.pos..).unwrap_or("");
            let mut chars = rest.chars();
            let ch = chars
                .next()
                .ok_or_else(|| self.error("unterminated string"))?;
            self.pos = self.pos.saturating_add(ch.len_utf8());
            match ch {
                '"' => return Ok(out),
                '\\' => {
                    let escape = self
                        .peek()
                        .ok_or_else(|| self.error("unterminated string"))?;
                    self.bump();
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let high = self.hex4()?;
                            let save = self.pos;
                            let code = if (0xD800..0xDC00).contains(&high)
                                && self
                                    .text
                                    .get(self.pos..)
                                    .is_some_and(|r| r.starts_with("\\u"))
                            {
                                self.pos = self.pos.saturating_add(2);
                                let low = self.hex4()?;
                                if (0xDC00..0xE000).contains(&low) {
                                    0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                                } else {
                                    self.pos = save;
                                    high
                                }
                            } else {
                                high
                            };
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.error("bad escape in a string")),
                    }
                }
                c if (c as u32) < 0x20 => return Err(self.error("control character in a string")),
                c => out.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, ImportError> {
        let digits = self
            .text
            .get(self.pos..self.pos.saturating_add(4))
            .ok_or_else(|| self.error("short \\u escape"))?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.error("bad \\u escape"));
        }
        let value = u32::from_str_radix(digits, 16).map_err(|_| self.error("bad \\u escape"))?;
        self.pos = self.pos.saturating_add(4);
        Ok(value)
    }

    fn value(&mut self, depth: usize) -> Result<JsonValue, ImportError> {
        if depth > MAX_NESTING {
            return Err(self.error("the document nests too deeply"));
        }
        self.skip_space()?;
        match self.peek() {
            Some(b'{') => {
                self.bump();
                let mut entries = Vec::new();
                loop {
                    self.skip_space()?;
                    if self.peek() == Some(b'}') {
                        self.bump();
                        return Ok(JsonValue::Object(entries));
                    }
                    let key = self.string()?;
                    self.expect(b':', "expected ':'")?;
                    let value = self.value(depth.saturating_add(1))?;
                    entries.push((key, value));
                    self.skip_space()?;
                    match self.peek() {
                        Some(b',') => self.bump(),
                        Some(b'}') => {}
                        _ => return Err(self.error("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.bump();
                let mut items = Vec::new();
                loop {
                    self.skip_space()?;
                    if self.peek() == Some(b']') {
                        self.bump();
                        return Ok(JsonValue::Array(items));
                    }
                    items.push(self.value(depth.saturating_add(1))?);
                    self.skip_space()?;
                    match self.peek() {
                        Some(b',') => self.bump(),
                        Some(b']') => {}
                        _ => return Err(self.error("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => Ok(JsonValue::Text(self.string()?)),
            Some(b't' | b'f' | b'n') => {
                let rest = self.text.get(self.pos..).unwrap_or("");
                let word = ["true", "false", "null"]
                    .into_iter()
                    .find(|word| rest.starts_with(word))
                    .ok_or_else(|| self.error("unexpected word"))?;
                self.pos = self.pos.saturating_add(word.len());
                Ok(JsonValue::Other)
            }
            Some(b'-' | b'0'..=b'9') => {
                while matches!(
                    self.peek(),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.bump();
                }
                Ok(JsonValue::Other)
            }
            _ => Err(self.error("expected a value")),
        }
    }

    fn document(&mut self) -> Result<JsonValue, ImportError> {
        let value = self.value(0)?;
        self.skip_space()?;
        if self.pos < self.bytes.len() {
            return Err(self.error("text after the document"));
        }
        Ok(value)
    }
}

fn windows_terminal_slot(key: &str) -> Option<Slot> {
    Some(match key {
        "foreground" => Slot::Foreground,
        "background" => Slot::Background,
        "cursorColor" => Slot::Cursor,
        "selectionBackground" => Slot::Selection,
        _ => match key.strip_prefix("bright") {
            Some(name) => {
                let mut lower = name.to_string();
                lower.make_ascii_lowercase();
                Slot::Ansi(ansi_name_index(&lower)?.saturating_add(8))
            }
            None => Slot::Ansi(ansi_name_index(key)?),
        },
    })
}

fn is_scheme_object(entries: &[(String, JsonValue)]) -> bool {
    entries
        .iter()
        .any(|(key, _)| key == "background" || key == "foreground")
}

fn scheme_object_name(entries: &[(String, JsonValue)]) -> Option<&str> {
    entries.iter().find_map(|(key, value)| match value {
        JsonValue::Text(name) if key == "name" => Some(name.as_str()),
        _ => None,
    })
}

fn convert_windows_terminal(text: &str, pick: Option<&str>) -> Result<ImportReport, ImportError> {
    let format = ImportFormat::WindowsTerminal;
    let mut reader = JsonReader {
        text,
        bytes: text.as_bytes(),
        pos: 0,
    };
    let document = reader.document()?;
    let candidates: Vec<&[(String, JsonValue)]> = match &document {
        JsonValue::Object(entries) if is_scheme_object(entries) => vec![entries.as_slice()],
        JsonValue::Object(entries) => entries
            .iter()
            .find_map(|(key, value)| match value {
                JsonValue::Array(items) if key == "schemes" => Some(items),
                _ => None,
            })
            .map(|items| scheme_objects(items))
            .unwrap_or_default(),
        JsonValue::Array(items) => scheme_objects(items),
        _ => Vec::new(),
    };
    let Some(first) = candidates.first() else {
        return Err(malformed(format, 0, "the document holds no colour scheme"));
    };
    let chosen = pick
        .and_then(|pick| {
            candidates.iter().find(|entries| {
                scheme_object_name(entries).is_some_and(|name| name.eq_ignore_ascii_case(pick))
            })
        })
        .unwrap_or(first);
    let mut draft = Draft::new(format);
    if candidates.len() > 1 {
        let names: Vec<String> = candidates
            .iter()
            .take(8)
            .map(|entries| quoted(scheme_object_name(entries).unwrap_or("(unnamed)")))
            .collect();
        let mut note = String::from("the file holds ");
        note.push_str(&candidates.len().to_string());
        note.push_str(" schemes (");
        note.push_str(&names.join(", "));
        if candidates.len() > names.len() {
            note.push_str(", …");
        }
        note.push_str("); imported \"");
        note.push_str(&quoted(scheme_object_name(chosen).unwrap_or("(unnamed)")));
        note.push_str("\" — pass --name with another scheme's name to import that one");
        draft.note(note);
    }
    for (key, value) in *chosen {
        if key == "name" {
            if let JsonValue::Text(name) = value {
                draft.name = Some(name.clone());
            }
            continue;
        }
        match (windows_terminal_slot(key), value) {
            (Some(slot), JsonValue::Text(colour)) => draft.colour(slot, key, colour),
            _ => draft.drop_key(key),
        }
    }
    draft.finish()
}

fn scheme_objects(items: &[JsonValue]) -> Vec<&[(String, JsonValue)]> {
    items
        .iter()
        .filter_map(|item| match item {
            JsonValue::Object(entries) if is_scheme_object(entries) => Some(entries.as_slice()),
            _ => None,
        })
        .collect()
}

const BASE_NAMES: [&str; 24] = [
    "base00", "base01", "base02", "base03", "base04", "base05", "base06", "base07", "base08",
    "base09", "base0A", "base0B", "base0C", "base0D", "base0E", "base0F", "base10", "base11",
    "base12", "base13", "base14", "base15", "base16", "base17",
];

fn looks_like_base16(text: &str) -> bool {
    text.lines().any(|line| {
        line.trim_start()
            .strip_prefix("base00")
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    })
}

fn base_index(key: &str) -> Option<usize> {
    let digits = key.strip_prefix("base")?;
    if digits.len() != 2 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let index = usize::from_str_radix(digits, 16).ok()?;
    (index < BASE_NAMES.len()).then_some(index)
}

fn yaml_scalar(raw: &str) -> &str {
    let raw = raw.trim();
    if raw.starts_with('"') || raw.starts_with('\'') {
        let quote = raw.chars().next().unwrap_or('"');
        if let Some(end) = raw.get(1..).and_then(|rest| rest.find(quote)) {
            return raw.get(1..end.saturating_add(1)).unwrap_or("");
        }
        return raw;
    }
    match raw.find(" #") {
        Some(comment) => raw.get(..comment).unwrap_or(raw).trim(),
        None => raw,
    }
}

fn convert_base16(text: &str) -> Result<ImportReport, ImportError> {
    let format = ImportFormat::Base16;
    let mut draft = Draft::new(format);
    let mut base: [Option<Rgb>; 24] = [None; 24];
    let mut in_palette = false;
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" || trimmed == "..." {
            continue;
        }
        let indented = raw.starts_with([' ', '\t']);
        if !indented {
            in_palette = false;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            draft.drop_key(trimmed);
            continue;
        };
        let key = yaml_scalar(key);
        let value = yaml_scalar(value);
        if let Some(index) = base_index(key).filter(|_| !indented || in_palette) {
            match (parse_foreign_colour(value), base.get_mut(index)) {
                (Some(colour), Some(slot)) => *slot = Some(colour),
                _ => {
                    draft.drop_key(key);
                    draft.note(cat(&[
                        "\"",
                        &quoted(key),
                        "\" is not a hex colour (\"",
                        &quoted(value),
                        "\"), so it was dropped",
                    ]));
                }
            }
            continue;
        }
        if indented {
            draft.drop_key(key);
            continue;
        }
        match key.to_ascii_lowercase().as_str() {
            "palette" if value.is_empty() => in_palette = true,
            "scheme" | "name" => draft.name = Some(value.to_string()),
            "author" => draft.author = Some(value.to_string()),
            "description" => draft.description = Some(value.to_string()),
            "variant" => match value.to_ascii_lowercase().as_str() {
                "dark" => draft.appearance = Some(Appearance::Dark),
                "light" => draft.appearance = Some(Appearance::Light),
                _ => draft.drop_key(key),
            },
            "system" if matches!(value, "base16" | "base24") => {}
            "slug" => {}
            _ => draft.drop_key(key),
        }
    }
    let get = |index: usize| -> Result<Rgb, ImportError> {
        base.get(index)
            .copied()
            .flatten()
            .ok_or(ImportError::MissingColour {
                format,
                key: BASE_NAMES.get(index).copied().unwrap_or("base00"),
            })
    };
    let normal = [0x00, 0x08, 0x0B, 0x0A, 0x0D, 0x0E, 0x0C, 0x05];
    for (slot, &source) in normal.iter().enumerate() {
        draft.set(Slot::Ansi(slot), get(source)?);
    }
    let base24 = (0x12..=0x17).all(|index| base.get(index).copied().flatten().is_some());
    let bright = if base24 {
        [0x03, 0x12, 0x14, 0x13, 0x16, 0x17, 0x15, 0x07]
    } else {
        [0x03, 0x08, 0x0B, 0x0A, 0x0D, 0x0E, 0x0C, 0x07]
    };
    for (slot, &source) in bright.iter().enumerate() {
        draft.set(Slot::Ansi(slot.saturating_add(8)), get(source)?);
    }
    draft.set(Slot::Foreground, get(0x05)?);
    draft.set(Slot::Background, get(0x00)?);
    draft.set(Slot::Cursor, get(0x05)?);
    draft.set(Slot::Selection, get(0x02)?);
    draft.finish()
}

#[cfg(test)]
#[path = "scheme_import_tests.rs"]
mod tests;
