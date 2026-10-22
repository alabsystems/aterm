// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `aterm theme import`: offline converters from other terminals' theme
//! formats into aterm's native theme file.
//!
//! The native reader ([`crate::scheme::parse_theme_str`]) stays the one parser
//! of untrusted theme text the window ever runs: it accepts aterm's own
//! `key = value` format and nothing else. Another terminal's theme comes in
//! only through an explicit import, which converts it here once and writes a
//! native file ([`to_native_conf`]) that the window then reads like any other.
//!
//! Every converter is pure and bounded: input above [`MAX_IMPORT_BYTES`] is
//! refused before it is decoded, nothing is fetched, no include or import line
//! is followed, and the container formats (plist, JSON, TOML tables) are read
//! to a fixed nesting depth. A key with no aterm equivalent is not an error:
//! it is listed in [`ImportReport::dropped`], so the user sees what the
//! conversion left behind. Only a document that cannot be read at all, or one
//! with no foreground or background, fails.
//!
//! Formats: kitty (`key value` lines), Ghostty (`key = value`,
//! `palette = N=#hex`), Alacritty TOML (`[colors.*]`), iTerm2 `.itermcolors`
//! (an XML property list), a Windows Terminal JSON scheme (alone, in an array,
//! or in a `settings.json`), base16 / base24 YAML, and aterm's own format.

use crate::Rgb;
use crate::scheme::{
    APPEARANCE_VALUES, Appearance, ChromeColors, ColorScheme, ForeignFormat, MAX_THEME_TEXT_CHARS,
    MAX_USER_THEME_FILE_BYTES, ParsedTheme, THEME_KEYS, ThemeKey, ThemeKeyGroup, parse_hex,
    parse_theme_str, sniff_foreign_format,
};

/// The largest theme file an import reads, in bytes: the same bound the
/// window puts on a theme file in the themes folder.
pub const MAX_IMPORT_BYTES: usize = MAX_USER_THEME_FILE_BYTES;

/// How deep a plist or JSON document may nest, and how many parts a TOML key
/// may have. A theme needs three levels; settings files need about six.
const MAX_NESTING: usize = 16;

/// How many dropped keys a report lists by name; the rest are counted.
const MAX_DROPPED: usize = 128;

/// How many notes a report carries.
const MAX_NOTES: usize = 32;

/// How many characters of a foreign key or value a report quotes.
const MAX_QUOTED_CHARS: usize = 64;

/// The theme formats an import reads.
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
    /// iTerm2: an `.itermcolors` XML property list.
    ITerm,
    /// Windows Terminal: a JSON colour scheme object.
    WindowsTerminal,
    /// base16 / base24: a YAML scheme with `base00` … `base0F` (… `base17`).
    Base16,
}

impl ImportFormat {
    /// Every format, in the order detection prefers them.
    pub const ALL: &'static [ImportFormat] = &[
        ImportFormat::Native,
        ImportFormat::Kitty,
        ImportFormat::Ghostty,
        ImportFormat::Alacritty,
        ImportFormat::ITerm,
        ImportFormat::WindowsTerminal,
        ImportFormat::Base16,
    ];

    /// The user-facing name of the format.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ImportFormat::Native => "aterm",
            ImportFormat::Kitty => "kitty",
            ImportFormat::Ghostty => "Ghostty",
            ImportFormat::Alacritty => "Alacritty",
            ImportFormat::ITerm => "iTerm2",
            ImportFormat::WindowsTerminal => "Windows Terminal",
            ImportFormat::Base16 => "base16",
        }
    }
}

/// What a conversion produced: the colours and identity text the native file
/// will carry, and what the source held that aterm has no place for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// The format the source was read as.
    pub format: ImportFormat,
    /// The converted colours. `name` is the source's own name for the theme
    /// (empty when it has none); `appearance` is `Dark` unless the source
    /// declared one ([`ImportReport::appearance_declared`]).
    pub scheme: ColorScheme,
    /// Whether the source said whether it is dark or light. When it did not,
    /// the native file leaves `appearance` out and the catalog measures it.
    pub appearance_declared: bool,
    /// The source's description, if it carries one.
    pub description: Option<String>,
    /// The source's author, if it carries one.
    pub author: Option<String>,
    /// The source's keys that have no aterm equivalent, in source order and
    /// without repeats. Dropping a key is not an error.
    pub dropped: Vec<String>,
    /// What the conversion assumed or filled in, one sentence each.
    pub notes: Vec<String>,
}

impl ImportReport {
    /// This report as a native theme file, with `header` written as comment
    /// lines above it. [`parse_theme_str`] reads the result back to this
    /// report's scheme, description, author and appearance flag.
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

/// Why an import failed.
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
        // Trust gate: `write_str` rather than `write!` (see `trust_fmt`).
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
                "the file is not a theme format aterm imports (kitty, Ghostty, Alacritty \
                 TOML, iTerm2 .itermcolors, Windows Terminal JSON, base16 YAML)",
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

/// Read `bytes` (the contents of `file_name`, whose extension guides format
/// detection) as a theme in any format an import reads. `pick` names the
/// scheme to take when the file holds several (a Windows Terminal
/// `settings.json`); without a match the first is taken and a note says so.
///
/// # Errors
/// [`ImportError::TooLarge`] before anything is decoded, then
/// [`ImportError::NotUtf8`], [`ImportError::UnknownFormat`], or the
/// converter's own error.
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

/// Which format `text` (from a file named `file_name`) is in. A distinctive
/// extension decides; otherwise the content does. `None` when it is none of
/// them.
#[must_use]
pub fn detect_format(file_name: Option<&str>, text: &str) -> Option<ImportFormat> {
    let extension = file_name
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase());
    match extension.as_deref() {
        Some("itermcolors" | "plist") => return Some(ImportFormat::ITerm),
        Some("json" | "jsonc") => return Some(ImportFormat::WindowsTerminal),
        Some("toml") => return Some(ImportFormat::Alacritty),
        Some("yaml" | "yml") => return Some(ImportFormat::Base16),
        _ => {}
    }
    let text = strip_bom(text);
    if parse_theme_str(text).is_ok() {
        return Some(ImportFormat::Native);
    }
    match sniff_foreign_format(text) {
        Some(ForeignFormat::Kitty) => Some(ImportFormat::Kitty),
        Some(ForeignFormat::Ghostty) => Some(ImportFormat::Ghostty),
        Some(ForeignFormat::Plist) => Some(ImportFormat::ITerm),
        Some(ForeignFormat::Toml) => Some(ImportFormat::Alacritty),
        Some(ForeignFormat::Json) => Some(ImportFormat::WindowsTerminal),
        None if looks_like_base16(text) => Some(ImportFormat::Base16),
        None => None,
    }
}

/// Convert `text` as `format`. `pick` is as for [`import_theme`].
///
/// # Errors
/// [`ImportError::TooLarge`] past [`MAX_IMPORT_BYTES`];
/// [`ImportError::Malformed`] for a document that cannot be read;
/// [`ImportError::MissingColour`] when no foreground or background results.
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
        ImportFormat::Alacritty => convert_alacritty(text),
        ImportFormat::ITerm => convert_iterm(text),
        ImportFormat::WindowsTerminal => convert_windows_terminal(text, pick),
        ImportFormat::Base16 => convert_base16(text),
    }
}

/// The native theme file for a scheme: the one serializer of aterm's format.
/// `header` lines are written first as `#` comments. Text fields are made
/// safe for the format (no control characters, at most
/// [`MAX_THEME_TEXT_CHARS`], nothing the reader would take for a comment), so
/// [`parse_theme_str`] always reads the output; when they already were safe
/// it reads back exactly `scheme`, `description`, `author` and
/// `appearance_declared` (with `scheme.appearance` `Dark` when undeclared).
/// Keys are written in [`THEME_KEYS`] order.
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

/// The native theme file for a whole parsed theme: colours, identity,
/// `schema`, `bold_is_bright` and typography, in [`THEME_KEYS`] order, the
/// typography under a header saying aterm.toml keys of the same name win.
/// [`parse_theme_str`] reads it back to `theme` (with no skipped keys: a key
/// this aterm does not know cannot be written) whenever its text fields were
/// already safe.
#[must_use]
pub fn to_native_conf_package(theme: &ParsedTheme, header: &[&str]) -> String {
    write_native_conf(theme, header, false)
}

/// [`to_native_conf_package`] for a person to edit, as
/// [`to_native_conf_commented`] is: every key the theme leaves out, the
/// typography keys too, is a commented-out example under its description.
#[must_use]
pub fn to_native_conf_package_commented(theme: &ParsedTheme, header: &[&str]) -> String {
    write_native_conf(theme, header, true)
}

/// [`to_native_conf`] for a person to edit: the same keys and values, each
/// under a comment from [`THEME_KEYS`] saying what it does and what leaving
/// it out means, every key the theme leaves out written as a commented-out
/// example line, and each ANSI slot named. It reads back exactly as
/// [`to_native_conf`]'s output does.
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

/// The ANSI colour names, slot `0..8` (`8..16` are their bright forms).
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

/// Append `#rrggbb`.
fn push_hex_colour(out: &mut String, colour: Rgb) {
    out.push('#');
    for byte in [colour.r, colour.g, colour.b] {
        for nibble in [byte >> 4, byte & 0xF] {
            out.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
}

/// `text` made safe as a native `name` / `description` / `author` value: control
/// characters become spaces, whitespace before a `#` or `;` is removed (the
/// reader would take ` #…` for a comment), it is cut to
/// [`MAX_THEME_TEXT_CHARS`] and trimmed. `None` when nothing is left.
/// Idempotent, and the identity on text that was already safe: what
/// [`to_native_conf`] writes for a text field is exactly this.
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

/// A foreign key or value as a report quotes it: control characters removed,
/// at most [`MAX_QUOTED_CHARS`].
fn quoted(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_QUOTED_CHARS)
        .collect()
}

/// Concatenate (Trust gate: no runtime `format!` in this crate).
fn cat(parts: &[&str]) -> String {
    let mut out = String::new();
    for part in parts {
        out.push_str(part);
    }
    out
}

/// A colour as other terminals write it: `#RRGGBB`, `RRGGBB`, `#RGB`, `RGB`
/// (Ghostty writes hex without `#`), `0xRRGGBB`, optionally quoted. Laxer
/// than the native [`parse_hex`] on purpose: the importer reads another
/// terminal's file, and what it writes back is always the six-digit form.
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

/// The `N` of a `<prefix>N` key, for up to three decimal digits.
fn indexed_key(key: &str, prefix: &str) -> Option<usize> {
    let digits = key.strip_prefix(prefix)?;
    if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The note every format's include / import directive earns.
const INCLUDE_NOTE: &str = "include and import lines are not followed: import the included file \
                            on its own if it holds colours";
/// The note a palette entry above 15 earns.
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

/// A conversion in progress: what has been read so far.
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

    /// Set `slot` from `value`, or drop `key` with a note when `value` is not
    /// a colour aterm reads.
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

    /// Set the theme's `palette_generate` from a `true`/`false` value, or
    /// drop `key` when the value is neither.
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
            let mut note = String::from(if missing.len() == 1 {
                "ANSI colour "
            } else {
                "ANSI colours "
            });
            note.push_str(&missing.join(", "));
            note.push_str(" are not in the file: aterm's defaults fill them");
            self.note(note);
        }
        if self.dropped_unlisted > 0 {
            let count = self.dropped_unlisted.to_string();
            self.note(cat(&["and ", &count, " more dropped keys not listed"]));
        }
        // An unset slot keeps the engine default, as in a native file.
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

/// A [`ImportError::Malformed`].
fn malformed(format: ImportFormat, line: usize, reason: &str) -> ImportError {
    ImportError::Malformed {
        format,
        line,
        reason: reason.to_string(),
    }
}

/// The 1-based line `offset` falls on.
fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()
        .iter()
        .take(offset)
        .filter(|&&b| b == b'\n')
        .count()
        .saturating_add(1)
}

// ---------------------------------------------------------------------------
// aterm (native)
// ---------------------------------------------------------------------------

fn convert_native(text: &str) -> Result<ImportReport, ImportError> {
    match parse_theme_str(text) {
        Ok(parsed) => {
            // An import carries colours and identity only; a native file's
            // type settings, `bold_is_bright` and `schema` are named as
            // dropped so nothing goes silently.
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

// ---------------------------------------------------------------------------
// kitty
// ---------------------------------------------------------------------------

/// kitty: `key value` lines, `#` comments, and the kitty-themes metadata
/// comments (`## name: …`, `## author: …`, `## blurb: …`). Later lines win.
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
            // `none` keeps each cell's own colour, which is aterm's default too.
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

// ---------------------------------------------------------------------------
// Ghostty
// ---------------------------------------------------------------------------

/// Ghostty: `key = value` lines with hyphenated keys, `palette = N=#hex`.
/// Later lines win.
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

// ---------------------------------------------------------------------------
// Alacritty (a TOML subset)
// ---------------------------------------------------------------------------

/// Where `#` starts a comment in a TOML line (outside strings), or the
/// line's length.
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

/// Split `s` at each top-level `separator` (outside strings, brackets and
/// braces). `None` when a string is left open.
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

/// The net bracket depth change of a TOML line, outside strings.
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

/// A dotted TOML key (`colors.primary`, `"colors".'normal'`) as its parts.
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

/// A single-line TOML string value (basic `"…"` with escapes, or literal
/// `'…'`). `None` for any other value.
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

/// Alacritty: the `[colors.*]` tables of its TOML config or theme file.
/// Every other key is dropped; `import` lines are not followed.
fn convert_alacritty(text: &str) -> Result<ImportReport, ImportError> {
    let format = ImportFormat::Alacritty;
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
        // The value is everything after the first top-level `=`.
        let value = line.get(key.len().saturating_add(1)..).unwrap_or("").trim();
        let mut path = table.clone();
        path.extend(toml_key(key).ok_or_else(|| malformed(format, lineno, "bad key"))?);
        if path.len() > MAX_NESTING {
            return Err(malformed(format, lineno, "keys nest too deeply"));
        }
        if value.starts_with("\"\"\"") || value.starts_with("'''") {
            // A multi-line string is never a colour: skip to its close.
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
            alacritty_assign(&mut draft, &path, None);
            continue;
        }
        let mut depth = toml_bracket_delta(value);
        while depth > 0 {
            // A multi-line array or table: never a colour, skip to its close.
            let Some((_, next)) = lines.next() else {
                return Err(malformed(format, lineno, "unterminated array"));
            };
            let next = next.get(..toml_comment_start(next)).unwrap_or(next);
            depth = depth.saturating_add(toml_bracket_delta(next));
        }
        if let Some(inner) = value
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
        {
            let pairs =
                toml_split(inner, b',').ok_or_else(|| malformed(format, lineno, "bad table"))?;
            for pair in pairs.into_iter().filter(|p| !p.trim().is_empty()) {
                let (sub_key, sub_value) = pair
                    .split_once('=')
                    .ok_or_else(|| malformed(format, lineno, "expected `key = value`"))?;
                let mut sub_path = path.clone();
                sub_path
                    .extend(toml_key(sub_key).ok_or_else(|| malformed(format, lineno, "bad key"))?);
                alacritty_assign(&mut draft, &sub_path, toml_string(sub_value).as_deref());
            }
            continue;
        }
        alacritty_assign(&mut draft, &path, toml_string(value).as_deref());
    }
    draft.finish()
}

/// Place one Alacritty key. `value` is the string value, `None` for a value
/// of any other type.
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
    let Some(value) = value else {
        return draft.drop_key(&dotted);
    };
    match value {
        // Retaining the cell's foreground is native selection text's default.
        "CellForeground" if slot == Slot::SelectionForeground => {}
        "CellForeground" | "CellBackground" => {
            draft.drop_key(&dotted);
            draft.note(
                "Alacritty's CellForeground / CellBackground (the cell's own colour) has no \
                 aterm theme key: aterm's default is used",
            );
        }
        _ => draft.colour(slot, &dotted, value),
    }
}

// ---------------------------------------------------------------------------
// iTerm2 (.itermcolors, an XML property list)
// ---------------------------------------------------------------------------

/// A property-list value, reduced to what a colour theme needs.
#[derive(Debug, Clone, PartialEq)]
enum PlistValue {
    Dict(Vec<(String, PlistValue)>),
    Number(f64),
    Text(String),
    Other,
}

/// A bounded scanner over a property list's XML.
struct PlistScanner<'a> {
    text: &'a str,
    pos: usize,
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
        malformed(ImportFormat::ITerm, line_of(self.text, self.pos), reason)
    }

    /// Skip whitespace, the XML declaration, processing instructions,
    /// comments and the DOCTYPE.
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

    /// The next tag: its name and kind.
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
        // The name ends at the first space (attributes follow it).
        let name = inner.split_whitespace().next().unwrap_or("");
        Ok((name, kind))
    }

    /// The text up to `</name>`, which is consumed.
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
                Ok(text
                    .trim()
                    .parse::<f64>()
                    .map_or(PlistValue::Other, PlistValue::Number))
            }
            ("string", TagKind::Open) => Ok(PlistValue::Text(self.text_until_close("string")?)),
            ("string", TagKind::Empty) => Ok(PlistValue::Text(String::new())),
            ("date" | "data", TagKind::Open) => {
                self.text_until_close(name)?;
                Ok(PlistValue::Other)
            }
            ("true" | "false", TagKind::Empty) => Ok(PlistValue::Other),
            _ => Err(self.error("unexpected element in the property list")),
        }
    }

    /// The document's root value, inside `<plist>` when there is one.
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

/// XML text with the five predefined entities and numeric references decoded.
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
                .map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| entity.strip_prefix('#').map(str::parse::<u32>))
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

/// The slot an iTerm2 colour key names.
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

/// An iTerm2 colour dictionary as an [`Rgb`], and its colour space when that
/// is not sRGB.
fn iterm_colour(entries: &[(String, PlistValue)]) -> Option<(Rgb, Option<String>)> {
    let component = |name: &str| {
        entries.iter().find_map(|(key, value)| match value {
            PlistValue::Number(n) if key == name && n.is_finite() => {
                // Clamped to 0..=255 before the cast, so it is exact.
                Some((n.clamp(0.0, 1.0) * 255.0).round() as u8)
            }
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

/// iTerm2: the colour dictionaries of an `.itermcolors` property list. A
/// key's `(Dark)` variant stands in when the plain key is absent; `(Light)`
/// variants are dropped.
fn convert_iterm(text: &str) -> Result<ImportReport, ImportError> {
    let format = ImportFormat::ITerm;
    let mut scanner = PlistScanner { text, pos: 0 };
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

// ---------------------------------------------------------------------------
// Windows Terminal (JSON, with comments and trailing commas)
// ---------------------------------------------------------------------------

/// A JSON value, reduced to what a colour scheme needs.
#[derive(Debug, Clone, PartialEq)]
enum JsonValue {
    Object(Vec<(String, JsonValue)>),
    Array(Vec<JsonValue>),
    Text(String),
    Other,
}

/// A bounded JSON reader that accepts the JSONC Windows Terminal writes
/// (`//` and `/* */` comments, trailing commas).
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

    fn bump(&mut self) {
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
                            let code = if (0xD800..0xDC00).contains(&high)
                                && self
                                    .text
                                    .get(self.pos..)
                                    .is_some_and(|r| r.starts_with("\\u"))
                            {
                                self.pos = self.pos.saturating_add(2);
                                let low = self.hex4()?;
                                0x10000
                                    + ((high - 0xD800) << 10)
                                    + (low.wrapping_sub(0xDC00) & 0x3FF)
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

/// The slot a Windows Terminal scheme key names.
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

/// Whether a JSON object looks like a colour scheme.
fn is_scheme_object(entries: &[(String, JsonValue)]) -> bool {
    entries
        .iter()
        .any(|(key, _)| key == "background" || key == "foreground")
}

/// The `name` of a scheme object, if it has one.
fn scheme_object_name(entries: &[(String, JsonValue)]) -> Option<&str> {
    entries.iter().find_map(|(key, value)| match value {
        JsonValue::Text(name) if key == "name" => Some(name.as_str()),
        _ => None,
    })
}

/// Windows Terminal: a scheme object on its own, in an array, or in a
/// `settings.json`'s `schemes`. `pick` chooses among several by name.
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

/// The scheme objects among `items`.
fn scheme_objects(items: &[JsonValue]) -> Vec<&[(String, JsonValue)]> {
    items
        .iter()
        .filter_map(|item| match item {
            JsonValue::Object(entries) if is_scheme_object(entries) => Some(entries.as_slice()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// base16 / base24 (YAML)
// ---------------------------------------------------------------------------

/// The base16 / base24 slot names, `base00` … `base17`.
const BASE_NAMES: [&str; 24] = [
    "base00", "base01", "base02", "base03", "base04", "base05", "base06", "base07", "base08",
    "base09", "base0A", "base0B", "base0C", "base0D", "base0E", "base0F", "base10", "base11",
    "base12", "base13", "base14", "base15", "base16", "base17",
];

/// Whether `text` looks like a base16 scheme: a top-level or `palette:` line
/// setting `base00`.
fn looks_like_base16(text: &str) -> bool {
    text.lines().any(|line| {
        line.trim_start()
            .strip_prefix("base00")
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    })
}

/// The index of a `baseXX` key (case-insensitive hex), below 24.
fn base_index(key: &str) -> Option<usize> {
    let digits = key.strip_prefix("base")?;
    if digits.len() != 2 {
        return None;
    }
    let index = usize::from_str_radix(digits, 16).ok()?;
    (index < BASE_NAMES.len()).then_some(index)
}

/// A YAML scalar with its quotes and a trailing ` # comment` removed.
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

/// base16 / base24: the classic flat file (`scheme:`, `author:`, `base00:` …)
/// and the tinted-theming form (`name:`, `variant:`, a `palette:` map).
/// Colours map the way base16-shell does; with all of `base12` … `base17`
/// present (base24), the bright colours 9-14 take them.
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
            // The file's id in the tinted-theming repositories, not a colour.
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
    // base16-shell's mapping: 0 bg, 1 red, 2 green, 3 yellow, 4 blue,
    // 5 magenta, 6 cyan, 7 fg; 8 comments, 9-14 repeat 1-6, 15 brightest.
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
mod tests {
    use super::*;

    fn rgb(hex: u32) -> Rgb {
        Rgb::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }

    /// The scheme a sample is expected to convert to: the given chrome and
    /// palette, appearance `Dark` undeclared.
    fn expected(
        name: &str,
        fg: u32,
        bg: u32,
        cursor: Option<u32>,
        selection: Option<u32>,
        selection_fg: Option<u32>,
        ansi: [u32; 16],
    ) -> ColorScheme {
        ColorScheme {
            name: name.to_string(),
            appearance: Appearance::Dark,
            foreground: rgb(fg),
            background: rgb(bg),
            cursor: cursor.map(rgb),
            selection: selection.map(rgb),
            selection_fg: selection_fg.map(rgb),
            ansi: ansi.map(rgb),
            chrome: ChromeColors::default(),
            palette_generate: false,
        }
    }

    /// One palette shared by every format's sample, so each converter is
    /// checked against the same answer.
    const PALETTE: [u32; 16] = [
        0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868,
        0xff899d, 0x9fe044, 0xfaba4a, 0x8db0ff, 0xc7a9ff, 0xa4daff, 0xc0caf5,
    ];

    /// The law every report keeps: the native file it writes reads back to
    /// the same scheme, text and appearance flag.
    fn assert_round_trips(report: &ImportReport) {
        let text = report.to_native_conf(&["Imported by a test", "from \u{7}a bell"]);
        let parsed = parse_theme_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(
            parsed,
            ParsedTheme::colours_only(
                report.scheme.clone(),
                report.appearance_declared,
                report.description.clone(),
                report.author.clone(),
            ),
            "{text}"
        );
    }

    fn kitty_sample() -> String {
        let mut text = String::from(
            "# vim:ft=kitty\n\
             ## name: Tokyo Night\n\
             ## author: enkia\n\
             ## blurb: A clean dark theme\n\
             foreground #c0caf5\n\
             background #1a1b26\n\
             cursor     #c0caf5\n\
             selection_background #283457\n\
             selection_foreground none\n\
             url_color #73daca\n\
             active_tab_background #7aa2f7\n\
             color16 #ff9e64\n\
             include ../extra.conf\n",
        );
        for (index, colour) in PALETTE.iter().enumerate() {
            text.push_str(&format!("color{index} #{colour:06x}\n"));
        }
        text
    }

    #[test]
    fn kitty_converts_structurally() {
        let report = convert(ImportFormat::Kitty, &kitty_sample(), None).unwrap();
        let mut want = expected(
            "Tokyo Night",
            0xc0caf5,
            0x1a1b26,
            Some(0xc0caf5),
            Some(0x283457),
            None,
            PALETTE,
        );
        // kitty's active tab background is aterm's `active_tab_color`.
        want.chrome.active_tab = Some(rgb(0x7aa2f7));
        assert_eq!(report.scheme, want);
        assert_eq!(report.author.as_deref(), Some("enkia"));
        assert_eq!(report.description.as_deref(), Some("A clean dark theme"));
        assert_eq!(report.dropped, ["url_color", "color16", "include"]);
        assert!(report.notes.iter().any(|n| n.contains("not followed")));
        assert_round_trips(&report);
    }

    #[test]
    fn ghostty_converts_structurally() {
        let mut text = String::from(
            "background = 1a1b26\n\
             foreground = #c0caf5\n\
             cursor-color = #c0caf5\n\
             cursor-text = #1a1b26\n\
             selection-background = #283457\n\
             selection-foreground = #c0caf5\n\
             palette = 200=#123456\n\
             config-file = other\n\
             font-family = Iosevka\n",
        );
        for (index, colour) in PALETTE.iter().enumerate() {
            text.push_str(&format!("palette = {index}=#{colour:06x}\n"));
        }
        let report = convert(ImportFormat::Ghostty, &text, None).unwrap();
        assert_eq!(
            report.scheme,
            expected(
                "",
                0xc0caf5,
                0x1a1b26,
                Some(0xc0caf5),
                Some(0x283457),
                Some(0xc0caf5),
                PALETTE
            )
        );
        assert_eq!(
            report.dropped,
            ["cursor-text", "palette 200", "config-file", "font-family"]
        );
        assert_round_trips(&report);
    }

    /// Spec `scheme-palette-generate-key`: Ghostty's `palette-generate`
    /// carries to the theme's `palette_generate`, is written back out and
    /// reads again. NEGATIVE CONTROL: a value that is not `true`/`false` is
    /// dropped, and the flag stays off.
    #[test]
    fn ghostty_import_carries_palette_generate() {
        let base = "background = #1a1b26\nforeground = #c0caf5\n";
        let on = convert(
            ImportFormat::Ghostty,
            &format!("{base}palette-generate = true\n"),
            None,
        )
        .unwrap();
        assert!(on.scheme.palette_generate);
        assert!(on.dropped.is_empty(), "{:?}", on.dropped);
        assert_round_trips(&on);
        let bad = convert(
            ImportFormat::Ghostty,
            &format!("{base}palette-generate = maybe\n"),
            None,
        )
        .unwrap();
        assert!(!bad.scheme.palette_generate);
        assert_eq!(bad.dropped, ["palette-generate"]);
    }

    /// THE CHROME KEYS IMPORT FROM THEIR FOREIGN NAMES: Ghostty's
    /// `split-divider-color` is `border_color` and Alacritty's search-match
    /// background is `search_match_color`, each written back out and read
    /// again. NEGATIVE CONTROL: a neighbouring foreign chrome key aterm has no
    /// key for (Ghostty's `cursor-text`, Alacritty's search-match foreground)
    /// is still dropped.
    #[test]
    fn chrome_keys_import_from_their_foreign_names() {
        let ghostty = "background = #1a1b26\n\
                       foreground = #c0caf5\n\
                       cursor-text = #1a1b26\n\
                       split-divider-color = #3b4261\n";
        let report = convert(ImportFormat::Ghostty, ghostty, None).unwrap();
        assert_eq!(report.scheme.chrome.border, Some(rgb(0x3b4261)));
        assert_eq!(report.dropped, ["cursor-text"]);
        assert_round_trips(&report);
        let alacritty = "[colors.primary]\n\
                         background = \"#1a1b26\"\n\
                         foreground = \"#c0caf5\"\n\
                         [colors.search.matches]\n\
                         foreground = \"#1a1b26\"\n\
                         background = \"#3d4f2f\"\n";
        let report = convert(ImportFormat::Alacritty, alacritty, None).unwrap();
        assert_eq!(report.scheme.chrome.search_match, Some(rgb(0x3d4f2f)));
        assert_eq!(report.dropped, ["colors.search.matches.foreground"]);
        assert_round_trips(&report);
    }

    #[test]
    fn alacritty_converts_structurally() {
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let mut text = String::from(
            "# Tokyo Night\n\
             import = [\n  \"~/.config/alacritty/base.toml\",\n]\n\
             [window]\nopacity = 0.9\n\
             [colors.primary]\n\
             background = '#1a1b26' # trailing comment\n\
             foreground = \"0xc0caf5\"\n\
             dim_foreground = \"#a9b1d6\"\n\
             [colors.cursor]\n\
             cursor = \"#c0caf5\"\n\
             text = \"CellBackground\"\n\
             [colors]\n\
             selection = { background = \"#283457\", text = \"CellForeground\" }\n",
        );
        for (group, offset) in [("normal", 0), ("bright", 8)] {
            text.push_str(&format!("[colors.{group}]\n"));
            for (index, name) in names.iter().enumerate() {
                text.push_str(&format!("{name} = \"#{:06x}\"\n", PALETTE[index + offset]));
            }
        }
        let report = convert(ImportFormat::Alacritty, &text, None).unwrap();
        assert_eq!(
            report.scheme,
            expected(
                "",
                0xc0caf5,
                0x1a1b26,
                Some(0xc0caf5),
                Some(0x283457),
                None,
                PALETTE
            )
        );
        assert_eq!(
            report.dropped,
            [
                "import",
                "window.opacity",
                "colors.primary.dim_foreground",
                "colors.cursor.text"
            ]
        );
        assert_round_trips(&report);
    }

    #[test]
    fn alacritty_reports_selection_text_that_cannot_follow_the_cell_background() {
        for (value, dropped) in [("CellForeground", false), ("CellBackground", true)] {
            let text = format!(
                "[colors.primary]\nforeground = '#c0caf5'\nbackground = '#1a1b26'\n\
                 [colors.selection]\ntext = '{value}'\n"
            );
            let report = convert(ImportFormat::Alacritty, &text, None).unwrap();
            // Native selection text can retain the cell foreground, but it
            // cannot dynamically follow each cell's background.
            assert_eq!(report.scheme.selection_fg, None);
            assert_eq!(
                report
                    .dropped
                    .iter()
                    .any(|key| key == "colors.selection.text"),
                dropped,
                "{value}"
            );
            // Both inputs omit the ANSI palette; the unsupported dynamic
            // background additionally earns a conversion-loss note.
            assert_eq!(report.notes.len(), if dropped { 2 } else { 1 }, "{value}");
            assert_round_trips(&report);
        }
    }

    fn iterm_colour_dict(hex: u32, space: &str) -> String {
        let c = rgb(hex);
        format!(
            "<dict>\n<key>Alpha Component</key><real>1</real>\n\
             <key>Blue Component</key><real>{}</real>\n\
             <key>Color Space</key><string>{space}</string>\n\
             <key>Green Component</key><real>{}</real>\n\
             <key>Red Component</key><real>{}</real>\n</dict>\n",
            f64::from(c.b) / 255.0,
            f64::from(c.g) / 255.0,
            f64::from(c.r) / 255.0,
        )
    }

    #[test]
    fn iterm_converts_structurally() {
        let mut text = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n<!-- generated -->\n",
        );
        for (index, colour) in PALETTE.iter().enumerate() {
            text.push_str(&format!("<key>Ansi {index} Color</key>\n"));
            text.push_str(&iterm_colour_dict(*colour, "sRGB"));
        }
        for (key, colour) in [
            ("Background Color", 0x1a1b26),
            ("Foreground Color", 0xc0caf5),
            ("Cursor Color", 0xc0caf5),
            ("Selection Color", 0x283457),
            ("Selected Text Color", 0xc0caf5),
            ("Link Color", 0x73daca),
            ("Foreground Color (Light)", 0x000000),
        ] {
            text.push_str(&format!("<key>{key}</key>\n"));
            text.push_str(&iterm_colour_dict(colour, "sRGB"));
        }
        text.push_str("<key>Badge &amp; Tab</key><string>x</string>\n</dict>\n</plist>\n");
        let report = convert(ImportFormat::ITerm, &text, None).unwrap();
        assert_eq!(
            report.scheme,
            expected(
                "",
                0xc0caf5,
                0x1a1b26,
                Some(0xc0caf5),
                Some(0x283457),
                Some(0xc0caf5),
                PALETTE
            )
        );
        assert_eq!(
            report.dropped,
            ["Link Color", "Foreground Color (Light)", "Badge & Tab"]
        );
        assert!(report.notes.is_empty(), "{:?}", report.notes);
        assert_round_trips(&report);
    }

    #[test]
    fn iterm_dark_variants_stand_in_and_other_spaces_are_noted() {
        let text = format!(
            "<plist><dict><key>Background Color (Dark)</key>{}\
             <key>Foreground Color</key>{}</dict></plist>",
            iterm_colour_dict(0x101010, "P3"),
            iterm_colour_dict(0xeeeeee, "sRGB"),
        );
        let report = convert(ImportFormat::ITerm, &text, None).unwrap();
        assert_eq!(report.scheme.background, rgb(0x101010));
        assert!(report.notes.iter().any(|n| n.contains("(Dark)")));
        assert!(report.notes.iter().any(|n| n.contains("\"P3\"")));
    }

    fn windows_terminal_scheme(name: &str) -> String {
        let names = [
            "black", "red", "green", "yellow", "blue", "purple", "cyan", "white",
        ];
        let mut text = format!(
            "{{\n  \"name\": \"{name}\",\n  \"foreground\": \"#C0CAF5\",\n  \
             \"background\": \"#1A1B26\",\n  \"cursorColor\": \"#c0caf5\",\n  \
             \"selectionBackground\": \"#283457\",\n"
        );
        for (index, colour_name) in names.iter().enumerate() {
            text.push_str(&format!(
                "  \"{colour_name}\": \"#{:06x}\",\n",
                PALETTE[index]
            ));
            let mut bright = String::from("bright");
            bright.push_str(&colour_name[..1].to_uppercase());
            bright.push_str(&colour_name[1..]);
            text.push_str(&format!(
                "  \"{bright}\": \"#{:06x}\",\n",
                PALETTE[index + 8]
            ));
        }
        text.push_str("  \"opacity\": 90\n}");
        text
    }

    #[test]
    fn windows_terminal_converts_structurally() {
        let report = convert(
            ImportFormat::WindowsTerminal,
            &windows_terminal_scheme("Tokyo Night"),
            None,
        )
        .unwrap();
        assert_eq!(
            report.scheme,
            expected(
                "Tokyo Night",
                0xc0caf5,
                0x1a1b26,
                Some(0xc0caf5),
                Some(0x283457),
                None,
                PALETTE
            )
        );
        assert_eq!(report.dropped, ["opacity"]);
        assert_round_trips(&report);
    }

    #[test]
    fn windows_terminal_settings_pick_a_scheme_by_name() {
        let text = format!(
            "// settings.json\n{{\n  \"profiles\": {{ \"list\": [] }},\n  /* schemes */\n  \
             \"schemes\": [\n{},\n{},\n],\n}}\n",
            windows_terminal_scheme("First"),
            windows_terminal_scheme("Second \\u00e9"),
        );
        let first = convert(ImportFormat::WindowsTerminal, &text, None).unwrap();
        assert_eq!(first.scheme.name, "First");
        assert!(first.notes.iter().any(|n| n.contains("2 schemes")));
        let second = convert(ImportFormat::WindowsTerminal, &text, Some("second é")).unwrap();
        assert_eq!(second.scheme.name, "Second é");
    }

    #[test]
    fn base16_converts_structurally() {
        let text = "scheme: \"Ocean\"\nauthor: \"Chris Kempson (http://chriskempson.com)\"\n\
                    base00: \"2b303b\"\nbase01: \"343d46\"\nbase02: \"4f5b66\"\n\
                    base03: \"65737e\"\nbase04: \"a7adba\"\nbase05: \"c0c5ce\"\n\
                    base06: \"dfe1e8\"\nbase07: \"eff1f5\"\nbase08: \"bf616a\"\n\
                    base09: \"d08770\"\nbase0A: \"ebcb8b\"\nbase0B: \"a3be8c\"\n\
                    base0C: \"96b5b4\"\nbase0D: \"8fa1b3\"\nbase0E: \"b48ead\"\n\
                    base0F: \"ab7967\" # brown\nextra: 1\n";
        let report = convert(ImportFormat::Base16, text, None).unwrap();
        let ansi = [
            0x2b303b, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x8fa1b3, 0xb48ead, 0x96b5b4, 0xc0c5ce,
            0x65737e, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x8fa1b3, 0xb48ead, 0x96b5b4, 0xeff1f5,
        ];
        assert_eq!(
            report.scheme,
            expected(
                "Ocean",
                0xc0c5ce,
                0x2b303b,
                Some(0xc0c5ce),
                Some(0x4f5b66),
                None,
                ansi
            )
        );
        assert_eq!(
            report.author.as_deref(),
            Some("Chris Kempson (http://chriskempson.com)")
        );
        assert_eq!(report.dropped, ["extra"]);
        assert!(!report.appearance_declared);
        assert_round_trips(&report);
    }

    #[test]
    fn tinted_base24_takes_its_brights_and_variant() {
        let mut text =
            String::from("system: \"base24\"\nname: \"Paper\"\nvariant: \"light\"\npalette:\n");
        for (index, name) in BASE_NAMES.iter().enumerate() {
            text.push_str(&format!(
                "  {name}: \"#{:06x}\"\n",
                0x101010 * (index as u32 % 15 + 1)
            ));
        }
        let report = convert(ImportFormat::Base16, &text, None).unwrap();
        let base = |i: u32| rgb(0x101010 * (i % 15 + 1));
        assert_eq!(report.scheme.appearance, Appearance::Light);
        assert!(report.appearance_declared);
        assert_eq!(report.scheme.ansi[9], base(0x12));
        assert_eq!(report.scheme.ansi[14], base(0x15));
        assert_eq!(report.scheme.ansi[1], base(0x08));
        assert!(report.dropped.is_empty(), "{:?}", report.dropped);
        assert_round_trips(&report);
    }

    #[test]
    fn unknown_keys_are_reported_not_fatal() {
        // Every format: a key aterm has no place for is listed and the rest
        // converts. The negative control is the same file without it.
        let cases: [(ImportFormat, &str, &str); 6] = [
            (
                ImportFormat::Kitty,
                "foreground #fff\nbackground #000\n",
                "frobnicate 1\n",
            ),
            (
                ImportFormat::Ghostty,
                "foreground = fff\nbackground = 000\n",
                "frobnicate = 1\n",
            ),
            (
                ImportFormat::Alacritty,
                "[colors.primary]\nforeground = '#fff'\nbackground = '#000'\n",
                "frobnicate = 1\n",
            ),
            (
                ImportFormat::WindowsTerminal,
                "{\"foreground\": \"#fff\", \"background\": \"#000\"",
                ", \"frobnicate\": 1",
            ),
            (
                ImportFormat::ITerm,
                "<plist><dict><key>Foreground Color</key><dict><key>Red Component</key><real>1</real><key>Green Component</key><real>1</real><key>Blue Component</key><real>1</real></dict><key>Background Color</key><dict><key>Red Component</key><real>0</real><key>Green Component</key><real>0</real><key>Blue Component</key><real>0</real></dict>",
                "<key>frobnicate</key><integer>1</integer>",
            ),
            (ImportFormat::Base16, "", "frobnicate: 1\n"),
        ];
        let base16_body: String = BASE_NAMES
            .iter()
            .take(16)
            .map(|name| format!("{name}: \"808080\"\n"))
            .collect();
        for (format, body, extra) in cases {
            let close = match format {
                ImportFormat::WindowsTerminal => "}",
                ImportFormat::ITerm => "</dict></plist>",
                _ => "",
            };
            let body = if format == ImportFormat::Base16 {
                base16_body.as_str()
            } else {
                body
            };
            let clean = convert(format, &format!("{body}{close}"), None).unwrap();
            assert!(clean.dropped.is_empty(), "{format:?}: {:?}", clean.dropped);
            let with = convert(format, &format!("{body}{extra}{close}"), None).unwrap();
            // Alacritty names a key by its table path (`colors.primary.frobnicate`).
            assert!(
                with.dropped.len() == 1 && with.dropped[0].ends_with("frobnicate"),
                "{format:?}: {:?}",
                with.dropped
            );
            assert_eq!(with.scheme, clean.scheme, "{format:?}");
        }
    }

    #[test]
    fn oversize_input_is_refused_before_it_is_read() {
        let mut bytes = kitty_sample().into_bytes();
        bytes.resize(MAX_IMPORT_BYTES + 1, b'\n');
        assert_eq!(
            import_theme(Some("big.conf"), &bytes, None),
            Err(ImportError::TooLarge {
                bytes: MAX_IMPORT_BYTES + 1
            })
        );
        // Negative control: the same file at exactly the bound converts.
        bytes.truncate(MAX_IMPORT_BYTES);
        assert!(import_theme(Some("big.conf"), &bytes, None).is_ok());
        // `convert` keeps the bound for callers that pass text directly.
        let text = "x".repeat(MAX_IMPORT_BYTES + 1);
        assert!(matches!(
            convert(ImportFormat::Kitty, &text, None),
            Err(ImportError::TooLarge { .. })
        ));
    }

    #[test]
    fn detection_follows_the_extension_then_the_content() {
        let kitty = kitty_sample();
        let ghostty = "background = #1a1b26\nforeground = #c0caf5\npalette = 0=#000000\n";
        let native = "foreground = #c0caf5\nbackground = #1a1b26\n";
        let alacritty = "[colors.primary]\nforeground = '#ffffff'\n";
        for (name, text, format) in [
            (Some("x.conf"), kitty.as_str(), ImportFormat::Kitty),
            (None, ghostty, ImportFormat::Ghostty),
            (Some("x.conf"), native, ImportFormat::Native),
            (Some("x"), alacritty, ImportFormat::Alacritty),
            (Some("x.toml"), native, ImportFormat::Alacritty),
            (Some("x.itermcolors"), "", ImportFormat::ITerm),
            (None, "<?xml version=\"1.0\"?><plist/>", ImportFormat::ITerm),
            (Some("X.JSON"), "", ImportFormat::WindowsTerminal),
            (
                None,
                "\u{feff}{\"background\": \"#000\"}",
                ImportFormat::WindowsTerminal,
            ),
            (Some("x.yaml"), "", ImportFormat::Base16),
            (
                None,
                "scheme: x\nbase00: \"000000\"\n",
                ImportFormat::Base16,
            ),
        ] {
            assert_eq!(detect_format(name, text), Some(format), "{name:?} {text:?}");
        }
        assert_eq!(detect_format(Some("notes.txt"), "just some prose\n"), None);
        assert_eq!(
            import_theme(Some("notes.txt"), b"just some prose\n", None),
            Err(ImportError::UnknownFormat)
        );
        assert_eq!(
            import_theme(Some("x.conf"), &[0xff, 0xfe, 0], None),
            Err(ImportError::NotUtf8)
        );
    }

    #[test]
    fn a_theme_without_foreground_or_background_fails() {
        assert_eq!(
            convert(ImportFormat::Kitty, "foreground #ffffff\n", None),
            Err(ImportError::MissingColour {
                format: ImportFormat::Kitty,
                key: "background"
            })
        );
        assert!(matches!(
            convert(ImportFormat::Base16, "base00: \"000000\"\n", None),
            Err(ImportError::MissingColour {
                format: ImportFormat::Base16,
                ..
            })
        ));
    }

    #[test]
    fn malformed_containers_fail_with_a_line() {
        for (format, text) in [
            (
                ImportFormat::WindowsTerminal,
                "{\n\"background\": \"#000\",\n\"x\": }",
            ),
            (
                ImportFormat::ITerm,
                "<plist>\n<dict>\n<key>x</key>\n<bogus/>",
            ),
            (
                ImportFormat::Alacritty,
                "[colors.primary\nforeground = '#fff'",
            ),
        ] {
            match convert(format, text, None) {
                Err(ImportError::Malformed { line, .. }) => assert!(line > 0, "{format:?}"),
                other => panic!("{format:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn nesting_is_bounded() {
        let deep = format!(
            "{}{}",
            "[".repeat(MAX_NESTING + 2),
            "]".repeat(MAX_NESTING + 2)
        );
        assert!(matches!(
            convert(ImportFormat::WindowsTerminal, &deep, None),
            Err(ImportError::Malformed { .. })
        ));
        let deep_plist = format!(
            "<plist>{}{}</plist>",
            "<array>".repeat(MAX_NESTING + 2),
            "</array>".repeat(MAX_NESTING + 2)
        );
        assert!(matches!(
            convert(ImportFormat::ITerm, &deep_plist, None),
            Err(ImportError::Malformed { .. })
        ));
    }

    #[test]
    fn unsafe_text_is_made_safe_and_still_round_trips() {
        let report = convert(
            ImportFormat::Kitty,
            &format!(
                "## name: Night #2 ; extra\u{1}\n## author: {}\nforeground #fff\nbackground #000\n",
                "x".repeat(MAX_THEME_TEXT_CHARS + 50)
            ),
            None,
        )
        .unwrap();
        assert_eq!(report.scheme.name, "Night#2; extra");
        assert_eq!(
            report.author.as_deref().map(|a| a.chars().count()),
            Some(MAX_THEME_TEXT_CHARS)
        );
        assert_round_trips(&report);
        for text in ["a #b", " #lead", "x\u{7}y", "  ", "#", "; a ;b"] {
            let once = sanitize_text(text);
            assert_eq!(once.as_deref().and_then(sanitize_text), once, "{text:?}");
        }
    }

    #[test]
    fn unreadable_colours_are_dropped_with_a_note() {
        let report = convert(
            ImportFormat::Ghostty,
            "foreground = #fff\nbackground = #000\ncursor-color = cell-foreground\n",
            None,
        )
        .unwrap();
        assert_eq!(report.scheme.cursor, None);
        assert_eq!(report.dropped, ["cursor-color"]);
        assert!(report.notes.iter().any(|n| n.contains("cell-foreground")));
        assert!(report.notes.iter().any(|n| n.contains("defaults fill")));
    }

    /// Every built-in written through the native serializer, plain or
    /// commented for a person, reads back as exactly the catalog's theme
    /// (colours, name, description and declared appearance) and imports as
    /// itself.
    #[test]
    fn every_builtin_round_trips_through_both_serializer_forms() {
        for name in crate::scheme::builtin_names() {
            let catalog = crate::scheme::CatalogTheme::builtin(name).expect("a built-in");
            let theme = &catalog.theme;
            for write in [to_native_conf, to_native_conf_commented] {
                let text = write(
                    &theme.scheme,
                    theme.appearance_declared,
                    theme.description.as_deref(),
                    theme.author.as_deref(),
                    &["a header"],
                );
                assert_eq!(
                    parse_theme_str(&text).as_ref(),
                    Ok(theme),
                    "{name}:\n{text}"
                );
                // A native file imports as itself, dropping nothing.
                let report = convert(ImportFormat::Native, &text, None).unwrap();
                assert_eq!(report.scheme, theme.scheme, "{name}");
                assert!(report.dropped.is_empty(), "{name}: {:?}", report.dropped);
            }
        }
    }

    /// The commented form writes every key the theme leaves out as a
    /// commented-out example, so uncommenting all of them still parses.
    #[test]
    fn commented_form_uncommented_parses() {
        let scheme = ColorScheme {
            cursor: None,
            selection: None,
            ..ColorScheme::default()
        };
        let text = to_native_conf_commented(&scheme, false, None, None, &[]);
        let plain = parse_theme_str(&text).expect("the commented form parses");
        assert_eq!(plain.scheme, scheme);
        assert!(!plain.appearance_declared);
        let uncommented: String = text
            .lines()
            .map(|line| {
                let key = line
                    .trim_start_matches("# ")
                    .split(" = ")
                    .next()
                    .unwrap_or("");
                if line.starts_with("# ") && crate::scheme::lookup_theme_key(key).is_some() {
                    line.trim_start_matches("# ").to_string()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let full = parse_theme_str(&uncommented).expect("uncommented examples parse");
        assert!(full.appearance_declared);
        assert!(full.scheme.cursor.is_some() && full.scheme.selection.is_some());
        assert!(full.description.is_some() && full.author.is_some());
    }
}
