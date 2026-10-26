// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm.toml` configuration model (`Config`), precedence resolvers, and [`App`] reload/apply methods.

use aterm_core::terminal::{CursorStyle, Rgb};
use aterm_render::Theme;
use aterm_window::dpi::PhysicalSize;

use crate::input::{InputEvent, Source};
use crate::platform::AppRt;
use crate::{
    App, Backend, FONT_PX, FONT_PX_MAX, FONT_PX_MIN, PresentTarget, WindowId, keybinding, launch,
};

/// Platform default for decoration drawn over content.
pub(crate) const DEFAULT_DECORATIVE_EFFECTS: bool = !cfg!(windows);

/// The cursor style an aterm.toml that names none draws.
pub(crate) const fn default_cursor_trail_style() -> &'static str {
    if DEFAULT_DECORATIVE_EFFECTS {
        crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE
    } else {
        "off"
    }
}

/// `toml` with the cursor trail drawn on every platform: where decorative
/// effects are off by default (Windows), the default style and its momentum
/// glow go ahead of it unless it names them.
#[cfg(test)]
pub(crate) fn trail_drawn(toml: &str) -> String {
    if DEFAULT_DECORATIVE_EFFECTS {
        return toml.to_string();
    }
    let mut out = String::new();
    if !toml.contains("cursor_trail_style =") {
        out.push_str(&format!(
            "cursor_trail_style = \"{}\"\n",
            crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE
        ));
    }
    if !toml.contains("cursor_momentum_glow =") {
        out.push_str("cursor_momentum_glow = true\n");
    }
    out + toml
}

/// The cursor style an aterm.toml draws from its `cursor_trail_style` (`style`) and the retired `cursor_trail` master (`master`), trimmed.
pub(crate) fn drawn_cursor_trail_style(master: Option<bool>, style: Option<&str>) -> &str {
    if master == Some(false) {
        return "off";
    }
    match style {
        Some(style) => style.trim(),
        None if master == Some(true) => crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
        None => default_cursor_trail_style(),
    }
}

/// Platform default for `line_height`, the cell-box multiplier.
pub(crate) const DEFAULT_LINE_HEIGHT: f32 = if cfg!(windows) { 1.1 } else { 1.0 };

/// Platform default for the theme-pair contrast floor while `minimum_contrast` is unset ([`Config::indexed_minimum_contrast_or_default`]).
pub(crate) const DEFAULT_INDEXED_MINIMUM_CONTRAST: f32 = if cfg!(windows) { 4.5 } else { 1.0 };

/// Whether panes show the overlay scrollbar when `scrollbar` is unset.
pub(crate) const DEFAULT_SCROLLBAR: bool = cfg!(windows);

/// User config file (`$XDG_CONFIG_HOME/aterm/aterm.toml`, else `~/.config/aterm/aterm.toml`).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct Config {
    pub(crate) font_px: Option<f32>,
    pub(crate) gpu: Option<bool>,
    pub(crate) scrollback_lines: Option<usize>,
    pub(crate) cursor_style: Option<String>,
    pub(crate) cursor_blink: Option<bool>,
    pub(crate) foreground: Option<String>,
    pub(crate) background: Option<String>,
    pub(crate) cursor_color: Option<String>,
    pub(crate) selection_color: Option<String>,
    pub(crate) palette: Option<Vec<String>>,
    pub(crate) palette_overrides: Option<std::collections::BTreeMap<String, String>>,
    pub(crate) motion: Option<String>,
    pub(crate) load_adaptive_motion: Option<bool>,
    pub(crate) serious_mode: Option<bool>,
    pub(crate) cursor_trail: Option<bool>,
    pub(crate) cursor_momentum_glow: Option<bool>,
    pub(crate) cursor_trail_style: Option<String>,
    pub(crate) trail_sounds: Option<bool>,
    pub(crate) trail_sound_volume: Option<f32>,
    pub(crate) trail_sound_bed: Option<bool>,
    pub(crate) trail_sound_style: Option<String>,
    pub(crate) tone_melody: Option<bool>,
    pub(crate) trail_sound_riff: Option<bool>,
    pub(crate) bell_sound: Option<bool>,
    pub(crate) cursor_trail_color: Option<String>,
    pub(crate) cursor_trail_accent: Option<String>,
    pub(crate) cursor_nyan_sprite: Option<String>,
    pub(crate) wallpaper: Option<String>,
    pub(crate) wallpaper_dim: Option<f32>,
    pub(crate) wallpaper_text_tint: Option<bool>,
    pub(crate) cursor_trail_ms: Option<u64>,
    pub(crate) cursor_trail_length: Option<usize>,
    pub(crate) cursor_trail_intensity: Option<f32>,
    pub(crate) cursor_trail_radius: Option<f32>,
    pub(crate) cursor_trail_ring: Option<bool>,
    pub(crate) cursor_trail_bloom: Option<bool>,
    pub(crate) cursor_trail_bloom_strength: Option<f32>,
    pub(crate) cursor_trail_bloom_radius: Option<f32>,
    pub(crate) cursor_fire_shimmer: Option<bool>,
    pub(crate) hdr_glow: Option<bool>,
    pub(crate) cursor_glow_sdr_boost: Option<f32>,
    pub(crate) predictive_echo: Option<String>,
    pub(crate) predictive_echo_editing: Option<bool>,
    pub(crate) focus_boost: Option<bool>,
    pub(crate) theme: Option<String>,
    pub(crate) shell: Option<String>,
    pub(crate) shell_args: Option<Vec<String>>,
    pub(crate) conpty: Option<String>,
    pub(crate) columns: Option<u16>,
    pub(crate) lines: Option<u16>,
    pub(crate) search_history_lines: Option<u32>,
    pub(crate) font_family: Option<String>,
    pub(crate) display_font: Option<String>,
    pub(crate) font_family_bold: Option<String>,
    pub(crate) font_family_italic: Option<String>,
    pub(crate) font_family_bold_italic: Option<String>,
    pub(crate) font_synthetic_style: Option<bool>,
    pub(crate) fallback_fonts: Option<FontList>,
    pub(crate) symbol_font: Option<String>,
    pub(crate) emoji_font: Option<String>,
    pub(crate) window_theme: Option<String>,
    pub(crate) window_colorspace: Option<String>,
    pub(crate) option_as_meta: Option<bool>,
    pub(crate) confirm_multiline_paste: Option<bool>,
    pub(crate) restore_session: Option<bool>,
    pub(crate) copy_on_select: Option<bool>,
    pub(crate) right_click: Option<String>,
    pub(crate) tab_menu_chord: Option<String>,
    pub(crate) windowing_behavior: Option<String>,
    pub(crate) show_build_badge: Option<bool>,
    pub(crate) keybindings: Option<std::collections::BTreeMap<String, String>>,
    pub(crate) key_sequences: Option<std::collections::BTreeMap<String, String>>,
    pub(crate) tab_strip_rows: Option<u16>,
    pub(crate) tab_band_height: Option<String>,
    pub(crate) active_tab_color: Option<String>,
    pub(crate) accent_color: Option<String>,
    pub(crate) border_color: Option<String>,
    pub(crate) search_match_color: Option<String>,
    pub(crate) tab_status: Option<bool>,
    pub(crate) tab_status_quiet_after_ms: Option<u64>,
    pub(crate) tab_status_dwell_ms: Option<u64>,
    pub(crate) spare_shell: Option<String>,
    pub(crate) spare_shell_needs_person: Option<bool>,
    pub(crate) tab_status_badge: Option<bool>,
    pub(crate) tab_connection_badge: Option<bool>,
    pub(crate) desktop_alerts: Option<bool>,
    pub(crate) agents_auto_prime: Option<bool>,
    pub(crate) bidi: Option<String>,
    pub(crate) ambiguous_width: Option<String>,
    pub(crate) ligatures: Option<bool>,
    pub(crate) merged_ligatures: Option<bool>,
    pub(crate) font_features: Option<Vec<String>>,
    pub(crate) text_blending: Option<String>,
    pub(crate) font_thicken: Option<bool>,
    pub(crate) font_variation: Option<FontList>,
    pub(crate) font_weight: Option<u32>,
    pub(crate) font_weight_dark_nudge: Option<f32>,
    pub(crate) stem_gamma: Option<f32>,
    pub(crate) font_hinting: Option<String>,
    pub(crate) font_subpixel: Option<String>,
    pub(crate) line_height: Option<f32>,
    pub(crate) font_size_scale: Option<f32>,
    pub(crate) adjust_baseline: Option<i64>,
    pub(crate) adjust_underline_position: Option<i64>,
    pub(crate) adjust_underline_thickness: Option<i64>,
    pub(crate) underline_skip_descenders: Option<bool>,
    pub(crate) minimum_contrast: Option<f32>,
    pub(crate) selection_foreground: Option<String>,
    pub(crate) selection_inactive: Option<bool>,
    pub(crate) split_focus_mark: Option<bool>,
    pub(crate) scrollbar: Option<bool>,
    pub(crate) cursor_break_ligatures: Option<bool>,
    pub(crate) bold_is_bright: Option<bool>,
    pub(crate) palette_generate: Option<String>,
    pub(crate) faint_opacity: Option<f32>,
    pub(crate) background_opacity: Option<f32>,
    pub(crate) background_material: Option<String>,
    pub(crate) window_padding: Option<f32>,
    pub(crate) window_padding_top: Option<f32>,
    pub(crate) allow_osc52_query: Option<bool>,
    pub(crate) secure_keyboard_entry: Option<bool>,
    pub(crate) allow_window_ops: Option<bool>,
    pub(crate) allow_notifications: Option<bool>,
    pub(crate) allow_palette_reconfigure: Option<bool>,
    pub(crate) allow_kitty_file_transfer: Option<bool>,
    pub(crate) temporal_recording: Option<bool>,
    pub(crate) fabric: Option<FabricConfig>,
    pub(crate) fleet: Option<FleetConfig>,
    pub(crate) update: Option<UpdateConfig>,
    pub(crate) sparkle_words: Option<SparkleWordsConfig>,
    pub(crate) matrix_rain: Option<MatrixRainConfig>,
    pub(crate) presence: Option<PresenceConfig>,
    pub(crate) claude_footer: Option<ClaudeFooterConfig>,
    pub(crate) render: Option<RenderConfig>,
    pub(crate) output_streak: Option<OutputStreakConfig>,
    pub(crate) packages: Option<PackagesConfig>,
    pub(crate) reroute: Option<RerouteConfig>,
    pub(crate) privacy: Option<PrivacyConfig>,
    pub(crate) machine: Option<MachineConfig>,
    pub(crate) sys: Option<SysConfig>,
    pub(crate) keeper: Option<KeeperConfig>,
    pub(crate) session_log: Option<SessionLogConfig>,
    #[serde(skip)]
    pub(crate) harness: HarnessPolicy,
    #[serde(skip)]
    pub(crate) ai: AiPolicy,
}

/// Tab status bounds, shared with `prefs` so a Settings save never writes a value the resolver would clamp.
pub(crate) const DEFAULT_TAB_STATUS_QUIET_AFTER_MS: u64 = 5_000;
pub(crate) const MIN_TAB_STATUS_QUIET_AFTER_MS: u64 = 500;
pub(crate) const MAX_TAB_STATUS_QUIET_AFTER_MS: u64 = 120_000;
pub(crate) const DEFAULT_TAB_STATUS_DWELL_MS: u64 = 750;
pub(crate) const MIN_TAB_STATUS_DWELL_MS: u64 = 0;
pub(crate) const MAX_TAB_STATUS_DWELL_MS: u64 = 10_000;
const NOMINAL_TAB_STATUS_OBSERVE_INTERVAL_MS: u64 = 250;
const MIN_TAB_STATUS_OBSERVE_INTERVAL_MS: u64 = 50;

/// Pack an [`Rgb`] as the renderer's `0x00RRGGBB`.
pub(crate) fn rgb_u32(c: Rgb) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

/// An ordered font list (`fallback_fonts`, `font_variation`) that deserializes from either a TOML array of strings (`["A", "B"]`) or one comma-separated string (`"A, B"`, the form the Settings text editor writes, so a Save round-trips through serde).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct FontList(pub(crate) Vec<String>);

impl<'de> serde::Deserialize<'de> for FontList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = FontList;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a list of font names/paths, or a comma-separated string")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<FontList, E> {
                Ok(FontList(
                    s.split(',')
                        .map(str::trim)
                        .filter(|e| !e.is_empty())
                        .map(str::to_string)
                        .collect(),
                ))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<FontList, A::Error> {
                let mut out = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    let t = s.trim();
                    if !t.is_empty() {
                        out.push(t.to_string());
                    }
                }
                Ok(FontList(out))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// The `[sparkle_words]` table.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleWordsConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) languages: Option<Vec<String>>,
    pub(crate) reduced_motion: Option<bool>,
    pub(crate) suppress_in_alt_screen: Option<bool>,
    pub(crate) lexicon: Option<String>,
    pub(crate) toy_packs: Option<Vec<String>>,
    pub(crate) deny: Option<Vec<String>>,
    pub(crate) profanity: Option<SparkleProfanityConfig>,
    pub(crate) feline: Option<SparkleFelineConfig>,
    pub(crate) canine: Option<SparkleCanineConfig>,
    pub(crate) tricks: Option<SparkleTricksConfig>,
    pub(crate) ink: Option<SparkleInkConfig>,
    pub(crate) emphasis: Option<SparkleEmphasisConfig>,
    pub(crate) custom: Option<Vec<aterm_effects::spec::RawCustomEntry>>,
}

const MAX_ACTIVE_TOY_PACKS: usize = 8;

#[cfg(test)]
const MAX_PATH_FEED_OPEN_ATTEMPTS: usize = 1 + MAX_ACTIVE_TOY_PACKS;
#[cfg(test)]
const MAX_PATH_FEED_FINGERPRINT_READ_BYTES: usize =
    (aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES + 1)
        + MAX_ACTIVE_TOY_PACKS * (aterm_effects::spec::MAX_TOY_PACK_BYTES + 1);

#[derive(Default)]
struct LoadedToyPacks {
    spec_table: aterm_effects::spec::SpecTable,
    lexicon_toml: String,
}

type AdmittedPathFeed = (std::path::PathBuf, Result<String, String>);

fn read_and_fingerprint_path_feed(
    path: &str,
    max_bytes: usize,
    hasher: &mut std::collections::hash_map::DefaultHasher,
) -> AdmittedPathFeed {
    read_and_fingerprint_path_feed_with_reader(
        path,
        max_bytes,
        hasher,
        aterm_effects::file_feed::read_bounded_regular_utf8,
    )
}

fn read_and_fingerprint_path_feed_with_reader(
    path: &str,
    max_bytes: usize,
    hasher: &mut std::collections::hash_map::DefaultHasher,
    read: impl FnOnce(&std::path::Path, usize) -> std::io::Result<String>,
) -> AdmittedPathFeed {
    use std::hash::Hash as _;

    #[cfg(test)]
    PATH_FEED_READS.with(|count| count.set(count.get().saturating_add(1)));
    path.hash(hasher);
    let expanded = sparkle_expand_tilde(path);
    match read(&expanded, max_bytes) {
        Ok(contents) => {
            true.hash(hasher);
            aterm_effects::file_feed::fingerprint_admitted_utf8(&contents).hash(hasher);
            (expanded, Ok(contents))
        }
        Err(error) => {
            false.hash(hasher);
            (expanded, Err(error.to_string()))
        }
    }
}

/// Word-decoration runtime compiled for one admitted config generation.
#[derive(Clone)]
pub(crate) struct PreparedSparkleRuntime {
    resolved: Option<crate::word_decorations::Resolved>,
    tricks: TrickVocabulary,
}

#[derive(Clone)]
struct TrickVocabulary(std::sync::Arc<std::sync::OnceLock<aterm_lexicon::TrickLexicon>>);

impl TrickVocabulary {
    fn compiled(langs: Vec<String>) -> Self {
        let vocabulary = Self(std::sync::Arc::default());
        let _ = vocabulary.0.set(compile_trick_vocabulary(&langs));
        vocabulary
    }

    fn compiling(langs: Vec<String>) -> Self {
        let vocabulary = Self(std::sync::Arc::default());
        let landing = std::sync::Arc::clone(&vocabulary.0);
        let worker_langs = langs.clone();
        let spawned = std::thread::Builder::new()
            .name("aterm-tricks-compile".into())
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                #[cfg(test)]
                let _held = TRICKS_COMPILE_GATE
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = landing.set(compile_trick_vocabulary(&worker_langs));
            });
        if spawned.is_err() {
            let _ = vocabulary.0.set(compile_trick_vocabulary(&langs));
        }
        vocabulary
    }

    fn get(&self) -> &aterm_lexicon::TrickLexicon {
        static EMPTY: std::sync::OnceLock<aterm_lexicon::TrickLexicon> = std::sync::OnceLock::new();
        self.0
            .get()
            .unwrap_or_else(|| EMPTY.get_or_init(aterm_lexicon::TrickLexicon::default))
    }
}

#[cfg(test)]
static TRICKS_COMPILE_GATE: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn compile_trick_vocabulary(langs: &[String]) -> aterm_lexicon::TrickLexicon {
    let refs: Vec<&str> = langs.iter().map(String::as_str).collect();
    let tricks = aterm_lexicon::TrickLexicon::with_languages(&refs);
    for conflict in tricks.conflicts() {
        crate::logging::stderr_line!("aterm-gui: kitty commands vocabulary: {conflict}");
    }
    tricks
}

/// One consistent generation of every path-backed effect feed.
#[derive(Clone)]
pub(crate) struct PreparedPathFeedGeneration {
    pub(crate) sparkle: PreparedSparkleRuntime,
    pub(crate) fingerprints: PathFeedFps,
}

impl PreparedSparkleRuntime {
    /// This generation's kitty-command vocabulary, borrowed for one press.
    pub(crate) fn tricks(&self) -> &aterm_lexicon::TrickLexicon {
        self.tricks.get()
    }

    /// Custom-spec consumers for this generation (valid Toy Packs, then inline overrides), so Settings can disclose dependencies without reopening manifests.
    pub(crate) fn consumer_capabilities(&self) -> aterm_effects::spec::SpecConsumerCapabilities {
        let Some(resolved) = self.resolved.as_ref() else {
            return Default::default();
        };
        let mut consumers = resolved.cfg.spec_table.consumer_capabilities();
        let opts = aterm_lexicon::ScanOptions {
            allow_bare_cat: resolved.cfg.allow_bare_cat,
            cjk_single_char: resolved.cfg.cjk_single_char,
            ignore: (!resolved.cfg.ignore.is_empty()).then_some(&resolved.cfg.ignore),
        };
        consumers.emphasis_class_default = resolved.lexicon.has_scannable_class_surface(
            aterm_lexicon::Class::Emphasis,
            &opts,
            |form_hash| resolved.cfg.spec_table.override_for(form_hash).is_some(),
        );
        consumers
    }
}

#[cfg(test)]
thread_local! {
    static SPARKLE_HOST_PREPARES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PATH_FEED_FINGERPRINTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PATH_FEED_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_sparkle_host_prepare_count() {
    SPARKLE_HOST_PREPARES.with(|count| count.set(0));
}

#[cfg(test)]
fn sparkle_host_prepare_count() -> usize {
    SPARKLE_HOST_PREPARES.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_path_feed_fingerprint_count() {
    PATH_FEED_FINGERPRINTS.with(|count| count.set(0));
}

#[cfg(test)]
fn path_feed_fingerprint_count() -> usize {
    PATH_FEED_FINGERPRINTS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_path_feed_read_count() {
    PATH_FEED_READS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn path_feed_read_count() -> usize {
    PATH_FEED_READS.with(std::cell::Cell::get)
}

/// The trail presentation resolved once per config generation, shared by `Arc` between the live host and Settings so redraw does not rescan the style vocabulary.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrailStyleCache {
    resolved_style_raw: Box<str>,
    resolved_presentation: ResolvedTrailPresentation,
}

/// Maximum encoded PNG bytes for a custom kitty cursor sprite.
pub(crate) const MAX_KITTY_SPRITE_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum width or height of a decoded custom cursor sprite.
pub(crate) const MAX_KITTY_SPRITE_DIMENSION: usize = 1024;
const MAX_KITTY_SOURCE_ID_BYTES: usize = 2 * 1024;
const MAX_KITTY_REASON_BYTES: usize = 320;

/// Bounded read for the configured wallpaper image file (a photo is an ordinary source, so this is far above the kitty sprite's 8 MiB).
pub(crate) const MAX_WALLPAPER_FILE_BYTES: usize = 64 * 1024 * 1024;
/// Reject a wallpaper whose decoded dims exceed this per side (the allocation bound; a 5K/6K screenshot fits comfortably).
pub(crate) const MAX_WALLPAPER_SOURCE_DIMENSION: usize = 8192;
/// Downscale an admitted wallpaper so neither side exceeds this (the resident memory bound — still larger than any window it will be cover-scaled to).
pub(crate) const MAX_WALLPAPER_KEEP_DIMENSION: usize = 4096;

/// Installed-theme discovery keeps a bounded, deterministic prefix and reads every file under a hard byte limit.
pub(crate) const MAX_USER_THEME_FILES: usize = aterm_types::scheme::MAX_USER_THEME_FILES;
/// `.conf` candidates kept, sorted, from the folder scan (the lowest names win).
pub(crate) const MAX_USER_THEME_CANDIDATES: usize = 512;
/// Directory entries inspected before enumeration stops.
pub(crate) const MAX_USER_THEME_DIRECTORY_ENTRIES: usize = 4_096;
pub(crate) const MAX_USER_THEME_FILE_BYTES: usize = aterm_types::scheme::MAX_USER_THEME_FILE_BYTES;

/// The three bounds one theme-folder scan keeps.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ThemeScanCaps {
    pub(crate) entries: usize,
    pub(crate) candidates: usize,
    pub(crate) files: usize,
}

impl ThemeScanCaps {
    pub(crate) const PRODUCTION: Self = Self {
        entries: MAX_USER_THEME_DIRECTORY_ENTRIES,
        candidates: MAX_USER_THEME_CANDIDATES,
        files: MAX_USER_THEME_FILES,
    };
}
const MAX_THEME_SOURCE_ID_BYTES: usize = 2 * 1024;
const MAX_THEME_REASON_BYTES: usize = 320;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ThemeAssetResolution {
    Ready(Box<aterm_types::scheme::ThemePackage>),
    Invalid(std::sync::Arc<str>),
    Shadowed(&'static str),
    Unreadable,
}

/// Admit a parsed theme file whose id (file stem) is `id`.
pub(crate) fn pack_package(
    name: &str,
) -> Option<(&'static str, &'static aterm_types::scheme::ThemePackage)> {
    static PACK: std::sync::OnceLock<
        Box<[std::sync::OnceLock<Option<aterm_types::scheme::ThemePackage>>]>,
    > = std::sync::OnceLock::new();
    let themes = aterm_theme_pack::themes();
    let slots = PACK.get_or_init(|| themes.iter().map(|_| std::sync::OnceLock::new()).collect());
    let index = themes
        .iter()
        .position(|theme| theme.id.eq_ignore_ascii_case(name))?;
    let theme = &themes[index];
    slots[index]
        .get_or_init(|| {
            aterm_types::scheme::parse_theme_str(theme.text)
                .ok()
                .map(|parsed| admit_theme_file(parsed, theme.id))
        })
        .as_ref()
        .map(|package| (theme.id, package))
}

fn admit_theme_file(
    parsed: aterm_types::scheme::ParsedTheme,
    id: &str,
) -> aterm_types::scheme::ThemePackage {
    let measured = measured_appearance(parsed.scheme.background);
    aterm_types::scheme::ThemePackage::admit(parsed, id, measured)
}

/// A loaded theme file's label wherever it is listed (the command palette's Theme section, Settings' theme pickers).
pub(crate) fn theme_file_label(id: &str, meta: &aterm_types::scheme::ThemeMeta) -> String {
    if meta.display_name == id {
        id.to_string()
    } else {
        format!(
            "{}{}{id}",
            meta.display_name,
            crate::native_ui::FACT_SEPARATOR
        )
    }
}

/// The partial-apply reason for a loaded theme from a newer schema.
pub(crate) fn skipped_keys_reason(meta: &aterm_types::scheme::ThemeMeta) -> Option<String> {
    if meta.skipped_keys.is_empty() {
        return None;
    }
    let count = meta
        .warnings
        .iter()
        .find_map(|warning| match warning {
            aterm_types::scheme::ThemeWarning::SkippedNewerKeys { count, .. } => Some(*count),
            _ => None,
        })
        .map_or(meta.skipped_keys.len(), usize::from);
    let reason = format!(
        "some settings need a newer aterm: {count} key(s) skipped ({})",
        meta.skipped_keys.join(", ")
    );
    Some(bounded_asset_text(&reason, MAX_THEME_REASON_BYTES).to_string())
}

/// The appearance a background measures as, by the one dark/light rule ([`aterm_messages::ink::bg_is_light`]).
pub(crate) fn measured_appearance(bg: aterm_types::Rgb) -> aterm_types::Appearance {
    if aterm_messages::ink::bg_is_light([bg.r, bg.g, bg.b]) {
        aterm_types::Appearance::Light
    } else {
        aterm_types::Appearance::Dark
    }
}

/// Why one custom-theme row did or did not load, as [`ThemeCatalog::listing`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeEntryStatus<'a> {
    Ready,
    /// Read but refused (syntax, size, unsafe name, case collision).
    Invalid(&'a str),
    /// Named like this built-in, which always wins.
    Shadowed(&'static str),
    /// A file name that is not UTF-8, listed under its lossy spelling.
    Unreadable,
}

impl ThemeEntryStatus<'_> {
    /// The one-line reason a non-ready row did not load.
    pub(crate) fn reason(self) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::Invalid(reason) => Some(reason.to_string()),
            Self::Shadowed(builtin) => Some(format!(
                "shadowed by the built-in theme {builtin}, which always wins; rename the file \
                 (e.g. \"{builtin} (mine).conf\")"
            )),
            Self::Unreadable => {
                Some("the file name is not valid UTF-8, so no config can name it; rename it".into())
            }
        }
    }
}

/// How much of the themes folder discovery looked at and kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ThemeCatalogLimits {
    pub(crate) seen: usize,
    pub(crate) kept: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThemeAsset {
    name: String,
    source_id: std::sync::Arc<str>,
    fingerprint: u64,
    resolution: ThemeAssetResolution,
}

/// Parsed custom themes (files only; built-ins live in `aterm-types`) for one config generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ThemeCatalog {
    entries: Vec<ThemeAsset>,
    truncated: bool,
    limits: ThemeCatalogLimits,
}

/// Live watcher failures that keep the last admitted catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeCatalogWatchError {
    DirectoryUnreadable,
    EntryUnreadable,
    FileUnavailable,
}

impl ThemeCatalog {
    pub(crate) fn empty() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    #[cfg(test)]
    pub(crate) fn from_schemes(
        schemes: impl IntoIterator<Item = (String, aterm_types::ColorScheme)>,
    ) -> std::sync::Arc<Self> {
        Self::from_parsed(schemes.into_iter().map(|(name, scheme)| {
            (
                name,
                aterm_types::scheme::ParsedTheme::colours_only(scheme, true, None, None),
            )
        }))
    }

    #[cfg(test)]
    pub(crate) fn from_parsed(
        themes: impl IntoIterator<Item = (String, aterm_types::scheme::ParsedTheme)>,
    ) -> std::sync::Arc<Self> {
        Self::from_packages(themes.into_iter().map(|(name, parsed)| {
            let package = admit_theme_file(parsed, &name);
            (name, package)
        }))
    }

    #[cfg(test)]
    pub(crate) fn from_packages(
        packages: impl IntoIterator<Item = (String, aterm_types::scheme::ThemePackage)>,
    ) -> std::sync::Arc<Self> {
        let entries = packages
            .into_iter()
            .map(|(name, package)| ThemeAsset {
                fingerprint: stable_asset_fingerprint(0x54, name.as_bytes(), 0, 0, &[]),
                source_id: std::sync::Arc::from(name.clone()),
                resolution: ThemeAssetResolution::Ready(Box::new(package)),
                name,
            })
            .collect::<Vec<_>>();
        let kept = entries.len();
        std::sync::Arc::new(Self {
            entries: normalize_theme_entries(entries),
            truncated: false,
            limits: ThemeCatalogLimits { seen: kept, kept },
        })
    }

    /// Scan and parse the user theme directory once on the cold startup path.
    pub(crate) fn discover() -> std::sync::Arc<Self> {
        let Some(directory) = aterm_types::scheme::user_theme_dir() else {
            return Self::empty();
        };
        std::sync::Arc::new(Self::discover_in(&directory))
    }

    pub(crate) fn discover_in(directory: &std::path::Path) -> Self {
        Self::discover_in_with(directory, false, || {}, ThemeScanCaps::PRODUCTION)
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn discover_in_capped(
        directory: &std::path::Path,
        live: bool,
        caps: ThemeScanCaps,
    ) -> Result<Self, ThemeCatalogWatchError> {
        Self::discover_in_with(directory, live, || {}, caps)
    }

    /// Fallible discovery for the live watcher.
    pub(crate) fn try_discover_in(
        directory: &std::path::Path,
    ) -> Result<Self, ThemeCatalogWatchError> {
        Self::discover_in_with(directory, true, || {}, ThemeScanCaps::PRODUCTION)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn try_discover_in_after_scan(
        directory: &std::path::Path,
        after_scan: impl FnOnce(),
    ) -> Result<Self, ThemeCatalogWatchError> {
        Self::discover_in_with(directory, true, after_scan, ThemeScanCaps::PRODUCTION)
    }

    fn discover_in_with(
        directory: &std::path::Path,
        retain_on_file_failure: bool,
        after_scan: impl FnOnce(),
        caps: ThemeScanCaps,
    ) -> Result<Self, ThemeCatalogWatchError> {
        let mut candidates = std::collections::BTreeSet::new();
        let mut links = std::collections::BTreeSet::new();
        let mut truncated = false;
        let mut seen = 0usize;
        let read_dir = match std::fs::read_dir(directory) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(_) => return Err(ThemeCatalogWatchError::DirectoryUnreadable),
        };
        for (index, result) in read_dir.take(caps.entries + 1).enumerate() {
            // Past the entry cap the scan stops but keeps what it gathered:
            // a folder full of strays degrades to a bounded prefix, never to
            // an empty catalog.
            if index == caps.entries {
                truncated = true;
                break;
            }
            let entry = match result {
                Ok(entry) => entry,
                Err(_) if retain_on_file_failure => {
                    return Err(ThemeCatalogWatchError::EntryUnreadable);
                }
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) if retain_on_file_failure => {
                    return Err(ThemeCatalogWatchError::EntryUnreadable);
                }
                Err(_) => continue,
            };
            if entry.path().extension() != Some(std::ffi::OsStr::new("conf")) {
                continue;
            }
            if file_type.is_symlink() {
                links.insert(entry.path());
            } else if !file_type.is_file() {
                continue;
            }
            seen = seen.saturating_add(1);
            candidates.insert(entry.path());
            if candidates.len() > caps.candidates {
                candidates.pop_last();
                truncated = true;
            }
        }
        after_scan();

        let mut entries = Vec::new();
        let mut kept = 0usize;
        for path in candidates {
            let source_id = bounded_asset_text(&path.to_string_lossy(), MAX_THEME_SOURCE_ID_BYTES);
            let Some(stem) = path.file_stem() else {
                continue;
            };
            let Some(name) = stem.to_str() else {
                entries.push(diagnostic_theme_asset(
                    &stem.to_string_lossy(),
                    &source_id,
                    ThemeAssetResolution::Unreadable,
                ));
                continue;
            };
            if let Some(builtin) = builtin_theme_name(name) {
                entries.push(diagnostic_theme_asset(
                    name,
                    &source_id,
                    ThemeAssetResolution::Shadowed(builtin),
                ));
                continue;
            }
            if let Err(reason) = safe_user_theme_name(name) {
                entries.push(invalid_theme_asset(name, &source_id, reason));
                continue;
            }
            if links.contains(&path) {
                entries.push(invalid_theme_asset(
                    name,
                    &source_id,
                    "symlinks are not followed; copy the file into the themes folder",
                ));
                continue;
            }
            if kept == caps.files {
                truncated = true;
                break;
            }
            kept += 1;

            let bytes = match read_bounded_theme_file(&path) {
                Ok(bytes) => bytes,
                Err(ThemeFileReadError::Unavailable(reason)) if retain_on_file_failure => {
                    let _ = reason;
                    return Err(ThemeCatalogWatchError::FileUnavailable);
                }
                Err(
                    ThemeFileReadError::Unavailable(reason) | ThemeFileReadError::Invalid(reason),
                ) => {
                    entries.push(invalid_theme_asset(name, &source_id, &reason));
                    continue;
                }
            };
            let text = match std::str::from_utf8(&bytes) {
                Ok(text) => text,
                Err(_) => {
                    entries.push(invalid_theme_asset(
                        name,
                        &source_id,
                        "theme file is not valid UTF-8",
                    ));
                    continue;
                }
            };
            match aterm_types::scheme::parse_theme_str(text) {
                Ok(parsed) => entries.push(ThemeAsset {
                    name: name.to_string(),
                    source_id,
                    fingerprint: stable_asset_fingerprint(
                        0x54,
                        path.to_string_lossy().as_bytes(),
                        0,
                        0,
                        &bytes,
                    ),
                    resolution: ThemeAssetResolution::Ready(Box::new(admit_theme_file(
                        parsed, name,
                    ))),
                }),
                Err(error) => {
                    entries.push(invalid_theme_asset(name, &source_id, &error.to_string()))
                }
            }
        }
        Ok(Self {
            entries: normalize_theme_entries(entries),
            truncated,
            limits: ThemeCatalogLimits { seen, kept },
        })
    }

    pub(crate) fn ready_names(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.resolution {
                ThemeAssetResolution::Ready(_) => Some(entry.name.as_str()),
                ThemeAssetResolution::Invalid(_)
                | ThemeAssetResolution::Shadowed(_)
                | ThemeAssetResolution::Unreadable => None,
            })
    }

    /// Each loaded custom theme's id (file stem, what config writes) with its identity.
    pub(crate) fn ready_themes(
        &self,
    ) -> impl Iterator<Item = (&str, &aterm_types::scheme::ThemeMeta)> {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.resolution {
                ThemeAssetResolution::Ready(theme) => Some((entry.name.as_str(), &theme.meta)),
                ThemeAssetResolution::Invalid(_)
                | ThemeAssetResolution::Shadowed(_)
                | ThemeAssetResolution::Unreadable => None,
            })
    }

    /// The identity of the theme `id` names, by the same lookup as [`Self::resolve`].
    pub(crate) fn meta(&self, id: &str) -> Option<aterm_types::scheme::ThemeMeta> {
        if let Some(builtin) = aterm_types::scheme::builtin_theme(id) {
            return Some(aterm_types::scheme::ThemeMeta::of_builtin(builtin));
        }
        self.ready_themes()
            .find(|(name, _)| name.eq_ignore_ascii_case(id))
            .map(|(_, meta)| meta.clone())
    }

    /// Where the loaded custom theme `id` was read from (its file path, bounded as discovery recorded it), by the lookup [`Self::resolve`] uses for files.
    pub(crate) fn source_path(&self, id: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| {
                matches!(entry.resolution, ThemeAssetResolution::Ready(_))
                    && entry.name.eq_ignore_ascii_case(id)
            })
            .map(|entry| &*entry.source_id)
    }

    /// Whether discovery kept a row named `name` (ignoring case), loaded or not.
    pub(crate) fn lists(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    /// The community pack's theme `name`, drawn from the embedded pack, where the folder has no file by that name at all.
    pub(crate) fn pack_fallback(
        &self,
        name: &str,
    ) -> Option<(&'static str, &'static aterm_types::scheme::ThemePackage)> {
        if self.lists(name) {
            return None;
        }
        pack_package(name)
    }

    /// Every custom theme file discovery kept a row for, by name, with its status.
    pub(crate) fn listing(&self) -> impl Iterator<Item = (&str, ThemeEntryStatus<'_>)> {
        self.entries.iter().map(|entry| {
            let status = match &entry.resolution {
                ThemeAssetResolution::Ready(_) => ThemeEntryStatus::Ready,
                ThemeAssetResolution::Invalid(reason) => ThemeEntryStatus::Invalid(reason),
                ThemeAssetResolution::Shadowed(builtin) => ThemeEntryStatus::Shadowed(builtin),
                ThemeAssetResolution::Unreadable => ThemeEntryStatus::Unreadable,
            };
            (entry.name.as_str(), status)
        })
    }

    /// Whether the folder held more than discovery keeps (entries, candidates or files).
    pub(crate) const fn truncated(&self) -> bool {
        self.truncated
    }

    /// How many `.conf` files discovery saw and how many it read.
    pub(crate) const fn limits(&self) -> ThemeCatalogLimits {
        self.limits
    }

    /// Resolve a built-in or parsed user theme, case-insensitively, without filesystem access.
    pub(crate) fn resolve(&self, name: &str) -> Result<aterm_types::ColorScheme, String> {
        if let Some(scheme) = aterm_types::scheme::builtin(name) {
            return Ok(scheme);
        }
        let found = self.entries.iter().find_map(|entry| {
            if !entry.name.eq_ignore_ascii_case(name) {
                return None;
            }
            match &entry.resolution {
                ThemeAssetResolution::Ready(theme) => Some(Ok(theme.colors.clone())),
                ThemeAssetResolution::Invalid(reason) => {
                    Some(Err(format!("{} is invalid ({reason})", entry.source_id)))
                }
                ThemeAssetResolution::Shadowed(_) | ThemeAssetResolution::Unreadable => None,
            }
        });
        match found {
            Some(resolved) => resolved,
            None if self.truncated => Err(
                "no built-in or loaded custom theme by that name (the themes folder holds more \
                 files than aterm loads)"
                    .to_string(),
            ),
            None => Err("no built-in or loaded custom theme by that name".to_string()),
        }
    }

    /// The whole theme `name` names, by the same lookup as [`Self::resolve`].
    pub(crate) fn resolve_package(
        &self,
        name: &str,
    ) -> Result<std::borrow::Cow<'_, aterm_types::scheme::ThemePackage>, String> {
        if let Some(builtin) = aterm_types::scheme::builtin_theme(name)
            && let Some(package) = aterm_types::scheme::ThemePackage::of_builtin(builtin)
        {
            return Ok(std::borrow::Cow::Owned(package));
        }
        let found = self.entries.iter().find_map(|entry| {
            if !entry.name.eq_ignore_ascii_case(name) {
                return None;
            }
            match &entry.resolution {
                ThemeAssetResolution::Ready(theme) => {
                    Some(Ok(std::borrow::Cow::Borrowed(&**theme)))
                }
                ThemeAssetResolution::Invalid(reason) => {
                    Some(Err(format!("{} is invalid ({reason})", entry.source_id)))
                }
                ThemeAssetResolution::Shadowed(_) | ThemeAssetResolution::Unreadable => None,
            }
        });
        match found {
            Some(resolved) => resolved,
            None => Err(self
                .resolve(name)
                .err()
                .unwrap_or_else(|| "no built-in or loaded custom theme by that name".to_string())),
        }
    }

    #[cfg(test)]
    pub(crate) fn fingerprint(&self) -> u64 {
        self.entries.iter().fold(
            if self.truncated {
                0x5452_554E_4341_5445
            } else {
                0x5448_454D_4553
            },
            |fingerprint, entry| fingerprint.rotate_left(7) ^ entry.fingerprint,
        )
    }
}

fn normalize_theme_entries(entries: Vec<ThemeAsset>) -> Vec<ThemeAsset> {
    let (diagnostic, resolvable): (Vec<_>, Vec<_>) = entries.into_iter().partition(|entry| {
        matches!(
            entry.resolution,
            ThemeAssetResolution::Shadowed(_) | ThemeAssetResolution::Unreadable
        )
    });
    let mut normalized = collapse_case_collisions(resolvable);
    normalized.extend(diagnostic);
    normalized.sort_by(theme_entry_order);
    normalized
}

fn theme_entry_order(left: &ThemeAsset, right: &ThemeAsset) -> std::cmp::Ordering {
    left.name
        .to_ascii_lowercase()
        .cmp(&right.name.to_ascii_lowercase())
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.source_id.cmp(&right.source_id))
}

fn collapse_case_collisions(mut entries: Vec<ThemeAsset>) -> Vec<ThemeAsset> {
    entries.sort_by(theme_entry_order);
    let mut normalized = Vec::with_capacity(entries.len());
    while !entries.is_empty() {
        let end = entries[1..]
            .iter()
            .position(|candidate| !candidate.name.eq_ignore_ascii_case(&entries[0].name))
            .map_or(entries.len(), |offset| 1 + offset);
        if end == 1 {
            normalized.push(entries.remove(0));
            continue;
        }

        let first = &entries[0];
        let names = entries[..end]
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        normalized.push(invalid_theme_asset(
            &first.name,
            &first.source_id,
            &format!("ambiguous theme filenames differ only by ASCII case: {names}"),
        ));
        entries.drain(..end);
    }
    normalized
}

fn invalid_theme_asset(name: &str, source_id: &std::sync::Arc<str>, reason: &str) -> ThemeAsset {
    ThemeAsset {
        name: name.to_string(),
        source_id: std::sync::Arc::clone(source_id),
        fingerprint: stable_asset_fingerprint(0x49, source_id.as_bytes(), 0, 0, reason.as_bytes()),
        resolution: ThemeAssetResolution::Invalid(bounded_asset_text(
            reason,
            MAX_THEME_REASON_BYTES,
        )),
    }
}

fn diagnostic_theme_asset(
    name: &str,
    source_id: &std::sync::Arc<str>,
    resolution: ThemeAssetResolution,
) -> ThemeAsset {
    let tag: &[u8] = match resolution {
        ThemeAssetResolution::Shadowed(builtin) => builtin.as_bytes(),
        _ => b"unreadable",
    };
    ThemeAsset {
        name: name.to_string(),
        source_id: std::sync::Arc::clone(source_id),
        fingerprint: stable_asset_fingerprint(0x53, source_id.as_bytes(), 0, 0, tag),
        resolution,
    }
}

fn builtin_theme_name(name: &str) -> Option<&'static str> {
    aterm_types::scheme::builtin_names()
        .into_iter()
        .find(|builtin| builtin.eq_ignore_ascii_case(name))
}

fn safe_user_theme_name(name: &str) -> Result<(), &'static str> {
    aterm_types::scheme::validate_user_theme_name(name)
}

enum ThemeFileReadError {
    Unavailable(String),
    Invalid(String),
}

fn read_bounded_theme_file(path: &std::path::Path) -> Result<Vec<u8>, ThemeFileReadError> {
    aterm_effects::file_feed::read_bounded_regular_file(path, MAX_USER_THEME_FILE_BYTES).map_err(
        |error| match error.kind() {
            std::io::ErrorKind::InvalidData => ThemeFileReadError::Invalid(format!(
                "larger than the {MAX_USER_THEME_FILE_BYTES}-byte theme limit"
            )),
            std::io::ErrorKind::InvalidInput => ThemeFileReadError::Unavailable(
                "theme source is not a regular non-link file".into(),
            ),
            _ => ThemeFileReadError::Unavailable(format!("unreadable ({error})")),
        },
    )
}

/// Resolved custom kitty cursor asset for one config generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum KittySpriteAsset {
    #[default]
    BuiltIn,
    Ready {
        source_id: std::sync::Arc<str>,
        w: u16,
        h: u16,
        rgba: std::sync::Arc<[u8]>,
        fp: u64,
    },
    Invalid {
        source_id: std::sync::Arc<str>,
        bounded_reason: std::sync::Arc<str>,
    },
}

impl KittySpriteAsset {
    /// Stable paint/install identity.
    pub(crate) fn fingerprint(&self) -> u64 {
        match self {
            Self::BuiltIn => 0x4E59_414E_5F42_5549,
            Self::Ready { fp, .. } => *fp,
            Self::Invalid {
                source_id,
                bounded_reason,
            } => stable_asset_fingerprint(
                0x49,
                source_id.as_bytes(),
                0,
                0,
                bounded_reason.as_bytes(),
            ),
        }
    }

    pub(crate) fn source_id(&self) -> Option<&str> {
        match self {
            Self::BuiltIn => None,
            Self::Ready { source_id, .. } | Self::Invalid { source_id, .. } => Some(source_id),
        }
    }

    pub(crate) fn diagnostic(&self) -> Option<&str> {
        match self {
            Self::Invalid { bounded_reason, .. } => Some(bounded_reason),
            Self::BuiltIn | Self::Ready { .. } => None,
        }
    }
}

/// Admitted wallpaper image for one config revision, shared by rendering, validation and Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum WallpaperAsset {
    /// No `wallpaper` key configured.
    #[default]
    None,
    Ready {
        source_id: std::sync::Arc<str>,
        w: u32,
        h: u32,
        rgba: std::sync::Arc<[u8]>,
        fp: u64,
    },
    Invalid {
        source_id: std::sync::Arc<str>,
        bounded_reason: std::sync::Arc<str>,
    },
}

impl WallpaperAsset {
    /// Stable paint identity.
    pub(crate) fn fingerprint(&self) -> u64 {
        match self {
            Self::None => 0x5741_4C4C_5F4F_4646, // "WALL_OFF"
            Self::Ready { fp, .. } => *fp,
            Self::Invalid {
                source_id,
                bounded_reason,
            } => stable_asset_fingerprint(
                0x77,
                source_id.as_bytes(),
                0,
                0,
                bounded_reason.as_bytes(),
            ),
        }
    }

    pub(crate) fn source_id(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Ready { source_id, .. } | Self::Invalid { source_id, .. } => Some(source_id),
        }
    }

    pub(crate) fn diagnostic(&self) -> Option<&str> {
        match self {
            Self::Invalid { bounded_reason, .. } => Some(bounded_reason),
            Self::None | Self::Ready { .. } => None,
        }
    }
}

/// Every non-text config asset admitted at one revision.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConfigAssetCatalog {
    pub(crate) trail_style: std::sync::Arc<TrailStyleCache>,
    pub(crate) kitty_sprite: KittySpriteAsset,
    pub(crate) wallpaper: WallpaperAsset,
    pub(crate) themes: std::sync::Arc<ThemeCatalog>,
    pub(crate) sparkle_spec_consumers:
        Option<std::sync::Arc<aterm_effects::spec::SpecConsumerCapabilities>>,
}

impl ConfigAssetCatalog {
    #[cfg(test)]
    pub(crate) fn empty() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            trail_style: TrailStyleCache::empty(),
            kitty_sprite: KittySpriteAsset::BuiltIn,
            wallpaper: WallpaperAsset::None,
            themes: ThemeCatalog::empty(),
            sparkle_spec_consumers: None,
        })
    }
}

impl TrailStyleCache {
    #[cfg(test)]
    pub(crate) fn empty() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    /// Resolve `raw` once for one config generation.
    pub(crate) fn for_style(raw: &str) -> Self {
        Self {
            resolved_style_raw: raw.trim().into(),
            resolved_presentation: resolve_trail_presentation(raw),
        }
    }

    /// Cached answer, with a dynamic fallback for test/preview code that mutates `Config` without publishing a new asset generation.
    pub(crate) fn presentation_for(&self, raw: &str) -> ResolvedTrailPresentation {
        let raw = raw.trim();
        if raw == self.resolved_style_raw.as_ref() {
            self.resolved_presentation
        } else {
            resolve_trail_presentation(raw)
        }
    }
}

/// Canonical interpretation of one raw `cursor_trail_style` value, shared by rendering, validation and Settings so an invalid value is treated the same everywhere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResolvedTrailStyle {
    pub(crate) canonical: Option<&'static str>,
    pub(crate) style: Option<aterm_effects::cursor_glow::GlowStyle>,
    pub(crate) unknown: bool,
}

/// All string-keyed presentation decisions for one resolved trail style, resolved once per config generation so rendering reads only `Copy` fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResolvedTrailPresentation {
    pub(crate) style: ResolvedTrailStyle,
    pub(crate) beam: bool,
    pub(crate) ribbon_tall: bool,
    pub(crate) ribbon_flat: bool,
    pub(crate) classic_mono: bool,
    pub(crate) pet_species: Option<aterm_effects::kitty_pet::PetSpecies>,
    pub(crate) comet: bool,
}

impl Default for ResolvedTrailPresentation {
    fn default() -> Self {
        let raw = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE;
        let style = aterm_effects::cursor_glow::GlowStyle::parse(raw);
        Self {
            style: ResolvedTrailStyle {
                canonical: Some(raw),
                style: Some(style),
                unknown: false,
            },
            beam: aterm_effects::cursor_glow::style_has_beam_of(style, raw),
            ribbon_tall: true,
            ribbon_flat: false,
            classic_mono: false,
            pet_species: Some(aterm_effects::kitty_pet::PetSpecies::Cat),
            comet: false,
        }
    }
}

impl Default for TrailStyleCache {
    fn default() -> Self {
        Self {
            resolved_style_raw: crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE.into(),
            resolved_presentation: ResolvedTrailPresentation::default(),
        }
    }
}

impl ResolvedTrailStyle {
    const fn off() -> Self {
        Self {
            canonical: Some("off"),
            style: None,
            unknown: false,
        }
    }
}

/// Renderer-neutral preference inputs for the shared cursor glow resolver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CursorGlowInputs {
    pub(crate) enabled: bool,
    pub(crate) color: Option<u32>,
    pub(crate) accent: Option<u32>,
    pub(crate) duration_ms: u64,
    pub(crate) length: usize,
    pub(crate) intensity: f32,
    pub(crate) radius: f32,
    pub(crate) ring: bool,
}

/// `[sparkle_words.ink]`.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleInkConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) strength: Option<f32>,
    pub(crate) sweep_ms: Option<u32>,
    #[serde(rename = "loop")]
    pub(crate) loop_: Option<bool>,
}

/// `[sparkle_words.emphasis]`.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleEmphasisConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) extra_words: Option<Vec<String>>,
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.profanity]`.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleProfanityConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) style: Option<String>,
    pub(crate) supernova_chance: Option<u32>,
    pub(crate) magic: Option<bool>,
    pub(crate) palette: Option<Vec<String>>,
    pub(crate) density: Option<u32>,
    pub(crate) anim_ms: Option<u64>,
    pub(crate) jitter: Option<i8>,
    pub(crate) intensity: Option<f32>,
    pub(crate) bonk: Option<bool>,
    pub(crate) bonk_detonation: Option<bool>,
    pub(crate) extra_words: Option<Vec<String>>,
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.feline]`.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleFelineConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) style: Option<String>,
    pub(crate) idle: Option<bool>,
    pub(crate) gaze: Option<bool>,
    pub(crate) magic: Option<bool>,
    pub(crate) color: Option<String>,
    pub(crate) intensity: Option<f32>,
    pub(crate) allow_bare_cat: Option<bool>,
    pub(crate) cjk_single_char: Option<bool>,
    pub(crate) log: Option<bool>,
    pub(crate) extra_words: Option<Vec<String>>,
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.canine]`.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleCanineConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) extra_words: Option<Vec<String>>,
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// `[sparkle_words.tricks]`.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SparkleTricksConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) ignore_words: Option<Vec<String>>,
}

/// The `[matrix_rain]` table.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct MatrixRainConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) fps: Option<u32>,
    pub(crate) density: Option<u32>,
    pub(crate) speed: Option<u32>,
    pub(crate) trail: Option<u32>,
    pub(crate) alpha: Option<u32>,
    pub(crate) head_alpha: Option<u32>,
    pub(crate) hue: Option<String>,
    pub(crate) mutation_ms: Option<u64>,
    pub(crate) idle_secs: Option<u64>,
    pub(crate) suppress_in_alt_screen: Option<bool>,
    pub(crate) materialize: Option<bool>,
    pub(crate) output_material: Option<bool>,
    pub(crate) turn_wave: Option<bool>,
    pub(crate) bell_alert: Option<bool>,
    pub(crate) exit_tint: Option<bool>,
    pub(crate) ink_text: Option<bool>,
    pub(crate) phosphor: Option<bool>,
    pub(crate) seed: Option<u64>,
}

/// The `[presence]` table.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct PresenceConfig {
    pub(crate) rim: Option<bool>,
}

/// The `[claude_footer]` table.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct ClaudeFooterConfig {
    pub(crate) usage: Option<bool>,
    pub(crate) card: Option<bool>,
    pub(crate) acts: Option<bool>,
    pub(crate) agents: Option<bool>,
}

/// The `[render]` table.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct RenderConfig {
    pub(crate) repaint_nudge: Option<String>,
}

/// The `[harness]` table as the supervisor reads it.
#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct HarnessPolicy {
    pub(crate) policy: aterm_agent::supervise::SupervisorConfig,
    pub(crate) notes: Vec<String>,
}

/// The `[ai]` table as the window holds it.
#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct AiPolicy {
    pub(crate) local: aterm_agent::ai::config::Local,
    pub(crate) notes: Vec<String>,
    pub(crate) source: Option<aterm_agent::ai::config::FleetSource>,
    pub(crate) fleet_paths: Vec<std::path::PathBuf>,
    pub(crate) fleet: Option<Result<(aterm_agent::ai::config::Fleet, Vec<String>), String>>,
}

impl AiPolicy {
    /// The table `text` — the whole `aterm.toml` — writes, over the fleet
    /// document it names or the one beside the canonical `aterm.toml`.
    pub(crate) fn read(text: &str) -> Self {
        #[cfg(test)]
        let beside: Option<std::path::PathBuf> = None;
        #[cfg(not(test))]
        let beside = config_path().and_then(|path| path.parent().map(std::path::Path::to_path_buf));
        Self::read_beside(text, beside.as_deref())
    }

    /// [`Self::read`] with `aterm.toml`'s folder given.
    pub(crate) fn read_beside(text: &str, dir: Option<&std::path::Path>) -> Self {
        use aterm_agent::ai::config::{self, FleetSource};
        let (local, notes) = config::read(text);
        #[cfg(test)]
        let ai_dir: Option<std::path::PathBuf> = None;
        #[cfg(not(test))]
        let ai_dir = config::default_ai_dir();
        let config_path = dir.map(|dir| dir.join("aterm.toml"));
        let admitted = config::fleet_source(&local, config_path.as_deref());
        let fleet = admitted.as_ref().map(|source| {
            config::load_source(
                source,
                local.fleet_key.as_deref(),
                ai_dir.as_deref(),
                aterm_agent::supervise::limit::unix_now(),
            )
        });
        let sig = |p: &std::path::Path| {
            let mut s = p.as_os_str().to_owned();
            s.push(".sig");
            std::path::PathBuf::from(s)
        };
        let watched = admitted
            .clone()
            .or_else(|| dir.map(|dir| FleetSource::Path(dir.join(config::FLEET_FILE))));
        let fleet_paths = match &watched {
            Some(FleetSource::Path(p)) if local.fleet_key.is_some() => vec![p.clone(), sig(p)],
            Some(FleetSource::Path(p)) => vec![p.clone()],
            Some(FleetSource::Url(_)) => ai_dir
                .map(|d| {
                    let held = aterm_agent::ai::fleet::cache_dir(&d);
                    vec![
                        held.join(aterm_agent::ai::fleet::DOC_FILE),
                        held.join(aterm_agent::ai::fleet::SIG_FILE),
                    ]
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Self {
            local,
            notes,
            source: admitted,
            fleet_paths,
            fleet,
        }
    }

    /// Every choice, resolved.
    pub(crate) fn settings(&self) -> aterm_agent::ai::config::Settings {
        aterm_agent::ai::config::resolve(
            &self.local,
            self.source.as_ref(),
            self.fleet.clone(),
            self.notes.clone(),
        )
    }

    /// Whether the window looks at the ai CLIs and says what needs a person (`[ai] enabled`, on by default; never from a fleet document).
    pub(crate) fn enabled(&self) -> bool {
        self.local.enabled.unwrap_or(true)
    }
}

impl HarnessPolicy {
    /// The policy `text` — the whole `aterm.toml` — writes.
    pub(crate) fn read(text: &str) -> Self {
        let (policy, notes) = aterm_agent::supervise::SupervisorConfig::from_aterm_toml(text);
        Self { policy, notes }
    }

    /// The policy the file at `path` writes.
    pub(crate) fn read_path(path: Option<&std::path::Path>) -> Self {
        let (policy, notes) = aterm_agent::supervise::SupervisorConfig::from_path(path);
        Self { policy, notes }
    }

    /// The policy as Settings ▸ Harness says it aloud (the switch's spoken label and the save's feedback).
    pub(crate) fn words(&self) -> String {
        if !self.policy.enabled {
            return "Off: no agent session is supervised".to_string();
        }
        let limits = self.limits();
        if limits.is_empty() {
            return "Automatic".to_string();
        }
        let mut parts: Vec<String> = limits
            .iter()
            .filter_map(|limit| match limit {
                HarnessLimit::Row { label, value } => Some(format!("{label} set to {value}")),
                HarnessLimit::File => None,
            })
            .collect();
        let in_file = file_limits(&limits);
        if in_file > 0 {
            parts.push(format!(
                "{in_file} {} in aterm.toml",
                if in_file == 1 { "setting" } else { "settings" }
            ));
        }
        format!("Automatic, limited by {}", parts.join("; "))
    }

    /// How many of the limits only aterm.toml holds.
    pub(crate) fn file_limits(&self) -> usize {
        file_limits(&self.limits())
    }

    fn limits(&self) -> Vec<HarnessLimit> {
        self.policy
            .against_default()
            .limits
            .iter()
            .map(|limit| {
                harness_limit_row(limit).map_or(HarnessLimit::File, |(label, value)| {
                    HarnessLimit::Row { label, value }
                })
            })
            .collect()
    }
}

enum HarnessLimit {
    Row { label: &'static str, value: String },
    File,
}

fn file_limits(limits: &[HarnessLimit]) -> usize {
    limits
        .iter()
        .filter(|limit| matches!(limit, HarnessLimit::File))
        .count()
}

fn harness_limit_row(limit: &str) -> Option<(&'static str, String)> {
    let (leaf, shown) = limit.split_once(": ")?;
    let agent = format!("harness.{leaf}.enabled");
    let key = if crate::prefs::agent_switch(&agent).is_some() {
        agent
    } else {
        format!("harness.{leaf}")
    };
    let row = crate::prefs::NESTED_LEAVES
        .iter()
        .find(|row| row.key == key)?;
    let value = match row.kind {
        crate::prefs::EditKind::Bool => "Off".to_string(),
        crate::prefs::EditKind::Enum { .. } => {
            crate::native_settings::setting_choice_label(row.key, shown)
        }
        _ => return None,
    };
    Some((row.label, value))
}

/// The `[output_streak]` table (`docs/DESIGN-output-streak-2026-08-30.md`).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct OutputStreakConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) intensity: Option<f32>,
    pub(crate) tail: Option<u32>,
    pub(crate) max_streaks: Option<u32>,
    pub(crate) idle_secs: Option<f32>,
    pub(crate) sound: Option<bool>,
}

/// The `[packages]` table (`atpkg`, `docs/TOOLCHAIN-PACKAGE-MANAGER.md` §11).
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct PackagesConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) auto_update: Option<bool>,
    pub(crate) auto_install: Option<bool>,
    pub(crate) seed_install: Option<bool>,
    pub(crate) account: Option<String>,
    pub(crate) exclude: Option<Vec<String>>,
}

/// Default `[privacy] warmup_hold_ms`.
pub(crate) const PRIVACY_WARMUP_HOLD_MS_DEFAULT: u64 = 120_000;

/// Ceiling for `[privacy] warmup_hold_ms`.
pub(crate) const PRIVACY_WARMUP_HOLD_MS_MAX: u64 = 600_000;

/// Default `[privacy] probe_interval_ms`.
pub(crate) const PRIVACY_PROBE_INTERVAL_MS_DEFAULT: u64 = 5_000;

/// `[privacy] warmup`: whether aterm asks macOS for each configured folder at the first quiet moment of a launch that reads Full Disk Access off (the default, so a new Mac is asked for file access; `crate::consent_launch`), and whether the "Ask for folder access now" gesture on the Security panel is offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PrivacyWarmup {
    /// Never offered, never automatic.
    Never,
    /// Only the owner's gesture on the Security panel.
    OnRequest,
    /// The gesture, and the launch pass.
    #[default]
    Automatic,
}

impl PrivacyWarmup {
    /// The config spelling, which is also the report spelling.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnRequest => "on-request",
            Self::Automatic => "automatic",
        }
    }

    /// Every accepted spelling, in the order `--validate-config` lists them.
    pub(crate) const ALL: &'static [Self] = &[Self::Never, Self::OnRequest, Self::Automatic];

    /// Whether the Security panel's gesture is offered.
    pub(crate) const fn offered(self) -> bool {
        !matches!(self, Self::Never)
    }

    /// Parse a config spelling (case-insensitive).
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL
            .iter()
            .copied()
            .find(|candidate| value.eq_ignore_ascii_case(candidate.as_str()))
    }
}

/// The `[privacy]` table (macOS consent / tcc, `docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §4).
/// It gates System State's access steps (`docs/DESIGN-system-state-2026-10-21.md` §6): a switch
/// turned off pauses a step and records no answer; no key raises one.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct PrivacyConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) check: Option<bool>,
    pub(crate) notice: Option<bool>,
    pub(crate) report_attribution: Option<bool>,
    pub(crate) warmup: Option<String>,
    pub(crate) warmup_folders: Option<Vec<String>>,
    pub(crate) warmup_hold_ms: Option<u64>,
    pub(crate) probe_interval_ms: Option<u64>,
    pub(crate) observer: Option<bool>,
    pub(crate) protected_roots: Option<Vec<String>>,
    pub(crate) auto_accept: Option<bool>,
}

/// The `[reroute]` table, read by atpkg and mirrored here with atpkg's resolution.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct RerouteConfig {
    pub(crate) announce: Option<bool>,
    pub(crate) strict: Option<bool>,
}

/// The `[keeper]` table.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct KeeperConfig {
    pub(crate) enabled: Option<bool>,
}

impl Config {
    /// Whether the PTY keeper is on (`[keeper] enabled`, default `false`).
    pub(crate) fn keeper_enabled(&self) -> bool {
        self.keeper
            .as_ref()
            .and_then(|keeper| keeper.enabled)
            .unwrap_or(false)
    }
}

/// The `[session_log]` table.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SessionLogConfig {
    pub(crate) journal: Option<bool>,
    pub(crate) clause: Option<bool>,
}

impl Config {
    /// Whether the session journal is asked for (`[session_log] journal`, default `true`; `journal = false` opts out).
    pub(crate) fn session_log_journal(&self) -> bool {
        self.session_log
            .as_ref()
            .and_then(|s| s.journal)
            .unwrap_or(true)
    }

    /// Whether a tab's tooltip and menu show its session log clause (`[session_log] clause`, default `true`; `clause = false` opts out).
    pub(crate) fn session_log_clause(&self) -> bool {
        self.session_log
            .as_ref()
            .and_then(|s| s.clause)
            .unwrap_or(true)
    }
}

/// `[machine]`: macOS host settings `atpkg` applies on every package pass and on `aterm pkg machine apply`, mirrored so Settings ▸ Security can show them.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct MachineConfig {
    pub(crate) universal_control: Option<String>,
    pub(crate) rosetta: Option<bool>,
}

/// The `[sys]` table: system awareness.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct SysConfig {
    pub(crate) stats_bars: Option<String>,
    pub(crate) stats_deck_size: Option<String>,
    pub(crate) disk_roots: Option<Vec<String>>,
}

/// The `[fabric]` table.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct FabricConfig {
    pub(crate) command: Option<FabricCommand>,
    pub(crate) hold_on_bridge_lost: Option<bool>,
}

/// `[fabric] command` as the argv it names, from either spelling ([`aterm_types::fabric_command`]).
#[derive(Default, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
pub(crate) struct FabricCommand(pub(crate) Vec<String>);

impl<'de> serde::Deserialize<'de> for FabricCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = FabricCommand;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(aterm_types::fabric_command::EXPECTED)
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<FabricCommand, E> {
                Ok(FabricCommand(aterm_types::fabric_command::split_line(s)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<FabricCommand, A::Error> {
                let mut argv = Vec::new();
                while let Some(word) = seq.next_element::<String>()? {
                    aterm_types::fabric_command::check_word(argv.len(), &word)
                        .map_err(<A::Error as serde::de::Error>::custom)?;
                    argv.push(word);
                }
                Ok(FabricCommand(argv))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// The `[fleet]` table: this machine's fleet listener, off by default.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct FleetConfig {
    pub(crate) listen: Option<String>,
    pub(crate) allow: Option<String>,
    pub(crate) drive_from: Option<Vec<String>>,
    pub(crate) watch: Option<bool>,
}

impl FleetConfig {
    /// `watch`, absent as false.
    pub(crate) fn watch_or_default(&self) -> bool {
        self.watch.unwrap_or(false)
    }
}

/// The `[update]` table.
#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(default)]
pub(crate) struct UpdateConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) owner: Option<String>,
    pub(crate) repo: Option<String>,
    pub(crate) auto_apply: Option<bool>,
    pub(crate) require_team_id: Option<String>,
}

/// `[update] require_team_id` as this build honours it.
pub(crate) fn update_required_team_id(config: &Config) -> Option<&str> {
    if !cfg!(any(debug_assertions, feature = "dev-seams")) {
        return None;
    }
    config
        .update
        .as_ref()
        .and_then(|u| u.require_team_id.as_deref())
}

/// The `update.auto_apply` answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoApplySetting {
    /// The default: the in-session lane applies a staged build by itself.
    On,
    /// `[update] auto_apply = false`.
    OffByConfig,
}

/// Whether a staged update applies by itself.
pub(crate) fn update_auto_apply_setting(config: &Config) -> AutoApplySetting {
    if config
        .update
        .as_ref()
        .and_then(|u| u.auto_apply)
        .unwrap_or(true)
    {
        AutoApplySetting::On
    } else {
        AutoApplySetting::OffByConfig
    }
}

/// "Check for updates automatically" as saved (`[update] enabled`, default on).
pub(crate) fn update_checks_automatic(config: &Config) -> bool {
    config
        .update
        .as_ref()
        .and_then(|u| u.enabled)
        .unwrap_or(true)
}

/// [`update_auto_apply_setting`] as the bool the lane's arm and poll read.
pub(crate) fn update_auto_apply(config: &Config) -> bool {
    matches!(update_auto_apply_setting(config), AutoApplySetting::On)
}

/// Default in-grid tab strip rows per platform.
#[cfg(target_os = "macos")]
pub(crate) const DEFAULT_TAB_STRIP_ROWS: u16 = 0;
/// See the macOS variant above.
#[cfg(not(target_os = "macos"))]
pub(crate) const DEFAULT_TAB_STRIP_ROWS: u16 = 1;
/// Upper clamp on `tab_strip_rows` so a mis-set config can't starve the terminal.
pub(crate) const MAX_TAB_STRIP_ROWS: u16 = 4;

/// Resolve the configured tab-strip row count (config, else [`DEFAULT_TAB_STRIP_ROWS`]), clamped to `0..=MAX_TAB_STRIP_ROWS`.
pub(crate) fn resolve_tab_strip_rows(config: &Config) -> u16 {
    config
        .tab_strip_rows
        .unwrap_or(DEFAULT_TAB_STRIP_ROWS)
        .min(MAX_TAB_STRIP_ROWS)
}

/// Search index depth from `search_history_lines`.
pub(crate) fn search_index_depth(config: &Config) -> usize {
    config
        .search_history_lines
        .map_or(aterm_core::search::DEFAULT_MAX_CACHED_LINES, |n| n as usize)
}

/// Aterm's own theme for `appearance`, a side of Default's light/dark pair.
pub(crate) const fn default_pair_side(appearance: aterm_types::Appearance) -> &'static str {
    match appearance {
        aterm_types::Appearance::Dark => "Default",
        aterm_types::Appearance::Light => "Default Light",
    }
}

/// Resolve a plain or appearance-split theme selector permissively through the one grammar ([`aterm_types::scheme::ThemeSelector`]).
pub(crate) fn resolve_theme_name_value(
    raw: Option<&str>,
    appearance: aterm_types::Appearance,
) -> Option<String> {
    let (selector, _) = aterm_types::scheme::ThemeSelector::parse(raw?);
    selector.name_for(appearance).map(str::to_string)
}

/// The selector issues in a `theme` value, as one sentence that also says what each appearance draws instead.
pub(crate) fn theme_selector_notice(raw: Option<&str>) -> Option<String> {
    use aterm_types::scheme::ThemeSelector;
    let (selector, issues) = ThemeSelector::parse(raw?);
    if issues.is_empty() {
        return None;
    }
    let mut message = issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    let unset = |side| selector.name_for(side).map(str::to_string);
    let consequence = match (&selector, selector.name_for(aterm_types::Appearance::Dark)) {
        (ThemeSelector::Single(_), None) => Some("Default is used".to_string()),
        (_, None) => Some("both modes use Default".to_string()),
        (ThemeSelector::Split { dark: None, .. }, Some(_)) => {
            unset(aterm_types::Appearance::Dark).map(|name| format!("dark mode uses {name:?}"))
        }
        (ThemeSelector::Split { light: None, .. }, Some(_)) => {
            unset(aterm_types::Appearance::Light).map(|name| format!("light mode uses {name:?}"))
        }
        _ => None,
    };
    if let Some(consequence) = consequence {
        message.push_str("; ");
        message.push_str(&consequence);
    }
    Some(message)
}

impl Config {
    /// Parse `aterm.toml` text.
    pub(crate) fn parse(text: &str) -> Result<Self, aterm_toml::Error> {
        let mut config: Self = aterm_toml::from_str(text)?;
        config.harness = HarnessPolicy::read(text);
        config.ai = AiPolicy::read(text);
        Ok(config)
    }

    /// Config used when `text` does not parse.
    pub(crate) fn stand_in(text: &str) -> Self {
        Self {
            harness: HarnessPolicy::read(text),
            ai: AiPolicy::read(text),
            ..Self::default()
        }
    }

    /// Scheme name this config selects for `appearance`, honouring `theme = "dark:<name>,light:<name>"`.
    pub(crate) fn resolve_theme_name(&self, appearance: aterm_types::Appearance) -> Option<String> {
        resolve_theme_name_value(self.theme.as_deref(), appearance)
    }

    #[cfg(test)]
    pub(crate) fn theme(&self) -> Theme {
        self.theme_for(aterm_types::Appearance::Dark)
    }

    #[cfg(test)]
    pub(crate) fn theme_for(&self, appearance: aterm_types::Appearance) -> Theme {
        self.theme_for_with_assets(appearance, &ThemeCatalog::default())
    }

    /// Renderer theme resolved exclusively from the immutable custom-theme catalog carried by the active config snapshot.
    pub(crate) fn theme_for_with_assets(
        &self,
        appearance: aterm_types::Appearance,
        themes: &ThemeCatalog,
    ) -> Theme {
        self.resolved_colors(appearance, themes, None).theme()
    }

    /// Whether Alt/Option sends esc-prefixed (Meta) sequences.
    pub(crate) fn option_as_meta_or_default(&self) -> bool {
        self.option_as_meta.unwrap_or(true)
    }

    /// Whether a paste holding a line end (a trailing one included) is confirmed when bracketed paste is off.
    pub(crate) fn confirm_multiline_paste_or_default(&self) -> bool {
        self.confirm_multiline_paste.unwrap_or(true)
    }

    /// Whether the previous graceful quit's windows, tabs and panes (with cwd) reopen at launch.
    pub(crate) fn restore_session_or_default(&self) -> bool {
        self.restore_session.unwrap_or(true)
    }

    /// Whether a completed mouse selection copies to the clipboard.
    pub(crate) fn copy_on_select_or_default(&self) -> bool {
        self.copy_on_select
            .unwrap_or(cfg!(not(target_os = "linux")))
    }

    /// Whether aterm primes detected coding agents on each fresh session spawn.
    pub(crate) fn agents_auto_prime_or_default(&self) -> bool {
        self.agents_auto_prime.unwrap_or(true)
    }

    /// Master switch for status classification.
    pub(crate) fn tab_status_or_default(&self) -> bool {
        self.tab_status.unwrap_or(true)
    }

    /// The warm spare's mode (`spare_shell`, default `auto`; an unknown value reads `auto`).
    pub(crate) fn spare_shell_mode(&self) -> crate::spare::SpareMode {
        self.spare_shell
            .as_deref()
            .and_then(crate::spare::SpareMode::parse)
            .unwrap_or(crate::spare::SpareMode::Auto)
    }

    /// `spare_shell_needs_person` (default true).
    pub(crate) fn spare_shell_needs_person_or_default(&self) -> bool {
        self.spare_shell_needs_person.unwrap_or(true)
    }

    /// `--validate-config` warning for an unknown `spare_shell` (at load it reads `auto`).
    pub(crate) fn spare_shell_warning(&self) -> Option<String> {
        let raw = self.spare_shell.as_deref()?;
        if crate::spare::SpareMode::parse(raw).is_some() {
            return None;
        }
        Some(format!(
            "spare_shell: {raw:?} is not a spare mode — reading auto; expected one of {}",
            crate::spare::SpareMode::SPELLINGS.join("|")
        ))
    }

    /// Whether a status sets the tab's indicator bits, independent of [`Self::tab_status_or_default`].
    pub(crate) fn tab_status_badge_or_default(&self) -> bool {
        self.tab_status_badge.unwrap_or(true)
    }

    /// Whether the connection role marks the tab.
    pub(crate) fn tab_connection_badge_or_default(&self) -> bool {
        self.tab_connection_badge.unwrap_or(true)
    }

    /// Whether aterm posts its own notices to the desktop.
    pub(crate) fn desktop_alerts_or_default(&self) -> bool {
        self.desktop_alerts.unwrap_or(false)
    }

    /// Stall threshold, bounded so a mistyped value can neither call every job quiet on arrival nor keep a finished one lit past any useful horizon.
    pub(crate) fn tab_status_quiet_after_ms_or_default(&self) -> u64 {
        self.tab_status_quiet_after_ms
            .unwrap_or(DEFAULT_TAB_STATUS_QUIET_AFTER_MS)
            .clamp(MIN_TAB_STATUS_QUIET_AFTER_MS, MAX_TAB_STATUS_QUIET_AFTER_MS)
    }

    /// Publication dwell, bounded so hysteresis cannot be set long enough to make the badge useless.
    pub(crate) fn tab_status_dwell_ms_or_default(&self) -> u64 {
        self.tab_status_dwell_ms
            .unwrap_or(DEFAULT_TAB_STATUS_DWELL_MS)
            .clamp(MIN_TAB_STATUS_DWELL_MS, MAX_TAB_STATUS_DWELL_MS)
    }

    /// The resolved classifier policy.
    pub(crate) fn tab_status_policy(&self) -> crate::session_status::StatusPolicy {
        crate::session_status::StatusPolicy {
            quiet_after: std::time::Duration::from_millis(
                self.tab_status_quiet_after_ms_or_default(),
            ),
            dwell: std::time::Duration::from_millis(self.tab_status_dwell_ms_or_default()),
        }
    }

    /// Observation budget interval, derived from dwell.
    pub(crate) fn tab_status_observe_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.tab_status_dwell_ms_or_default().clamp(
            MIN_TAB_STATUS_OBSERVE_INTERVAL_MS,
            NOMINAL_TAB_STATUS_OBSERVE_INTERVAL_MS,
        ))
    }

    /// Whether a cursor trail is drawn at all.
    pub(crate) fn cursor_trail_drawn(&self) -> bool {
        !self.cursor_trail_style_raw().eq_ignore_ascii_case("off")
    }

    /// Typing-momentum glow switch.
    pub(crate) fn cursor_momentum_glow_or_default(&self) -> bool {
        if self.cursor_trail == Some(false) {
            return false;
        }
        self.cursor_momentum_glow
            .unwrap_or(self.cursor_trail.unwrap_or(DEFAULT_DECORATIVE_EFFECTS))
    }

    /// Whether a config apply should warm the demand-driven effect pipelines (`aterm_gpu::EffectPipeline`) off the frame path.
    pub(crate) fn warms_effect_pipelines(&self) -> bool {
        self.cursor_trail_drawn()
    }

    /// Trail sound effects on/off (`trail_sounds`, default on; every gate that silences the trail's light silences them too).
    pub(crate) fn trail_sounds_or_default(&self) -> bool {
        self.trail_sounds.unwrap_or(true)
    }

    /// Tone-melody on/off (`tone_melody`, default on; the field doc lists what it moves and the gates it inherits).
    pub(crate) fn tone_melody_or_default(&self) -> bool {
        self.tone_melody.unwrap_or(true)
    }

    /// Kitty-command gate: `[sparkle_words] enabled` (via [`Self::sparkle_words_enabled_or_default`]) and `[sparkle_words.tricks] enabled` (default on).
    pub(crate) fn kitty_tricks_enabled(&self) -> bool {
        self.sparkle_words_enabled_or_default()
            && self
                .sparkle_words
                .as_ref()
                .and_then(|sw| sw.tricks.as_ref())
                .and_then(|t| t.enabled)
                .unwrap_or(true)
    }

    /// Whether a fire is on `[sparkle_words.tricks] ignore_words`.
    pub(crate) fn kitty_trick_ignored<'w>(
        &self,
        word: &str,
        commands: impl IntoIterator<Item = &'w str>,
    ) -> bool {
        let Some(ignore) = self
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.tricks.as_ref())
            .and_then(|t| t.ignore_words.as_deref())
            .filter(|words| !words.is_empty())
        else {
            return false;
        };
        let listed = |surface: &str| {
            let folded = aterm_lexicon::fold(surface);
            ignore.iter().any(|w| aterm_lexicon::fold(w) == folded)
        };
        listed(word) || commands.into_iter().filter(|c| *c != word).any(listed)
    }

    /// Secure keyboard entry (default off, like every security opt-in: it suppresses other apps' global hotkeys while active).
    pub(crate) fn secure_keyboard_entry_or_default(&self) -> bool {
        self.secure_keyboard_entry.unwrap_or(false)
    }

    /// Ambient-bed on/off (`trail_sound_bed`, default off).
    pub(crate) fn trail_sound_bed_or_default(&self) -> bool {
        self.trail_sound_bed.unwrap_or(false)
    }

    /// Sing-along riff on/off (`trail_sound_riff`, default on).
    pub(crate) fn trail_sound_riff_or_default(&self) -> bool {
        self.trail_sound_riff.unwrap_or(true)
    }

    /// Audible bell on/off (`bell_sound`, default on).
    pub(crate) fn bell_sound_or_default(&self) -> bool {
        self.bell_sound.unwrap_or(true)
    }

    /// Parsed `trail_sound_style` voice (default `"auto"`, follow the trail style), via [`aterm_effects::trail_sound::SoundVoice::parse`] (case-insensitive, aliases included) without allocating.
    pub(crate) fn trail_sound_voice(&self) -> aterm_effects::trail_sound::SoundVoice {
        use aterm_effects::trail_sound::SoundVoice;
        self.trail_sound_style
            .as_deref()
            .and_then(SoundVoice::parse)
            .unwrap_or_default()
    }

    /// [`Self::trail_sound_voice`] for a trail gesture under one resolved presentation (`RAINBOW-KITTY-V2.md` §16 row 9).
    pub(crate) fn trail_sound_voice_for(
        &self,
        presentation: ResolvedTrailPresentation,
    ) -> aterm_effects::trail_sound::SoundVoice {
        use aterm_effects::trail_sound::SoundVoice;
        match self.trail_sound_voice() {
            SoundVoice::Style
                if presentation.style.style
                    == Some(aterm_effects::cursor_glow::GlowStyle::RainbowKitty) =>
            {
                SoundVoice::RainbowKittyV2
            }
            voice => voice,
        }
    }

    /// `--validate-config` warning for an unknown `trail_sound_style` (at load it silently falls back to `auto`).
    pub(crate) fn trail_sound_style_warning(&self) -> Option<String> {
        use aterm_effects::trail_sound::SoundVoice;
        let raw = self.trail_sound_style.as_deref().map(str::trim)?;
        if SoundVoice::parse(raw).is_some() {
            return None;
        }
        Some(format!(
            "trail_sound_style: {raw:?} is not a typing sound — playing auto (follow the \
             trail); expected one of {} (or a documented alias like water/mech/thock)",
            crate::prefs::TRAIL_SOUND_STYLES.join("|")
        ))
    }

    /// Trail sound volume 0..1 (`trail_sound_volume`, default 0.4), clamped.
    pub(crate) const fn trail_sound_volume(&self) -> f32 {
        match self.trail_sound_volume {
            Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
            Some(_) => 0.0,
            None => 0.4,
        }
    }

    /// Wallpaper dim fraction toward the theme background, clamped to `0..=1`.
    pub(crate) const fn wallpaper_dim_or_default(&self) -> f32 {
        match self.wallpaper_dim {
            Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
            _ => 0.3,
        }
    }

    /// Whether default-fg glyphs take the backdrop-hue tint while a wallpaper is attached (`wallpaper_text_tint`).
    pub(crate) fn wallpaper_text_tint_or_default(&self) -> bool {
        self.wallpaper_text_tint.unwrap_or(true)
    }

    /// Parsed `motion` mode.
    pub(crate) fn motion_mode(&self) -> crate::motion::MotionMode {
        self.motion.as_deref().map_or(
            crate::motion::MotionMode::Auto,
            crate::motion::MotionMode::parse,
        )
    }

    /// Whether load-adaptive shedding may force the reduced state for decorative effects under sustained render overload.
    pub(crate) fn load_adaptive_motion_or_default(&self) -> bool {
        self.load_adaptive_motion.unwrap_or(true)
    }

    /// Serious-mode master gate.
    pub(crate) fn serious_mode_or_default(&self) -> bool {
        self.serious_mode.unwrap_or(false)
    }

    /// Predictive echo mode (`off` / `adaptive` / `always`).
    pub(crate) fn predictive_echo_or_default(&self) -> &str {
        self.predictive_echo
            .as_deref()
            .map(str::trim)
            .unwrap_or("adaptive")
    }

    /// Predictive editing (`aterm_predict::Predictor::set_editing`).
    pub(crate) fn predictive_echo_editing_or_default(&self) -> bool {
        self.predictive_echo_editing.unwrap_or(true)
    }

    /// Focus-linked shell priority boost.
    pub(crate) fn focus_boost_or_default(&self) -> bool {
        self.focus_boost.unwrap_or(true)
    }

    /// Trail fade duration (ms).
    pub(crate) fn cursor_trail_ms_or_default(&self) -> u64 {
        self.cursor_trail_ms.unwrap_or(260).clamp(30, 2000)
    }

    /// Max comet length in cells.
    pub(crate) fn cursor_trail_length_or_default(&self) -> usize {
        self.cursor_trail_length.unwrap_or(24).clamp(1, 512)
    }

    /// The explicit trail-colour override as packed `0x00RRGGBB`, if `cursor_trail_color` is a colour.
    pub(crate) fn cursor_trail_color_u32(&self) -> Option<u32> {
        match self.trail_color() {
            aterm_effects::cursor_fx::TrailColor::Pinned(color) => Some(color),
            _ => None,
        }
    }

    /// What `cursor_trail_color` says ([`crate::prefs::trail_color_setting`]).
    pub(crate) fn trail_color(&self) -> aterm_effects::cursor_fx::TrailColor {
        crate::prefs::trail_color_setting(self.cursor_trail_color.as_deref())
    }

    /// The cursor family's knobs as this config sets them, for a trail that may draw (`enabled`: Serious Mode folded in by the host) against `theme_cursor` in `presentation` (its brightness default is the style's, [`Self::cursor_trail_intensity_for`]).
    pub(crate) fn cursor_glow_inputs(
        &self,
        enabled: bool,
        theme_cursor: u32,
        presentation: ResolvedTrailPresentation,
    ) -> CursorGlowInputs {
        CursorGlowInputs {
            enabled,
            color: self.trail_color().knob(theme_cursor),
            accent: self.cursor_trail_accent_u32(),
            duration_ms: self.cursor_trail_ms_or_default(),
            length: self.cursor_trail_length_or_default(),
            intensity: self.cursor_trail_intensity_for(presentation),
            radius: self.cursor_trail_radius_or_default(),
            ring: self.cursor_trail_ring_or_default(),
        }
    }

    /// The primary-font request.
    pub(crate) fn font_family_request(&self) -> Option<String> {
        self.font_family.clone().filter(|family| {
            !family.trim().is_empty() && aterm_render::display_face_ids_for_family(family).is_none()
        })
    }

    /// The display layer drawn over the primary font.
    pub(crate) fn display_faces_request(&self) -> Vec<&'static str> {
        if let Some(raw) = self.display_font.as_deref() {
            return aterm_render::display_layer_ids(raw.split('+'));
        }
        self.font_family
            .as_deref()
            .and_then(aterm_render::display_face_ids_for_family)
            .map(aterm_render::display_layer_ids)
            .unwrap_or_default()
    }

    /// The aurora accent-colour override as packed `0x00RRGGBB`, if set + valid.
    pub(crate) fn cursor_trail_accent_u32(&self) -> Option<u32> {
        self.cursor_trail_accent
            .as_deref()
            .and_then(aterm_types::scheme::parse_hex)
            .map(rgb_u32)
    }

    /// Configured cursor-trail style, trimmed but not lowercased.
    pub(crate) fn cursor_trail_style_raw(&self) -> &str {
        drawn_cursor_trail_style(self.cursor_trail, self.cursor_trail_style.as_deref())
    }

    /// Trail brightness for the resolved style, clamped 0.0..=1.0.
    pub(crate) fn cursor_trail_intensity_for(
        &self,
        presentation: ResolvedTrailPresentation,
    ) -> f32 {
        let mono = presentation.classic_mono || self.trail_color().chosen();
        let v = self.cursor_trail_intensity.unwrap_or(
            aterm_effects::cursor_fx::default_trail_intensity(presentation.style.style, mono),
        );
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Bloom-crown radius in cells, default 0.6, clamped 0.0..=2.0.
    pub(crate) fn cursor_trail_radius_or_default(&self) -> f32 {
        self.cursor_trail_radius.unwrap_or(0.6).clamp(0.0, 2.0)
    }

    /// Landing-ring ping, default on.
    pub(crate) fn cursor_trail_ring_or_default(&self) -> bool {
        self.cursor_trail_ring.unwrap_or(true)
    }

    /// `[sparkle_words] suppress_in_alt_screen`.
    pub(crate) fn sparkle_suppress_alt_screen(&self) -> bool {
        self.sparkle_words
            .as_ref()
            .and_then(|sw| sw.suppress_in_alt_screen)
            .unwrap_or(false)
    }

    /// The languages whose ambiguous homograph lexicon entries are un-gated.
    pub(crate) fn sparkle_languages(&self) -> Vec<String> {
        self.sparkle_words
            .as_ref()
            .and_then(|s| s.languages.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec!["en".to_string()])
    }

    fn admitted_sparkle_lexicon(
        &self,
        fingerprint: &mut std::collections::hash_map::DefaultHasher,
    ) -> Option<AdmittedPathFeed> {
        let path = self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.lexicon.as_deref())?;
        Some(read_and_fingerprint_path_feed(
            path,
            aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
            fingerprint,
        ))
    }

    #[cfg(test)]
    fn sparkle_toy_packs(&self) -> LoadedToyPacks {
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        self.sparkle_toy_packs_with_fingerprint(&mut fingerprint)
    }

    fn sparkle_toy_packs_with_fingerprint(
        &self,
        fingerprint: &mut std::collections::hash_map::DefaultHasher,
    ) -> LoadedToyPacks {
        let mut loaded = LoadedToyPacks::default();
        let Some(paths) = self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.toy_packs.as_deref())
        else {
            return loaded;
        };
        if paths.len() > MAX_ACTIVE_TOY_PACKS {
            crate::logging::stderr_line!(
                "aterm-gui: sparkle_words.toy_packs lists {} paths; only the first \
                 {MAX_ACTIVE_TOY_PACKS} are active",
                paths.len()
            );
        }
        for (index, path) in paths.iter().take(MAX_ACTIVE_TOY_PACKS).enumerate() {
            let (expanded, source) = read_and_fingerprint_path_feed(
                path,
                aterm_effects::spec::MAX_TOY_PACK_BYTES,
                fingerprint,
            );
            let source = match source {
                Ok(source) => source,
                Err(error) => {
                    crate::logging::stderr_line!(
                        "aterm-gui: sparkle_words.toy_packs[{index}] {expanded:?} \
                         unreadable ({error}); skipping"
                    );
                    continue;
                }
            };
            let pack = match aterm_effects::spec::compile_toy_pack_toml(&source) {
                Ok(pack) => pack,
                Err(error) => {
                    crate::logging::stderr_line!(
                        "aterm-gui: sparkle_words.toy_packs[{index}] {expanded:?} \
                         invalid ({error}); skipping"
                    );
                    continue;
                }
            };
            let (_, spec_table, lexicon_toml) = pack.into_parts();
            loaded.spec_table.overlay(spec_table);
            loaded.lexicon_toml.push_str(&lexicon_toml);
        }
        loaded
    }

    /// Resolve the non-feed part of a catalog while the Sparkle generation is prepared separately.
    pub(crate) fn resolve_preliminary_asset_catalog_with_themes(
        &self,
        themes: std::sync::Arc<ThemeCatalog>,
    ) -> std::sync::Arc<ConfigAssetCatalog> {
        std::sync::Arc::new(ConfigAssetCatalog {
            trail_style: std::sync::Arc::new(self.resolve_trail_style_cache()),
            kitty_sprite: resolve_kitty_sprite_asset(self.cursor_nyan_sprite.as_deref()),
            wallpaper: resolve_wallpaper_asset(self.wallpaper.as_deref()),
            themes,
            sparkle_spec_consumers: None,
        })
    }

    /// Resolve every filesystem-backed visual asset for one config generation.
    pub(crate) fn resolve_asset_catalog_with_themes(
        &self,
        themes: std::sync::Arc<ThemeCatalog>,
    ) -> std::sync::Arc<ConfigAssetCatalog> {
        std::sync::Arc::new(ConfigAssetCatalog {
            trail_style: std::sync::Arc::new(self.resolve_trail_style_cache()),
            kitty_sprite: resolve_kitty_sprite_asset(self.cursor_nyan_sprite.as_deref()),
            wallpaper: resolve_wallpaper_asset(self.wallpaper.as_deref()),
            themes,
            sparkle_spec_consumers: None,
        })
    }

    /// This config's trail presentation, resolved once per generation.
    pub(crate) fn resolve_trail_style_cache(&self) -> TrailStyleCache {
        TrailStyleCache::for_style(self.cursor_trail_style_raw())
    }

    #[cfg(test)]
    pub(crate) fn resolve_asset_catalog(&self) -> std::sync::Arc<ConfigAssetCatalog> {
        self.resolve_asset_catalog_with_themes(ThemeCatalog::empty())
    }

    #[cfg(test)]
    pub(crate) fn path_feed_fingerprints(&self) -> PathFeedFps {
        #[cfg(test)]
        PATH_FEED_FINGERPRINTS.with(|count| count.set(count.get().saturating_add(1)));
        use std::hash::{Hash, Hasher};
        fn fold(
            hasher: &mut std::collections::hash_map::DefaultHasher,
            path: &str,
            max_bytes: usize,
            remaining_opens: &mut usize,
            remaining_bytes: &mut usize,
        ) {
            path.hash(hasher);
            let Some(read_ceiling) = max_bytes.checked_add(1) else {
                false.hash(hasher);
                return;
            };
            if *remaining_opens == 0 || *remaining_bytes < read_ceiling {
                false.hash(hasher);
                return;
            }
            *remaining_opens -= 1;
            *remaining_bytes -= read_ceiling;
            match aterm_effects::file_feed::fingerprint_bounded_regular_utf8(
                &sparkle_expand_tilde(path),
                max_bytes,
            ) {
                Ok(content_fingerprint) => {
                    true.hash(hasher);
                    content_fingerprint.hash(hasher);
                }
                Err(_) => false.hash(hasher),
            }
        }
        let mut remaining_opens = MAX_PATH_FEED_OPEN_ATTEMPTS;
        let mut remaining_bytes = MAX_PATH_FEED_FINGERPRINT_READ_BYTES;
        let mut deco = std::collections::hash_map::DefaultHasher::new();
        if let Some(sw) = self
            .sparkle_words
            .as_ref()
            .filter(|sparkle| sparkle.enabled != Some(false))
        {
            if let Some(lexicon) = sw.lexicon.as_deref() {
                fold(
                    &mut deco,
                    lexicon,
                    aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
                    &mut remaining_opens,
                    &mut remaining_bytes,
                );
            }
            for path in sw
                .toy_packs
                .as_deref()
                .unwrap_or_default()
                .iter()
                .take(MAX_ACTIVE_TOY_PACKS)
            {
                fold(
                    &mut deco,
                    path,
                    aterm_effects::spec::MAX_TOY_PACK_BYTES,
                    &mut remaining_opens,
                    &mut remaining_bytes,
                );
            }
        }
        PathFeedFps {
            deco: deco.finish(),
        }
    }

    /// Toy Pack admission checks for `--validate-config`.
    pub(crate) fn sparkle_toy_pack_warnings(&self) -> Vec<String> {
        let Some(paths) = self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.toy_packs.as_deref())
        else {
            return Vec::new();
        };
        let mut warnings = Vec::new();
        if paths.len() > MAX_ACTIVE_TOY_PACKS {
            warnings.push(format!(
                "sparkle_words.toy_packs: {} paths listed; only the first \
                 {MAX_ACTIVE_TOY_PACKS} load",
                paths.len()
            ));
        }
        for (index, path) in paths.iter().take(MAX_ACTIVE_TOY_PACKS).enumerate() {
            let expanded = sparkle_expand_tilde(path);
            match aterm_effects::spec::read_toy_pack_file(&expanded) {
                Err(error) => warnings.push(format!(
                    "sparkle_words.toy_packs[{index}] {expanded:?} unreadable ({error})"
                )),
                Ok(source) => {
                    if let Err(error) = aterm_effects::spec::compile_toy_pack_toml(&source) {
                        warnings.push(format!(
                            "sparkle_words.toy_packs[{index}] {expanded:?} invalid ({error})"
                        ));
                    }
                }
            }
        }
        warnings
    }

    /// Resolve the pack/config bundle (the deco config and the lexicon override TOML), never per frame.
    pub(crate) fn sparkle_runtime_parts(
        &self,
    ) -> Option<(crate::word_decorations::DecoConfig, Option<String>)> {
        self.sparkle_runtime_parts_with_fingerprint().0
    }

    fn sparkle_runtime_parts_with_fingerprint(
        &self,
    ) -> (
        Option<(crate::word_decorations::DecoConfig, Option<String>)>,
        u64,
    ) {
        use std::hash::Hasher as _;

        #[cfg(test)]
        SPARKLE_HOST_PREPARES.with(|count| count.set(count.get().saturating_add(1)));
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        if self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.enabled)
            == Some(false)
        {
            return (None, fingerprint.finish());
        }
        let admitted_lexicon = self.admitted_sparkle_lexicon(&mut fingerprint);
        let LoadedToyPacks {
            spec_table,
            lexicon_toml,
        } = self.sparkle_toy_packs_with_fingerprint(&mut fingerprint);
        let resolved = self
            .sparkle_deco_config_with_pack_specs(spec_table)
            .map(|cfg| {
                let override_toml = self.sparkle_override_toml_with_admitted_lexicon(
                    &lexicon_toml,
                    admitted_lexicon.as_ref(),
                );
                (cfg, override_toml)
            });
        (resolved, fingerprint.finish())
    }

    #[cfg(test)]
    pub(crate) fn prepare_sparkle_runtime(&self) -> PreparedSparkleRuntime {
        self.prepare_sparkle_runtime_with_fingerprint(TrickVocabulary::compiled)
            .0
    }

    fn prepare_sparkle_runtime_with_fingerprint(
        &self,
        tricks: fn(Vec<String>) -> TrickVocabulary,
    ) -> (PreparedSparkleRuntime, u64) {
        let (parts, fingerprint) = self.sparkle_runtime_parts_with_fingerprint();
        let langs = self.sparkle_languages();
        let tricks = tricks(langs.clone());
        let resolved = parts.map(|(cfg, override_toml)| {
            let refs: Vec<&str> = langs.iter().map(String::as_str).collect();
            let lexicon = aterm_lexicon::Lexicon::with_languages_and_override(
                &refs,
                override_toml.as_deref(),
            )
            .unwrap_or_else(|error| {
                crate::logging::stderr_line!(
                    "aterm-gui: sparkle words lexicon override rejected ({error}); using builtin"
                );
                aterm_lexicon::Lexicon::with_languages(&refs)
            });
            for warning in sparkle_logged_warnings(lexicon.conflicts(), cfg.cjk_single_char) {
                crate::logging::stderr_line!("aterm-gui: sparkle_words lexicon: {warning}");
            }
            crate::word_decorations::Resolved {
                cfg,
                lexicon: std::sync::Arc::new(lexicon),
            }
        });
        (PreparedSparkleRuntime { resolved, tricks }, fingerprint)
    }

    /// Files this config names by path whose content is applied state.
    pub(crate) fn path_feed_paths(&self) -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        if let Some(sparkle) = self
            .sparkle_words
            .as_ref()
            .filter(|sparkle| sparkle.enabled != Some(false))
        {
            paths.extend(sparkle.lexicon.as_deref().map(sparkle_expand_tilde));
            paths.extend(
                sparkle
                    .toy_packs
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .take(MAX_ACTIVE_TOY_PACKS)
                    .map(|path| sparkle_expand_tilde(path)),
            );
        }
        paths.extend(self.ai.fleet_paths.iter().cloned());
        paths
    }

    /// Every file besides `aterm.toml` whose content a warm of this config presents, by the path its reader opens (warm successor P2).
    pub(crate) fn warm_read_paths(&self) -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        if let Some(directory) = aterm_types::scheme::user_theme_dir() {
            for appearance in [
                aterm_types::Appearance::Dark,
                aterm_types::Appearance::Light,
            ] {
                let Some(name) = self.resolve_theme_name(appearance) else {
                    continue;
                };
                if aterm_types::scheme::builtin(&name).is_some()
                    || safe_user_theme_name(&name).is_err()
                {
                    continue;
                }
                let path = directory.join(format!("{name}.conf"));
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        for raw in [
            self.cursor_nyan_sprite.as_deref(),
            self.wallpaper.as_deref(),
        ] {
            if let Some(raw) = raw
                .map(str::trim)
                .filter(|raw| !raw.is_empty() && raw.len() <= MAX_KITTY_SOURCE_ID_BYTES)
            {
                paths.push(sparkle_expand_tilde(raw));
            }
        }
        paths.extend(self.path_feed_paths());
        paths
    }

    /// Prepare every path-backed effect consumer and identify it from the exact same-handle bytes that entered its parser.
    pub(crate) fn prepare_path_feed_generation(&self) -> PreparedPathFeedGeneration {
        self.prepare_path_feed_generation_with(TrickVocabulary::compiled)
    }

    /// [`Self::prepare_path_feed_generation`] for the launch's config-runtime worker, which the boot joins before the first window.
    pub(crate) fn prepare_launch_path_feed_generation(&self) -> PreparedPathFeedGeneration {
        self.prepare_path_feed_generation_with(TrickVocabulary::compiling)
    }

    fn prepare_path_feed_generation_with(
        &self,
        tricks: fn(Vec<String>) -> TrickVocabulary,
    ) -> PreparedPathFeedGeneration {
        let (sparkle, deco) = self.prepare_sparkle_runtime_with_fingerprint(tricks);
        PreparedPathFeedGeneration {
            sparkle,
            fingerprints: PathFeedFps { deco },
        }
    }

    /// The `[sparkle_words] enabled` master, resolved in one place.
    pub(crate) fn sparkle_words_enabled_or_default(&self) -> bool {
        self.sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.enabled)
            .unwrap_or(true)
    }

    #[cfg(test)]
    pub(crate) fn sparkle_deco_config(&self) -> Option<crate::word_decorations::DecoConfig> {
        if self
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.enabled)
            == Some(false)
        {
            return None;
        }
        let packs = self.sparkle_toy_packs();
        self.sparkle_deco_config_with_pack_specs(packs.spec_table)
    }

    fn sparkle_deco_config_with_pack_specs(
        &self,
        mut spec_table: aterm_effects::spec::SpecTable,
    ) -> Option<crate::word_decorations::DecoConfig> {
        let sw = self.sparkle_words.clone().unwrap_or_default();
        if !sw.enabled.unwrap_or(true) {
            return None;
        }
        let prof = sw.profanity.clone().unwrap_or_default();
        let fel = sw.feline.clone().unwrap_or_default();
        let can_cfg = sw.canine.clone().unwrap_or_default();
        let ink_cfg = sw.ink.clone().unwrap_or_default();
        let emph = sw.emphasis.clone().unwrap_or_default();
        let sparkle_words = prof.enabled.unwrap_or(true);
        let profanity = sparkle_words;
        let feline = fel.enabled.unwrap_or(true);
        let canine = sparkle_words && can_cfg.enabled.unwrap_or(true);
        let ink_enabled = ink_cfg.enabled.unwrap_or(true);
        let custom_entries = sw.custom.clone().unwrap_or_default();
        let (inline_specs, _) = aterm_effects::spec::build_custom(&custom_entries);
        if sparkle_words {
            spec_table.overlay(inline_specs);
        } else {
            spec_table = aterm_effects::spec::SpecTable::default();
        }
        let emphasis = sparkle_words
            && emph.enabled.unwrap_or(true)
            && (ink_enabled || spec_table.has_custom());
        if !profanity && !feline && !canine && !emphasis {
            return None;
        }
        let ink_loop = ink_cfg.loop_.unwrap_or(false);
        let ink_sweep_floor = if ink_loop { 600 } else { 350 };
        let ink_sweep_ms = ink_cfg
            .sweep_ms
            .unwrap_or(2200)
            .clamp(ink_sweep_floor, 6000);
        let ink_strength = ink_cfg.strength.unwrap_or(0.75).clamp(0.0, 1.0);
        let palette: Vec<u32> = prof
            .palette
            .unwrap_or_default()
            .iter()
            .filter_map(|s| aterm_types::scheme::parse_hex(s))
            .map(rgb_u32)
            .collect();
        let reduced_motion = sw.reduced_motion.unwrap_or(false);
        let suppress_in_alt_screen = sw.suppress_in_alt_screen.unwrap_or(false);
        let mut ignore: std::collections::HashSet<String> = std::collections::HashSet::new();
        for words in [
            &sw.deny,
            &prof.ignore_words,
            &fel.ignore_words,
            &can_cfg.ignore_words,
            &emph.ignore_words,
        ]
        .into_iter()
        .flatten()
        {
            ignore.extend(words.iter().map(|w| aterm_lexicon::fold(w)));
        }
        Some(crate::word_decorations::DecoConfig {
            profanity,
            feline,
            canine,
            emphasis,
            ink_enabled,
            ink_strength,
            ink_sweep_ms,
            ink_loop,
            reduced_motion,
            suppress_in_alt_screen,
            allow_bare_cat: fel.allow_bare_cat.unwrap_or(true),
            cjk_single_char: fel.cjk_single_char.unwrap_or(false),
            feline_style: if fel
                .style
                .as_deref()
                .is_some_and(|style| style.trim().eq_ignore_ascii_case("paw"))
            {
                crate::word_decorations::FelineStyle::Paw
            } else {
                crate::word_decorations::FelineStyle::Cat
            },
            feline_magic: fel.magic.unwrap_or(true),
            profanity_style: match prof.style.as_deref().map(str::trim) {
                Some(s) if s.eq_ignore_ascii_case("sparkle") => {
                    crate::word_decorations::ProfanityStyle::Sparkle
                }
                Some(s) if s.eq_ignore_ascii_case("nova") => {
                    crate::word_decorations::ProfanityStyle::Nova
                }
                _ => crate::word_decorations::ProfanityStyle::Rainbow,
            },
            profanity_magic: prof.magic.unwrap_or(true),
            supernova_chance: prof.supernova_chance.unwrap_or(10).min(100) as u8,
            spec_table,
            palette,
            density: prof.density.unwrap_or(3).clamp(1, 12),
            anim_ms: prof.anim_ms.unwrap_or(2500).clamp(350, 10_000),
            jitter: prof.jitter.unwrap_or(2).clamp(0, 6),
            intensity: prof.intensity.unwrap_or(0.85).clamp(0.0, 1.0),
            ignore,
            glyphs: vec![
                aterm_render::DecoGlyph::Star4,
                aterm_render::DecoGlyph::Star5,
                aterm_render::DecoGlyph::Dot,
                aterm_render::DecoGlyph::Plus,
            ],
        })
    }

    #[cfg(test)]
    pub(crate) fn sparkle_override_toml(&self) -> Option<String> {
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        let lexicon = self.admitted_sparkle_lexicon(&mut fingerprint);
        let packs = self.sparkle_toy_packs_with_fingerprint(&mut fingerprint);
        self.sparkle_override_toml_with_admitted_lexicon(&packs.lexicon_toml, lexicon.as_ref())
    }

    fn sparkle_override_toml_with_admitted_lexicon(
        &self,
        pack_lexicon_toml: &str,
        admitted_lexicon: Option<&AdmittedPathFeed>,
    ) -> Option<String> {
        let sw = self.sparkle_words.as_ref()?;
        let mut out = String::new();
        if let Some((expanded, admitted)) = admitted_lexicon {
            match admitted {
                Ok(contents) => {
                    let languages = self.sparkle_languages();
                    let refs: Vec<&str> = languages.iter().map(String::as_str).collect();
                    match aterm_lexicon::Lexicon::with_languages_and_override(&refs, Some(contents))
                    {
                        Ok(_) => {
                            out.push_str(contents);
                            if !contents.ends_with('\n') {
                                out.push('\n');
                            }
                        }
                        Err(e) => crate::logging::stderr_line!(
                            "aterm-gui: sparkle_words.lexicon {expanded:?} rejected ({e}); \
                             skipping that layer"
                        ),
                    }
                }
                Err(error) => {
                    crate::logging::stderr_line!(
                        "aterm-gui: sparkle_words.lexicon {expanded:?} unreadable ({error}); \
                         skipping that layer"
                    );
                }
            }
        }
        let prof = sw.profanity.clone().unwrap_or_default();
        let fel = sw.feline.clone().unwrap_or_default();
        let can_cfg = sw.canine.clone().unwrap_or_default();
        let emph = sw.emphasis.clone().unwrap_or_default();
        append_extra_words_entry(&mut out, "profanity", prof.extra_words.as_deref());
        append_extra_words_entry(&mut out, "feline", fel.extra_words.as_deref());
        append_extra_words_entry(&mut out, "canine", can_cfg.extra_words.as_deref());
        append_extra_words_entry(&mut out, "emphasis", emph.extra_words.as_deref());
        out.push_str(pack_lexicon_toml);
        if let Some(customs) = sw.custom.as_deref() {
            let (_, fragment) = aterm_effects::spec::build_custom(customs);
            out.push_str(&fragment);
        }
        (!out.is_empty()).then_some(out)
    }

    /// The durable `[matrix_rain]` enabled bit.
    pub(crate) fn matrix_rain_enabled(&self) -> bool {
        self.matrix_rain
            .as_ref()
            .and_then(|mr| mr.enabled)
            .unwrap_or(false)
    }

    /// The `[render] repaint_nudge` mode.
    pub(crate) fn repaint_nudge_mode(&self) -> aterm_core::terminal::repaint_nudge::Mode {
        use aterm_core::terminal::repaint_nudge::Mode;
        self.render
            .as_ref()
            .and_then(|r| r.repaint_nudge.as_deref())
            .map_or_else(Mode::default, |word| {
                Mode::parse(word.trim()).unwrap_or(Mode::Log)
            })
    }

    /// The `[presence] rim` resolved bit (default true).
    pub(crate) fn presence_rim_enabled(&self) -> bool {
        self.presence.as_ref().and_then(|p| p.rim).unwrap_or(true)
    }

    /// The `[claude_footer] usage` bit (default on).
    pub(crate) fn claude_footer_usage(&self) -> bool {
        self.claude_footer
            .as_ref()
            .and_then(|c| c.usage)
            .unwrap_or(true)
    }

    /// The `[claude_footer] card` bit (default on).
    pub(crate) fn claude_footer_card(&self) -> bool {
        self.claude_footer
            .as_ref()
            .and_then(|c| c.card)
            .unwrap_or(true)
    }

    /// The `[claude_footer] acts` resolved bit (default true).
    pub(crate) fn claude_footer_acts(&self) -> bool {
        self.claude_footer
            .as_ref()
            .and_then(|c| c.acts)
            .unwrap_or(true)
    }

    /// The `[claude_footer] agents` resolved bit (default true).
    pub(crate) fn claude_footer_agents(&self) -> bool {
        self.claude_footer
            .as_ref()
            .and_then(|c| c.agents)
            .unwrap_or(true)
    }

    /// `[harness.<agent>] enabled` as the supervisor's reader took it.
    pub(crate) fn harness_agent_enabled(&self, agent: aterm_phase::Program) -> bool {
        !self.harness.policy.off.contains(&agent)
    }

    /// The `[harness]` reader's notes as config notices (`config harness.<key>: …`), each with its resolved value.
    pub(crate) fn harness_notices(&self) -> Vec<String> {
        use aterm_agent::supervise::config::{is_retired_key_note, is_unknown_key_note};
        self.harness
            .notes
            .iter()
            .filter(|note| !is_unknown_key_note(note) && !is_retired_key_note(note))
            .map(|note| format!("config {note}"))
            .collect()
    }

    /// `[packages] enabled` (default true), folding `auto_update = false` in as atpkg does (`atpkg::config::PackagesConfig::enabled`).
    pub(crate) fn packages_enabled(&self) -> bool {
        self.packages
            .as_ref()
            .is_none_or(|p| p.enabled.unwrap_or(true) && p.auto_update != Some(false))
    }

    /// Why [`Self::packages_enabled`] reads off when a legacy `auto_update = false` holds it, in atpkg's words (`atpkg::config::auto_update_note`).
    pub(crate) fn packages_retired_switch_note(&self) -> Option<String> {
        let packages = self.packages.as_ref()?;
        (packages.auto_update == Some(false) && packages.enabled != Some(false))
            .then(|| atpkg::config::auto_update_note(false, packages.enabled))
    }

    /// `[packages] auto_install` (default true), reading legacy `seed_install` when unset, as atpkg does (`atpkg::config::PackagesConfig::auto_install`).
    pub(crate) fn packages_auto_install(&self) -> bool {
        self.packages
            .as_ref()
            .and_then(|p| p.auto_install.or(p.seed_install))
            .unwrap_or(true)
    }

    /// `[machine] universal_control`, resolved like `atpkg::config::MachineConfig::universal_control` (absent, empty, `"off"` ⇒ `Off`; anything else ⇒ `Leave`).
    pub(crate) fn machine_universal_control(&self) -> atpkg::config::UniversalControlPolicy {
        match self
            .machine
            .as_ref()
            .and_then(|m| m.universal_control.as_deref())
            .map(str::trim)
        {
            None | Some("") | Some("off") => atpkg::config::UniversalControlPolicy::Off,
            Some(_) => atpkg::config::UniversalControlPolicy::Leave,
        }
    }

    /// `[machine] rosetta`, resolved as `atpkg::config::MachineConfig::rosetta` (absent ⇒ `true`).
    pub(crate) fn machine_rosetta(&self) -> bool {
        self.machine
            .as_ref()
            .and_then(|m| m.rosetta)
            .unwrap_or(true)
    }

    /// The `[privacy]` master bit (default true).
    pub(crate) fn privacy_enabled(&self) -> bool {
        self.privacy
            .as_ref()
            .and_then(|p| p.enabled)
            .unwrap_or(true)
    }

    /// The `[privacy]` `check` bit (default true).
    pub(crate) fn privacy_check(&self) -> bool {
        self.privacy.as_ref().and_then(|p| p.check).unwrap_or(true)
    }

    /// The `[privacy]` `notice` bit (default true).
    pub(crate) fn privacy_notice(&self) -> bool {
        self.privacy.as_ref().and_then(|p| p.notice).unwrap_or(true)
    }

    /// The `[privacy]` `report_attribution` bit (default true).
    pub(crate) fn privacy_report_attribution(&self) -> bool {
        self.privacy
            .as_ref()
            .and_then(|p| p.report_attribution)
            .unwrap_or(true)
    }

    /// Both probe bits as the consent module's gate type, so the master always outranks `check`.
    pub(crate) fn privacy_probe_gate(&self) -> aterm_containment::ProbeGate {
        aterm_containment::ProbeGate {
            enabled: self.privacy_enabled(),
            check: self.privacy_check(),
        }
    }

    /// The `[privacy]` `warmup` mode (default [`PrivacyWarmup::Automatic`]).
    pub(crate) fn privacy_warmup(&self) -> PrivacyWarmup {
        self.privacy
            .as_ref()
            .and_then(|p| p.warmup.as_deref())
            .and_then(PrivacyWarmup::parse)
            .unwrap_or_default()
    }

    /// The `[privacy]` `warmup_folders` names (default `aterm_containment::Folder::WARMUP_DEFAULT`).
    pub(crate) fn privacy_warmup_folders(&self) -> Vec<String> {
        self.privacy
            .as_ref()
            .and_then(|p| p.warmup_folders.clone())
            .unwrap_or_else(|| {
                aterm_containment::Folder::WARMUP_DEFAULT
                    .iter()
                    .map(|folder| folder.as_str().to_string())
                    .collect()
            })
    }

    /// Resolved `[privacy] warmup_hold_ms` (default [`PRIVACY_WARMUP_HOLD_MS_DEFAULT`]), clamped to [`PRIVACY_WARMUP_HOLD_MS_MAX`].
    pub(crate) fn privacy_warmup_hold_ms(&self) -> u64 {
        self.privacy
            .as_ref()
            .and_then(|p| p.warmup_hold_ms)
            .unwrap_or(PRIVACY_WARMUP_HOLD_MS_DEFAULT)
            .min(PRIVACY_WARMUP_HOLD_MS_MAX)
    }

    /// Resolved `[privacy] probe_interval_ms` floor (default [`PRIVACY_PROBE_INTERVAL_MS_DEFAULT`]), also the worst-case posture lag.
    pub(crate) fn privacy_probe_interval_ms(&self) -> u64 {
        self.privacy
            .as_ref()
            .and_then(|p| p.probe_interval_ms)
            .unwrap_or(PRIVACY_PROBE_INTERVAL_MS_DEFAULT)
    }

    /// The `[privacy]` `observer` key as a mode.
    pub(crate) fn privacy_observer_mode(&self) -> crate::consent_observer::ObserverMode {
        use crate::consent_observer::ObserverMode;
        match self.privacy.as_ref().and_then(|p| p.observer) {
            None => ObserverMode::Sentinel,
            Some(false) => ObserverMode::Off,
            Some(true) => ObserverMode::Full,
        }
    }

    /// `[privacy] observer = false`.
    pub(crate) fn privacy_observer_off(&self) -> bool {
        self.privacy_observer_mode() == crate::consent_observer::ObserverMode::Off
    }

    /// `[privacy] protected_roots` resolved to absolute paths.
    pub(crate) fn privacy_protected_roots(&self) -> Vec<std::path::PathBuf> {
        let overrides = self
            .privacy
            .as_ref()
            .and_then(|p| p.protected_roots.as_deref())
            .unwrap_or(&[]);
        aterm_containment::protected_roots(overrides)
    }

    /// Resolve `[matrix_rain]` into [`crate::matrix_rain::RainConfig`] with every default and clamp (the engine re-clamps).
    pub(crate) fn matrix_rain_params(
        &self,
        default_bg: u32,
        theme_fg: u32,
        theme_accent: u32,
    ) -> crate::matrix_rain::RainConfig {
        use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainHue};
        let mr = self.matrix_rain.clone().unwrap_or_default();
        let alpha = mr
            .alpha
            .map(|a| a.clamp(u32::from(RAIN_ALPHA_FLOOR), u32::from(RAIN_ALPHA_CAP)) as u8);
        let head_floor = alpha.unwrap_or(RAIN_ALPHA_FLOOR);
        let head_alpha = mr
            .head_alpha
            .map(|a| (a.min(u32::from(RAIN_ALPHA_CAP)) as u8).max(head_floor));
        let hue = match mr.hue.as_deref().map(str::trim) {
            Some(h) if h.eq_ignore_ascii_case("theme") => RainHue::Theme,
            Some(h) if h.starts_with('#') => match aterm_types::scheme::parse_hex(h) {
                Some(c) => RainHue::Custom(rgb_u32(c)),
                None => RainHue::Matrix,
            },
            _ => RainHue::Matrix,
        };
        let _ = mr.materialize;
        let _ = mr.ink_text;
        let _ = mr.phosphor;
        crate::matrix_rain::RainConfig {
            enabled: true,
            fps: mr.fps.unwrap_or(30).clamp(12, 60) as u8,
            density: mr.density.unwrap_or(6).clamp(1, 12) as u8,
            speed: mr.speed.unwrap_or(5).clamp(1, 10) as u8,
            trail: mr.trail.unwrap_or(5).clamp(1, 10) as u8,
            alpha_override: alpha,
            head_alpha_override: head_alpha,
            hue,
            mutation_ms: mr.mutation_ms.unwrap_or(133).clamp(80, 2000) as u16,
            idle_secs: mr.idle_secs.unwrap_or(8).clamp(2, 120) as u16,
            suppress_in_alt_screen: mr.suppress_in_alt_screen.unwrap_or(false),
            turn_wave: mr.turn_wave.unwrap_or(true),
            output_material: mr.output_material.unwrap_or(true),
            exit_tint: mr.exit_tint.unwrap_or(true),
            bell_alert: mr.bell_alert.unwrap_or(true),
            seed: mr.seed.unwrap_or(0),
            default_bg: default_bg & 0x00FF_FFFF,
            theme_fg: theme_fg & 0x00FF_FFFF,
            theme_accent: theme_accent & 0x00FF_FFFF,
        }
    }

    /// The `[output_streak]` master (`docs/DESIGN-output-streak-2026-08-30.md` §7), default [`DEFAULT_DECORATIVE_EFFECTS`].
    pub(crate) fn output_streak_enabled_or_default(&self) -> bool {
        self.output_streak
            .as_ref()
            .and_then(|streak| streak.enabled)
            .unwrap_or(DEFAULT_DECORATIVE_EFFECTS)
    }

    /// Whether the output streak plays sound (`[output_streak] sound`, default on).
    pub(crate) fn output_streak_sound_or_default(&self) -> bool {
        self.output_streak
            .as_ref()
            .and_then(|streak| streak.sound)
            .unwrap_or(true)
    }

    /// Output streak amplitude scale (`[output_streak] intensity`, default `1.0`), clamped `0..=1`.
    pub(crate) fn output_streak_intensity_or_default(&self) -> f32 {
        match self
            .output_streak
            .as_ref()
            .and_then(|streak| streak.intensity)
        {
            Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
            Some(_) => 0.0,
            None => 1.0,
        }
    }

    /// Comet tail length in cells (`[output_streak] tail`, default 9), clamped into [`aterm_effects::output_streak::TAIL_MIN`]`..=`[`aterm_effects::output_streak::TAIL_MAX`], the engine's own bounds.
    pub(crate) fn output_streak_tail_or_default(&self) -> u16 {
        use aterm_effects::output_streak::{TAIL_MAX, TAIL_MIN};
        let raw = self
            .output_streak
            .as_ref()
            .and_then(|streak| streak.tail)
            .unwrap_or(9);
        raw.clamp(u32::from(TAIL_MIN), u32::from(TAIL_MAX)) as u16
    }

    /// Resident comet cap (`[output_streak] max_streaks`, default 3), clamped `1..=`[`aterm_effects::output_streak::MAX_COMETS`].
    pub(crate) fn output_streak_max_streaks_or_default(&self) -> u8 {
        use aterm_effects::output_streak::MAX_COMETS;
        let raw = self
            .output_streak
            .as_ref()
            .and_then(|streak| streak.max_streaks)
            .unwrap_or(3);
        raw.clamp(1, MAX_COMETS as u32) as u8
    }

    /// Idle drain in seconds (`[output_streak] idle_secs`, default 10), clamped `2..=120`.
    pub(crate) fn output_streak_idle_secs_or_default(&self) -> f32 {
        match self
            .output_streak
            .as_ref()
            .and_then(|streak| streak.idle_secs)
        {
            Some(value) if value.is_finite() => value.clamp(2.0, 120.0),
            Some(_) => 2.0,
            None => 10.0,
        }
    }

    #[cfg(test)]
    pub(crate) fn output_streak_warning(&self) -> Option<String> {
        let streak = self.output_streak.as_ref()?;
        (streak.sound == Some(true) && !self.output_streak_enabled_or_default()).then(|| {
            "output_streak.sound is configured as true but is effectively silent because \
             output_streak is disabled; a streak pip is only ever recorded on an edge that \
             also mints light"
                .to_string()
        })
    }

    /// GPU cursor bloom, the light crown around the comet head.
    pub(crate) fn cursor_trail_bloom_or_default(&self) -> bool {
        self.cursor_trail_bloom.unwrap_or(true)
    }

    /// Bloom strength, default 0.85, clamped 0.0..=3.0.
    pub(crate) fn cursor_trail_bloom_strength_or_default(&self) -> f32 {
        let v = self.cursor_trail_bloom_strength.unwrap_or(0.85);
        if v.is_finite() {
            v.clamp(0.0, 3.0)
        } else {
            0.0
        }
    }

    /// Bloom radius (half-res blur texels) for every style except the rainbow kitty.
    pub(crate) const CURSOR_TRAIL_BLOOM_RADIUS_DEFAULT: f32 = 2.2;

    /// The rainbow kitty's bloom radius (`RAINBOW-KITTY-V2.md` §21 row 14).
    pub(crate) const RAINBOW_KITTY_BLOOM_RADIUS: f32 = 1.8;

    /// The unset bloom radius for a resolved trail style.
    pub(crate) fn bloom_radius_style_default(style: ResolvedTrailStyle) -> f32 {
        if style.style == Some(aterm_effects::cursor_glow::GlowStyle::RainbowKitty) {
            Self::RAINBOW_KITTY_BLOOM_RADIUS
        } else {
            Self::CURSOR_TRAIL_BLOOM_RADIUS_DEFAULT
        }
    }

    /// Bloom radius (half-res texels) for the resolved trail style, clamped 0.5..=8.0.
    pub(crate) fn cursor_trail_bloom_radius_or_default(
        &self,
        presentation: ResolvedTrailPresentation,
    ) -> f32 {
        let per_style = Self::bloom_radius_style_default(presentation.style);
        let v = self.cursor_trail_bloom_radius.unwrap_or(per_style);
        if v.is_finite() {
            v.clamp(0.5, 8.0)
        } else {
            per_style
        }
    }

    /// GPU heat shimmer above burning cells.
    pub(crate) fn cursor_fire_shimmer_or_default(&self) -> bool {
        self.cursor_fire_shimmer.unwrap_or(true)
    }

    /// Whether the edr cursor glow is enabled.
    pub(crate) fn hdr_glow_or_default(&self) -> bool {
        self.hdr_glow.unwrap_or(true)
    }

    /// SDR glow-boost strength (`cursor_glow_sdr_boost`), default 0.25, clamped 0..=1.
    pub(crate) fn cursor_glow_sdr_boost_or_default(&self) -> f32 {
        self.cursor_glow_sdr_boost.unwrap_or(0.25).clamp(0.0, 1.0)
    }

    /// Renderer text-shaping config from `ligatures`, `cursor_break_ligatures`, `font_features` and `merged_ligatures`.
    pub(crate) fn text_shaping(&self) -> aterm_render::TextShapingConfig {
        use aterm_types::text_shaping::{
            FontFeature, FontFeatureSet, LigatureMode, parse_font_features,
        };
        let ligature_mode = match (self.ligatures, self.cursor_break_ligatures) {
            (Some(false), _) => LigatureMode::Disabled,
            (_, Some(true)) => LigatureMode::CursorDisabled,
            _ => LigatureMode::Enabled,
        };
        let features: Vec<FontFeature> = self
            .font_features
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .flat_map(|spec| parse_font_features(spec))
            .collect();
        let font_features = if features.is_empty() {
            Vec::new()
        } else {
            vec![FontFeatureSet {
                font_id: 0,
                features,
            }]
        };
        aterm_render::TextShapingConfig {
            ligature_mode,
            font_features,
            admit_collapsed: self.merged_ligatures.unwrap_or(false),
            ..Default::default()
        }
    }

    /// Coverage blend mode: `"linear-corrected"` (default) or `"linear"`.
    pub(crate) fn text_blending_or_default(&self) -> aterm_render::TextBlending {
        match self.text_blending.as_deref().map(str::trim) {
            None => aterm_render::TextBlending::LinearCorrected,
            Some(s) if s.eq_ignore_ascii_case("linear") => aterm_render::TextBlending::Linear,
            Some(s)
                if s.eq_ignore_ascii_case("linear-corrected")
                    || s.eq_ignore_ascii_case("linear_corrected") =>
            {
                aterm_render::TextBlending::LinearCorrected
            }
            Some(other) => {
                crate::logging::stderr_line!(
                    "aterm-gui: unknown text_blending {other:?} (expected \"linear\" or \
                     \"linear-corrected\"); using linear-corrected"
                );
                aterm_render::TextBlending::LinearCorrected
            }
        }
    }

    /// MacOS `font_thicken` (CoreText font smoothing at raster time).
    pub(crate) fn font_thicken_or_default(&self) -> bool {
        self.font_thicken.unwrap_or(false)
    }

    /// Variable-font requests.
    pub(crate) fn font_variation_requests(&self) -> (Vec<(u32, f32)>, Vec<String>) {
        let mut warns = Vec::new();
        let mut out: Vec<(u32, f32)> = self
            .font_variation
            .as_ref()
            .map_or(&[][..], |l| l.0.as_slice())
            .iter()
            .filter_map(|spec| {
                let parsed = aterm_render::variation::parse_variation_spec(spec);
                if parsed.is_none() {
                    warns.push(format!(
                        "config font_variation: ignored malformed entry {spec:?} \
                         (use \"tag=value\", e.g. \"wght=450\")"
                    ));
                }
                parsed
            })
            .collect();
        if let Some(w) = self.font_weight {
            out.push((aterm_render::variation::WGHT_TAG, w.clamp(1, 1000) as f32));
        }
        (out, warns)
    }

    /// `font_weight_dark_nudge`, default 0 (off), clamped 0..=300.
    pub(crate) const fn font_weight_dark_nudge_or_default(&self) -> f32 {
        match self.font_weight_dark_nudge {
            Some(v) if v.is_finite() => v.clamp(0.0, 300.0),
            _ => 0.0,
        }
    }

    /// Effective stem gamma.
    pub(crate) fn stem_gamma_or_default(&self) -> f32 {
        aterm_render::clamp_stem_gamma(self.stem_gamma.unwrap_or(1.0))
    }

    /// Effective grid-fitting mode.
    pub(crate) fn font_hinting_or_default(&self) -> String {
        self.font_hinting
            .clone()
            .unwrap_or_else(|| "full".to_string())
    }

    /// Effective Linux subpixel mode.
    pub(crate) fn font_subpixel_or_default(&self) -> String {
        self.font_subpixel
            .clone()
            .unwrap_or_else(|| "off".to_string())
    }

    /// Line-height multiplier, default [`DEFAULT_LINE_HEIGHT`] (`1.1` on Windows, `1.0` elsewhere), clamped 0.8..=2.0 (the renderer floors at 0.5).
    pub(crate) const fn line_height_or_default(&self) -> f32 {
        match self.line_height {
            Some(v) if v.is_finite() => v.clamp(0.8, 2.0),
            _ => DEFAULT_LINE_HEIGHT,
        }
    }

    /// The text size scale in thousandths.
    pub(crate) fn font_size_scale_milli_or_default(&self) -> u16 {
        match self.font_size_scale {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0.8..=1.25 first, so the thousandths are 800..=1250"
            )]
            Some(v) if v.is_finite() => (f64::from(v.clamp(0.8, 1.25)) * 1000.0).round() as u16,
            _ => 1000,
        }
    }

    /// Baseline shift in px, default `0`, clamped ±32 (the renderer clamps again at ±64).
    pub(crate) fn adjust_baseline_or_default(&self) -> i32 {
        self.adjust_baseline
            .map_or(0i64, |v| v.clamp(-32, 32))
            .try_into()
            .unwrap_or(0)
    }

    /// Underline position shift in px, default `0`, clamped ±32 (the renderer clamps again at ±64 and re-clamps the band in-cell).
    pub(crate) fn adjust_underline_position_or_default(&self) -> i32 {
        self.adjust_underline_position
            .map_or(0i64, |v| v.clamp(-32, 32))
            .try_into()
            .unwrap_or(0)
    }

    /// Underline thickness delta in px, default `0`, clamped ±32 (the renderer clamps again at ±64 and floors the thickness at 1px).
    pub(crate) fn adjust_underline_thickness_or_default(&self) -> i32 {
        self.adjust_underline_thickness
            .map_or(0i64, |v| v.clamp(-32, 32))
            .try_into()
            .unwrap_or(0)
    }

    /// Descender ink-skip, default on.
    pub(crate) fn underline_skip_descenders_or_default(&self) -> bool {
        self.underline_skip_descenders.unwrap_or(true)
    }

    /// Whether programs may set the 256 indexed colours (OSC 4 / numeric OSC 21), default `true`.
    pub(crate) fn allow_palette_reconfigure_or_default(&self) -> bool {
        self.allow_palette_reconfigure.unwrap_or(true)
    }

    /// Per-cell minimum contrast ratio, default `1.0` (off; xterm treats 1 as "do nothing"), clamped to the WCAG 1.0..=21.0 domain.
    pub(crate) const fn minimum_contrast_or_default(&self) -> f32 {
        match self.minimum_contrast {
            Some(v) if v.is_finite() => v.clamp(1.0, 21.0),
            _ => 1.0,
        }
    }

    /// The engine's theme-pair contrast floor (`TerminalConfig::indexed_minimum_contrast`).
    pub(crate) const fn indexed_minimum_contrast_or_default(&self) -> f32 {
        if self.minimum_contrast.is_some() {
            1.0
        } else {
            DEFAULT_INDEXED_MINIMUM_CONTRAST
        }
    }

    /// Whether the selection band dims while unfocused.
    pub(crate) fn selection_inactive_or_default(&self) -> bool {
        self.selection_inactive.unwrap_or(false)
    }

    /// Whether a split marks the focused pane.
    pub(crate) fn split_focus_mark_or_default(&self) -> bool {
        self.split_focus_mark.unwrap_or(true)
    }

    /// Whether panes show the overlay scrollbar ([`DEFAULT_SCROLLBAR`] when unset).
    pub(crate) fn scrollbar_or_default(&self) -> bool {
        self.scrollbar.unwrap_or(DEFAULT_SCROLLBAR)
    }

    /// SGR 2 faint opacity, default `0.5`, clamped 0.0..=1.0.
    pub(crate) const fn faint_opacity_or_default(&self) -> f32 {
        match self.faint_opacity {
            Some(v) if v.is_finite() => v.clamp(0.0, 1.0),
            _ => aterm_types::DIM_FACTOR,
        }
    }

    /// Window background opacity, default `1.0` (solid), clamped to 0.0..=1.0.
    pub(crate) const fn background_opacity_or_default(&self) -> f32 {
        match self.background_opacity {
            Some(v) if v.is_finite() => v.clamp(0.0, 1.0),
            _ => 1.0,
        }
    }

    /// Window material, default [`BackgroundMaterial::None`].
    pub(crate) fn background_material_or_default(&self) -> BackgroundMaterial {
        match self.background_material.as_deref() {
            None => BackgroundMaterial::None,
            Some(s) => BackgroundMaterial::parse(s).unwrap_or_else(|| {
                crate::logging::stderr_line!(
                    "aterm-gui: config background_material: expected none|hud|sidebar|\
                     under-window, got {s:?}; using none"
                );
                BackgroundMaterial::None
            }),
        }
    }

    /// Window padding in logical px (`window_padding`), default [`crate::PAD_LOGICAL_PX`] (12.0), clamped 0..=[`MAX_WINDOW_PADDING_PX`].
    pub(crate) const fn window_padding_or_default(&self) -> f32 {
        match self.window_padding {
            Some(v) if v.is_finite() => v.clamp(0.0, MAX_WINDOW_PADDING_PX),
            _ => crate::PAD_LOGICAL_PX,
        }
    }

    /// Top padding in logical px (`window_padding_top`), default [`crate::PAD_TOP_LOGICAL_PX`] (2.0), clamped to `0..=window_padding` to match the renderer's `set_pad_top` (`pad_top <= pad`).
    pub(crate) fn window_padding_top_or_default(&self) -> f32 {
        let pad = self.window_padding_or_default();
        match self.window_padding_top {
            Some(v) if v.is_finite() => v.clamp(0.0, pad),
            _ => crate::PAD_TOP_LOGICAL_PX.min(pad),
        }
    }

    /// Effective per-cell minimum contrast.
    pub(crate) fn effective_minimum_contrast(&self) -> f32 {
        aterm_render::vibrancy::effective_minimum_contrast(
            self.minimum_contrast_or_default(),
            self.background_opacity_or_default(),
        )
    }

    /// Warning when a configured `font_family` does not resolve to a font file, for the startup/reload notice and `--validate-config`.
    pub(crate) fn font_family_warning(family: Option<&str>) -> Option<String> {
        Self::font_family_admission(family).err()
    }

    /// Admission verdict behind [`Self::font_family_warning`].
    pub(crate) fn font_family_admission(
        family: Option<&str>,
    ) -> Result<Option<aterm_render::FaceRef>, String> {
        let Some(fam) = family.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        match aterm_render::resolve_config_font(fam, aterm_render::FaceTarget::REGULAR) {
            Ok(face) => Ok(Some(face)),
            Err(error) => Err(format!(
                "font_family {fam:?} is not an admissible font ({error}); keeping the current \
                 working font (see `aterm list-fonts` for resolvable families)"
            )),
        }
    }

    /// Warning when `cursor_trail_style` is an unrecognized spelling (it falls back to [`crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE`]).
    pub(crate) fn cursor_trail_style_warning(&self) -> Option<String> {
        let raw = self.cursor_trail_style.as_deref().map(str::trim)?;
        if !resolve_trail_style(raw).unknown {
            return None;
        }
        Some(format!(
            "cursor_trail_style: unknown style {raw:?}{suggestion} (falling back to \
             {default:?}; expected one of {options}, or a documented alias like \
             nyan rainbow/nyan/rainbow/tall rainbow/ember/ocean)",
            default = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
            options = crate::prefs::CURSOR_TRAIL_STYLES.join("|"),
            suggestion = crate::native_config_language::nearest_enum_value(
                raw,
                &crate::prefs::CURSOR_TRAIL_STYLES
                    .iter()
                    .copied()
                    .chain(
                        crate::prefs::CURSOR_TRAIL_STYLE_ALIASES
                            .iter()
                            .map(|&(alias, _)| alias),
                    )
                    .collect::<Vec<_>>(),
            )
            .and_then(crate::prefs::cursor_trail_style_canonical)
            .map_or_else(String::new, |near| format!(" — did you mean {near:?}?")),
        ))
    }

    /// The stats bars' form (`[sys] stats_bars`).
    pub(crate) fn stats_bars_or_default(&self) -> crate::stats_deck::StatsBars {
        crate::stats_deck::StatsBars::parse(
            self.sys.as_ref().and_then(|sys| sys.stats_bars.as_deref()),
        )
    }

    /// The Deck's size setting (`[sys] stats_deck_size`).
    pub(crate) fn stats_deck_size_or_default(&self) -> crate::stats_plan::DeckSizePref {
        crate::stats_plan::DeckSizePref::parse(
            self.sys
                .as_ref()
                .and_then(|sys| sys.stats_deck_size.as_deref()),
        )
    }

    /// `[sys] disk_roots` as absolute folders.
    pub(crate) fn sys_disk_roots(&self, home: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
        self.sys
            .as_ref()
            .and_then(|sys| sys.disk_roots.as_deref())
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| match entry.strip_prefix("~/") {
                Some(rest) => home.map(|h| h.join(rest)),
                None => Some(std::path::PathBuf::from(entry)).filter(|p| p.is_absolute()),
            })
            .collect()
    }

    /// Whether to show the floating top-right build/version pill.
    pub(crate) fn show_build_badge_or_default(&self) -> bool {
        self.show_build_badge.unwrap_or(false)
    }

    /// Window-chrome appearance ([`WindowTheme`]).
    pub(crate) fn window_theme_or_default(&self) -> WindowTheme {
        match self.window_theme.as_deref() {
            None => WindowTheme::Auto,
            Some(s) => match WindowTheme::parse(s) {
                Some(t) => t,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config window_theme: expected auto|light|dark, got {s:?}; using auto"
                    );
                    WindowTheme::Auto
                }
            },
        }
    }

    /// Grid right-click gesture ([`RightClickGesture`]) from `right_click`.
    pub(crate) fn right_click_or_default(&self) -> RightClickGesture {
        match self.right_click.as_deref() {
            None => RightClickGesture::PLATFORM_DEFAULT,
            Some(s) => match RightClickGesture::parse(s) {
                Some(g) => g,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config right_click: expected copy_paste|off, got {s:?}; using the platform default"
                    );
                    RightClickGesture::PLATFORM_DEFAULT
                }
            },
        }
    }

    /// Tab-context-menu keyboard chord ([`TabMenuChord`]) from `tab_menu_chord`.
    #[cfg(any(not(target_os = "macos"), test))]
    pub(crate) fn tab_menu_chord_or_default(&self) -> TabMenuChord {
        match self.tab_menu_chord.as_deref() {
            None => TabMenuChord::MenuKey,
            Some(s) => match TabMenuChord::parse(s) {
                Some(g) => g,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config tab_menu_chord: expected on|menu_key|off, got {s:?}; using menu_key"
                    );
                    TabMenuChord::MenuKey
                }
            },
        }
    }

    /// GPU-present colour-space tag ([`WindowColorspace`]) from `window_colorspace`.
    pub(crate) fn window_colorspace_or_default(&self) -> WindowColorspace {
        match self.window_colorspace.as_deref() {
            None => WindowColorspace::Srgb,
            Some(s) => match WindowColorspace::parse(s) {
                Some(c) => c,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config window_colorspace: expected srgb|display-p3, got {s:?}; using srgb"
                    );
                    WindowColorspace::Srgb
                }
            },
        }
    }

    /// In-grid tab band height ([`TabBandHeight`]) from `tab_band_height`.
    pub(crate) fn tab_band_height_or_default(&self) -> TabBandHeight {
        match self.tab_band_height.as_deref() {
            None => TabBandHeight::PLATFORM_DEFAULT,
            Some(s) => match TabBandHeight::parse(s) {
                Some(h) => h,
                None => {
                    crate::logging::stderr_line!(
                        "aterm-gui: config tab_band_height: expected compact|standard, got {s:?}; using the platform default"
                    );
                    TabBandHeight::PLATFORM_DEFAULT
                }
            },
        }
    }
}

/// Renderer typography/appearance knobs resolved from config.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct RenderKnobs {
    pub(crate) line_height: f32,
    pub(crate) minimum_contrast: f32,
    pub(crate) selection_fg: Option<u32>,
    pub(crate) selection_inactive: bool,
    pub(crate) adjust_baseline: i32,
    pub(crate) adjust_underline: (i32, i32),
    pub(crate) underline_skip_descenders: bool,
    pub(crate) background_opacity: f32,
    pub(crate) background_material: BackgroundMaterial,
}

/// One changed renderer knob from a config hot-reload.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum KnobChange {
    LineHeight(f32),
    MinimumContrast(f32),
    SelectionFg(Option<u32>),
    SelectionInactive(bool),
    AdjustBaseline(i32),
    AdjustUnderline(i32, i32),
    UnderlineSkipDescenders(bool),
    BackgroundOpacity(f32),
    BackgroundMaterial(BackgroundMaterial),
}

impl RenderKnobs {
    /// Resolve the knob values from a config and the colours it resolves to.
    pub(crate) fn from_config(
        cfg: &Config,
        colors: &crate::resolved_colors::ResolvedColors,
    ) -> Self {
        Self {
            line_height: cfg.line_height_or_default(),
            minimum_contrast: cfg.effective_minimum_contrast(),
            selection_fg: colors.selection_fg().map(rgb_u32),
            selection_inactive: cfg.selection_inactive_or_default(),
            adjust_baseline: cfg.adjust_baseline_or_default(),
            adjust_underline: (
                cfg.adjust_underline_position_or_default(),
                cfg.adjust_underline_thickness_or_default(),
            ),
            underline_skip_descenders: cfg.underline_skip_descenders_or_default(),
            background_opacity: cfg.background_opacity_or_default(),
            background_material: cfg.background_material_or_default(),
        }
    }

    /// The changes from `self` to `new`, one [`KnobChange`] per changed knob (an unchanged knob emits nothing, so a metadata-only reload is free).
    pub(crate) fn diff(&self, new: &Self) -> Vec<KnobChange> {
        let mut out = Vec::new();
        if (self.line_height - new.line_height).abs() > f32::EPSILON {
            out.push(KnobChange::LineHeight(new.line_height));
        }
        if (self.minimum_contrast - new.minimum_contrast).abs() > f32::EPSILON {
            out.push(KnobChange::MinimumContrast(new.minimum_contrast));
        }
        if self.selection_fg != new.selection_fg {
            out.push(KnobChange::SelectionFg(new.selection_fg));
        }
        if self.selection_inactive != new.selection_inactive {
            out.push(KnobChange::SelectionInactive(new.selection_inactive));
        }
        if self.adjust_baseline != new.adjust_baseline {
            out.push(KnobChange::AdjustBaseline(new.adjust_baseline));
        }
        if self.adjust_underline != new.adjust_underline {
            out.push(KnobChange::AdjustUnderline(
                new.adjust_underline.0,
                new.adjust_underline.1,
            ));
        }
        if self.underline_skip_descenders != new.underline_skip_descenders {
            out.push(KnobChange::UnderlineSkipDescenders(
                new.underline_skip_descenders,
            ));
        }
        if (self.background_opacity - new.background_opacity).abs() > f32::EPSILON {
            out.push(KnobChange::BackgroundOpacity(new.background_opacity));
        }
        if self.background_material != new.background_material {
            out.push(KnobChange::BackgroundMaterial(new.background_material));
        }
        out
    }
}
/// Renderer settings that bake into rasterized coverage or the cell box, whose setters drop the glyph cache on change.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct GlyphRasterKnobs {
    pub(crate) text_shaping: aterm_render::TextShapingConfig,
    pub(crate) font_thicken: bool,
    pub(crate) stem_gamma: f32,
    pub(crate) font_hinting: String,
    pub(crate) line_height: f32,
    pub(crate) adjust_baseline: i32,
}

impl GlyphRasterKnobs {
    /// Resolve the knobs from config.
    pub(crate) fn from_config(cfg: &Config) -> Self {
        Self {
            text_shaping: cfg.text_shaping(),
            font_thicken: cfg.font_thicken_or_default(),
            stem_gamma: cfg.stem_gamma_or_default(),
            font_hinting: cfg.font_hinting_or_default(),
            line_height: cfg.line_height_or_default(),
            adjust_baseline: cfg.adjust_baseline_or_default(),
        }
    }
}

impl Default for RenderKnobs {
    fn default() -> Self {
        let config = Config::default();
        let colors = config.resolved_colors(
            aterm_types::Appearance::Dark,
            &ThemeCatalog::default(),
            None,
        );
        Self::from_config(&config, &colors)
    }
}

/// Resolved styled/fallback font config (faces and file paths, not family names).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct FontConfig {
    pub(crate) styled_paths: [Option<aterm_render::FaceRef>; 3],
    pub(crate) synthetic_style: bool,
    pub(crate) fallback_fonts: Vec<String>,
    pub(crate) symbol_font: Option<String>,
    pub(crate) emoji_font: Option<String>,
    /// The display layer drawn over the primary (`display_font`, or a `--font display:` flag).
    pub(crate) display_faces: Vec<&'static str>,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            styled_paths: [None, None, None],
            synthetic_style: true,
            fallback_fonts: Vec::new(),
            symbol_font: None,
            emoji_font: None,
            display_faces: Vec::new(),
        }
    }
}

impl FontConfig {
    /// Resolve the font keys into paths plus warnings for entries that do not resolve (skipped, never a hard failure).
    pub(crate) fn from_config(cfg: &Config) -> (Self, Vec<String>) {
        let mut warns = Vec::new();
        let mut resolve = |key: &str,
                           fam: Option<&str>,
                           target: aterm_render::FaceTarget|
         -> Option<aterm_render::FaceRef> {
            let fam = fam.map(str::trim).filter(|s| !s.is_empty())?;
            match aterm_render::resolve_config_font(fam, target) {
                Ok(face) => Some(face),
                Err(error) => {
                    warns.push(format!(
                        "config {key}: {fam:?} is not an admissible font ({error}); ignored \
                         (see `aterm list-fonts`)"
                    ));
                    None
                }
            }
        };
        let styled = [
            ("font_family_bold", cfg.font_family_bold.as_deref()),
            ("font_family_italic", cfg.font_family_italic.as_deref()),
            (
                "font_family_bold_italic",
                cfg.font_family_bold_italic.as_deref(),
            ),
        ];
        let styled_paths = std::array::from_fn(|slot| {
            let (key, fam) = styled[slot];
            resolve(key, fam, aterm_render::FaceTarget::for_slot(slot))
        });
        let regular = aterm_render::FaceTarget::REGULAR;
        let fallback_fonts: Vec<String> = cfg
            .fallback_fonts
            .as_ref()
            .map(|l| l.0.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|f| resolve("fallback_fonts", Some(f), regular).map(|face| face.path))
            .collect();
        let symbol_font =
            resolve("symbol_font", cfg.symbol_font.as_deref(), regular).map(|face| face.path);
        let emoji_font =
            resolve("emoji_font", cfg.emoji_font.as_deref(), regular).map(|face| face.path);
        (
            Self {
                styled_paths,
                synthetic_style: cfg.font_synthetic_style.unwrap_or(true),
                fallback_fonts,
                symbol_font,
                emoji_font,
                display_faces: crate::effective_display_faces(cfg),
            },
            warns,
        )
    }

    #[cfg(test)]
    pub(crate) fn preserve_rejected_from(&mut self, cfg: &Config, previous: &Self) {
        fn preserve<T: Clone>(authored: Option<&str>, next: &mut Option<T>, old: &Option<T>) {
            if authored.is_some_and(|value| !value.trim().is_empty()) && next.is_none() {
                next.clone_from(old);
            }
        }
        preserve(
            cfg.font_family_bold.as_deref(),
            &mut self.styled_paths[0],
            &previous.styled_paths[0],
        );
        preserve(
            cfg.font_family_italic.as_deref(),
            &mut self.styled_paths[1],
            &previous.styled_paths[1],
        );
        preserve(
            cfg.font_family_bold_italic.as_deref(),
            &mut self.styled_paths[2],
            &previous.styled_paths[2],
        );
        if cfg
            .fallback_fonts
            .as_ref()
            .is_some_and(|fonts| !fonts.0.is_empty())
            && self.fallback_fonts.is_empty()
        {
            self.fallback_fonts.clone_from(&previous.fallback_fonts);
        }
        if cfg
            .symbol_font
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && self.symbol_font.is_none()
        {
            self.symbol_font.clone_from(&previous.symbol_font);
        }
        if cfg
            .emoji_font
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && self.emoji_font.is_none()
        {
            self.emoji_font.clone_from(&previous.emoji_font);
        }
    }
}

const TITLEBAR_BAND_SANITY_CAP_PTS: f64 = 120.0;

fn accept_titlebar_band_pts(
    measured_pts: f64,
    fullscreen: bool,
    decorated: bool,
    prev_pts: f64,
) -> f64 {
    if fullscreen {
        return 0.0;
    }
    if !decorated {
        return measured_pts.max(0.0);
    }
    if !titlebar_band_sane(measured_pts) {
        return prev_pts;
    }
    measured_pts
}

fn titlebar_band_sane(pts: f64) -> bool {
    pts > 0.0 && pts <= TITLEBAR_BAND_SANITY_CAP_PTS
}

/// Titlebar head decision for one sample.
pub(crate) fn titlebar_band_decision(
    measured_pts: f64,
    fullscreen: bool,
    decorated: bool,
    memory_pts: f64,
) -> (f64, f64) {
    let applied = accept_titlebar_band_pts(measured_pts, fullscreen, decorated, memory_pts);
    let memory = if !fullscreen && decorated && titlebar_band_sane(measured_pts) {
        measured_pts
    } else {
        memory_pts
    };
    (applied, memory)
}

#[cfg(test)]
mod titlebar_band_acceptance_tests {
    use super::{TITLEBAR_BAND_SANITY_CAP_PTS, accept_titlebar_band_pts, titlebar_band_decision};

    #[test]
    fn a_titlebar_band_sample_is_accepted_only_when_windowed_decorated_and_sane() {
        let inflated = TITLEBAR_BAND_SANITY_CAP_PTS + 1.0;
        for (label, sample, fullscreen, decorated, prev, expected) in [
            ("fullscreen 38", 38.0, true, true, 55.0, 0.0),
            ("fullscreen 0", 0.0, true, true, 55.0, 0.0),
            ("fullscreen 900", 900.0, true, true, 55.0, 0.0),
            ("windowed 0", 0.0, false, true, 55.0, 55.0),
            ("windowed -3", -3.0, false, true, 55.0, 55.0),
            ("windowed past the cap", inflated, false, true, 55.0, 55.0),
            ("windowed 700", 700.0, false, true, 55.0, 55.0),
            ("windowed 38", 38.0, false, true, 55.0, 38.0),
            ("windowed 62, no prev", 62.0, false, true, 0.0, 62.0),
            ("undecorated 0", 0.0, false, false, 55.0, 0.0),
            ("undecorated -1", -1.0, false, false, 55.0, 0.0),
        ] {
            assert_eq!(
                accept_titlebar_band_pts(sample, fullscreen, decorated, prev),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn band_memory_survives_a_fullscreen_stay_and_restores_on_exit_artifact() {
        let (applied, memory) = titlebar_band_decision(55.0, false, true, 0.0);
        assert_eq!((applied, memory), (55.0, 55.0));

        let (applied, memory) = titlebar_band_decision(700.0, true, true, memory);
        assert_eq!((applied, memory), (0.0, 55.0));

        let (applied, memory) = titlebar_band_decision(0.0, true, true, memory);
        assert_eq!((applied, memory), (0.0, 55.0));

        let (applied, memory) = titlebar_band_decision(0.0, false, true, memory);
        assert_eq!((applied, memory), (55.0, 55.0));

        assert_eq!(
            titlebar_band_decision(0.0, false, false, 55.0),
            (0.0, 55.0),
            "undecorated applies 0 and keeps the memory slot"
        );
    }
}

fn warn_background_opacity_unimplemented_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::logging::stderr_line!(
            "aterm-gui: background_opacity < 1.0 requests translucent glass, but the \
             CPU renderer has no translucent present path; the window \
             renders solid (use the GPU backend for real vibrancy; the raised contrast \
             floor still applies)"
        );
        crate::message_inbox::queue_message(crate::message_reporters::cpu_renderer_no_effect(
            "background_opacity",
            "it has no translucent present path, so the window stays solid; enable the GPU \
             renderer for real transparency",
        ));
    });
}

fn warn_background_material_unimplemented_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::logging::stderr_line!(
            "aterm-gui: background_material selects a window-level vibrancy blur \
             (NSVisualEffectView), but the CPU renderer cannot composite \
             over it; the setting has no effect on the CPU backend (use the GPU backend)"
        );
        crate::message_inbox::queue_message(crate::message_reporters::cpu_renderer_no_effect(
            "background_material",
            "it cannot composite over a window-level blur; enable the GPU renderer to see \
             the material",
        ));
    });
}

/// Window-chrome appearance, distinct from the terminal color scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum WindowTheme {
    /// Follow the OS light/dark setting, so chrome tracks live appearance switches.
    #[default]
    Auto,
    /// Force light window chrome.
    Light,
    /// Force dark window chrome.
    Dark,
}

impl WindowTheme {
    /// Parse a config `window_theme` value (case-insensitive, trimmed).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// What a right press in the grid does when no app tracks the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightClickGesture {
    /// The conhost / Windows Terminal convention.
    CopyPaste,
    /// Leave the right button to the seam.
    Off,
}

impl RightClickGesture {
    /// Default when the key is absent.
    pub(crate) const PLATFORM_DEFAULT: Self = if cfg!(windows) {
        Self::CopyPaste
    } else {
        Self::Off
    };

    /// Parse a config `right_click` value (case-insensitive, trimmed).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "copy_paste" | "copy-paste" => Some(Self::CopyPaste),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// Which keyboard spellings of "show this tab's context menu" aterm claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) enum TabMenuChord {
    /// Both Windows spellings.
    On,
    /// The Menu key only.
    MenuKey,
    /// Neither; the menu is pointer-only and both keys reach the application.
    Off,
}

#[cfg(any(not(target_os = "macos"), test))]
impl TabMenuChord {
    /// Parse a config `tab_menu_chord` value (case-insensitive, trimmed).
    #[cfg(any(not(target_os = "macos"), test))]
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "on" | "both" => Some(Self::On),
            "menu_key" | "menu-key" | "menu" => Some(Self::MenuKey),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// Whether this policy claims the dedicated Menu / Application key.
    #[cfg(any(not(target_os = "macos"), test))]
    pub(crate) fn claims_menu_key(self) -> bool {
        matches!(self, Self::On | Self::MenuKey)
    }

    /// Whether this policy claims Shift+F10.
    #[cfg(any(not(target_os = "macos"), test))]
    pub(crate) fn claims_shift_f10(self) -> bool {
        matches!(self, Self::On)
    }
}

/// In-grid tab band height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabBandHeight {
    /// No synthetic head: the band is the top pad plus the strip's cell rows (23 px at Windows defaults, 96 dpi: one 9x23 cell row).
    Compact,
    /// Size the whole band to [`TAB_BAND_STANDARD_LOGICAL_PX`] logical px (a real WinUI tab) by reserving the difference as a synthetic chrome head.
    Standard,
}

impl TabBandHeight {
    /// Default when the key is absent.
    pub(crate) const PLATFORM_DEFAULT: Self = if cfg!(any(windows, target_os = "linux")) {
        Self::Standard
    } else {
        Self::Compact
    };

    /// Parse a config `tab_band_height` value (case-insensitive, trimmed).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "standard" => Some(Self::Standard),
            _ => None,
        }
    }

    /// Target band height in logical px (1/96 in on Windows).
    pub(crate) const fn target_logical_px(self) -> f32 {
        match self {
            Self::Compact => 0.0,
            Self::Standard => TAB_BAND_STANDARD_LOGICAL_PX,
        }
    }
}

/// The `"standard"` band target in logical px.
pub(crate) const TAB_BAND_STANDARD_LOGICAL_PX: f32 = if cfg!(target_os = "linux") {
    36.0
} else {
    32.0
};

const SYNTHETIC_BAND_HEAD_CELL_CAP: usize = 4;

/// Synthetic chrome-head band in device px for `strip_rows` rows of `cell_h` px under `pad_top_px`, when the whole band should measure `target_logical` logical px at `scale`.
pub(crate) fn synthetic_band_head_px(
    target_logical: f32,
    pad_top_px: usize,
    strip_rows: u16,
    cell_h: usize,
    scale: f64,
) -> usize {
    let target_is_positive = matches!(
        target_logical.partial_cmp(&0.0),
        Some(std::cmp::Ordering::Greater)
    );
    if strip_rows == 0 || !target_is_positive {
        return 0;
    }
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let target_px = crate::logical_to_device_px(target_logical, scale);
    let strip_px = (strip_rows as usize).saturating_mul(cell_h);
    let head = target_px
        .saturating_sub(pad_top_px)
        .saturating_sub(strip_px);
    head.min(cell_h.saturating_mul(SYNTHETIC_BAND_HEAD_CELL_CAP))
}

/// Colour-space tag for the window's CAMetalLayer, resolved via [`Config::window_colorspace_or_default`] and applied at surface attach and on reload through [`crate::platform::AppRt::window_set_surface_colorspace`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum WindowColorspace {
    /// Tag the layer sRGB (the default).
    #[default]
    Srgb,
    /// Tag the layer Display-P3.
    DisplayP3,
}

impl WindowColorspace {
    /// Parse a config `window_colorspace` value (case-insensitive, trimmed).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "srgb" => Some(Self::Srgb),
            "display-p3" | "displayp3" | "p3" => Some(Self::DisplayP3),
            _ => None,
        }
    }
}

/// Upper clamp on `window_padding` (logical px).
pub(crate) const MAX_WINDOW_PADDING_PX: f32 = 64.0;

/// Backdrop material behind a translucent window (macOS `NSVisualEffectView`; Windows maps it to a dwm backdrop), resolved via [`Config::background_material_or_default`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum BackgroundMaterial {
    /// No vibrancy view.
    #[default]
    None,
    /// `.hud` — the heads-up-display material (darkest, most saturated blur).
    Hud,
    /// `.sidebar` — the source-list material (a lighter, source-list blur).
    Sidebar,
    /// `.underWindowBackground`.
    UnderWindow,
}

impl BackgroundMaterial {
    /// Parse a config `background_material` value (case-insensitive, trimmed).
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "" => Some(Self::None),
            "hud" => Some(Self::Hud),
            "sidebar" => Some(Self::Sidebar),
            "under-window" | "underwindow" | "under_window" => Some(Self::UnderWindow),
            _ => None,
        }
    }
}

/// Content fingerprints of the active files the config references by path.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct PathFeedFps {
    pub(crate) deco: u64,
}

fn deco_feed_changed(old: &Config, new: &Config, fresh_deco_fp: u64, applied_deco_fp: u64) -> bool {
    old.sparkle_words != new.sparkle_words
        || old.theme != new.theme
        || fresh_deco_fp != applied_deco_fp
}

/// Expand a leading `~` / `~/` / `$HOME` to the user's home directory.
pub(crate) fn sparkle_expand_tilde(path: &str) -> std::path::PathBuf {
    if path == "~"
        && let Some(h) = std::env::var_os("HOME")
    {
        return std::path::PathBuf::from(h);
    } else if let Some(rest) = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("$HOME/"))
        && let Some(h) = std::env::var_os("HOME")
    {
        return std::path::Path::new(&h).join(rest);
    }
    std::path::PathBuf::from(path)
}

fn bounded_asset_text(value: &str, max_bytes: usize) -> std::sync::Arc<str> {
    if value.len() <= max_bytes {
        return std::sync::Arc::from(value);
    }
    let suffix = '…';
    let mut end = max_bytes.saturating_sub(suffix.len_utf8()).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    std::sync::Arc::from(format!("{}{}", &value[..end], suffix))
}

fn invalid_kitty_sprite(source_id: &str, reason: impl AsRef<str>) -> KittySpriteAsset {
    KittySpriteAsset::Invalid {
        source_id: bounded_asset_text(source_id, MAX_KITTY_SOURCE_ID_BYTES),
        bounded_reason: bounded_asset_text(reason.as_ref(), MAX_KITTY_REASON_BYTES),
    }
}

fn read_bounded_kitty_png(path: &std::path::Path) -> Result<Vec<u8>, String> {
    aterm_effects::file_feed::read_bounded_regular_file(path, MAX_KITTY_SPRITE_FILE_BYTES)
        .map_err(|error| format!("unreadable ({error})"))
}

const fn stable_asset_fingerprint(tag: u8, source: &[u8], w: u16, h: u16, rgba: &[u8]) -> u64 {
    let mut hash = aterm_hash::Fnv1a64::new();
    hash.write_u8(tag);
    hash.write(&(source.len() as u64).to_le_bytes());
    hash.write(source);
    hash.write(&w.to_le_bytes());
    hash.write(&h.to_le_bytes());
    hash.write(&(rgba.len() as u64).to_le_bytes());
    hash.write(rgba);
    hash.finish() | 1
}

fn resolve_kitty_sprite_asset(raw: Option<&str>) -> KittySpriteAsset {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return KittySpriteAsset::BuiltIn;
    };
    if raw.len() > MAX_KITTY_SOURCE_ID_BYTES {
        return invalid_kitty_sprite(raw, "configured sprite source is too long");
    }
    let source_id = std::sync::Arc::<str>::from(raw);
    let path = sparkle_expand_tilde(raw);
    let bytes = match read_bounded_kitty_png(&path) {
        Ok(bytes) => bytes,
        Err(reason) => return invalid_kitty_sprite(&source_id, reason),
    };
    let Some((rgba, w, h)) = aterm_render::decode_png_rgba8(&bytes) else {
        return invalid_kitty_sprite(&source_id, "PNG decode failed");
    };
    if w == 0
        || h == 0
        || w > MAX_KITTY_SPRITE_DIMENSION
        || h > MAX_KITTY_SPRITE_DIMENSION
        || rgba.len() != w.saturating_mul(h).saturating_mul(4)
    {
        return invalid_kitty_sprite(
            &source_id,
            format!("decoded sprite must be 1..={MAX_KITTY_SPRITE_DIMENSION} pixels per side"),
        );
    }
    let Ok(w) = u16::try_from(w) else {
        return invalid_kitty_sprite(&source_id, "decoded width is out of range");
    };
    let Ok(h) = u16::try_from(h) else {
        return invalid_kitty_sprite(&source_id, "decoded height is out of range");
    };
    let fp = stable_asset_fingerprint(0x52, source_id.as_bytes(), w, h, &rgba);
    KittySpriteAsset::Ready {
        source_id,
        w,
        h,
        rgba: std::sync::Arc::from(rgba),
        fp,
    }
}

fn invalid_wallpaper(source_id: &str, reason: impl AsRef<str>) -> WallpaperAsset {
    WallpaperAsset::Invalid {
        source_id: bounded_asset_text(source_id, MAX_KITTY_SOURCE_ID_BYTES),
        bounded_reason: bounded_asset_text(reason.as_ref(), MAX_KITTY_REASON_BYTES),
    }
}

/// The file name extensions of the images the wallpaper decoder reads on every platform ([`decode_wallpaper_portable`]).
#[cfg(any(windows, test))]
pub(crate) fn wallpaper_portable_extensions() -> Vec<&'static str> {
    std::iter::once("png")
        .chain(aterm_jpeg::EXTENSIONS.iter().copied())
        .collect()
}

/// The wallpaper picker's image row.
#[cfg(any(windows, test))]
pub(crate) fn wallpaper_image_filter(platform: &[String]) -> (String, String) {
    let mut extensions: Vec<String> = wallpaper_portable_extensions()
        .into_iter()
        .map(str::to_string)
        .collect();
    for extension in platform {
        let extension = extension.to_ascii_lowercase();
        if !extension.is_empty() && !extensions.contains(&extension) {
            extensions.push(extension);
        }
    }
    let patterns = extensions
        .iter()
        .map(|extension| format!("*.{extension}"))
        .collect::<Vec<_>>()
        .join(";");
    ("Images".to_string(), patterns)
}

/// Decode wallpaper bytes with the first-party decoders every platform has.
pub(crate) fn decode_wallpaper_portable(bytes: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    let bound = MAX_WALLPAPER_SOURCE_DIMENSION as u32;
    aterm_png::decode_rgba8(bytes, bound).or_else(|| aterm_jpeg::decode_rgba8(bytes, bound))
}

fn decode_wallpaper_bytes(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    if let Some(decoded) = aterm_png::decode_rgba8(bytes, MAX_WALLPAPER_SOURCE_DIMENSION as u32) {
        return Ok(decoded);
    }
    #[cfg(target_os = "macos")]
    {
        decode_wallpaper_appkit(bytes)
            .or_else(|appkit| decode_wallpaper_portable(bytes).ok_or(appkit))
    }
    #[cfg(windows)]
    {
        decode_wallpaper_portable(bytes).map_or_else(
            || crate::image_decode_win::decode_rgba8(bytes, MAX_WALLPAPER_SOURCE_DIMENSION),
            Ok,
        )
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        decode_wallpaper_portable(bytes).ok_or_else(|| {
            format!(
                "not a decodable PNG or JPEG (up to {MAX_WALLPAPER_SOURCE_DIMENSION} px per side)"
            )
        })
    }
}

#[cfg(target_os = "macos")]
fn decode_wallpaper_appkit(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    use std::ffi::c_void;

    use aterm_objc::{Id, Sel, autoreleasepool, class, sel};

    use crate::appkit;

    const NS_BITMAP_IMAGE_FILE_TYPE_PNG: usize = 4;
    const NS_COLOR_RENDERING_INTENT_PERCEPTUAL: isize = 3;

    autoreleasepool(|_| {
        // SAFETY: raster work on immutable bytes; no class here is
        // main-thread-bound. `+dataWithBytes:length:` copies,
        // `+imageRepWithData:` returns an autoreleased rep alive for this pool,
        // and `-pixelsWide`/`-pixelsHigh` return `NSInteger`.
        unsafe {
            let data_with: unsafe extern "C-unwind" fn(Id, Sel, *const c_void, usize) -> Id =
                aterm_objc::checked_msg();
            let data = data_with(
                class(c"NSData").as_id(),
                sel!(dataWithBytes:length:),
                bytes.as_ptr().cast(),
                bytes.len(),
            );
            if data.is_null() {
                return Err("not a decodable image".to_string());
            }
            let rep = appkit::send_id_id(
                class(c"NSBitmapImageRep").as_id(),
                sel!(imageRepWithData:),
                data,
            );
            if rep.is_null() {
                return Err("not a decodable image".to_string());
            }
            let pw = appkit::send_isize(rep, sel!(pixelsWide));
            let ph = appkit::send_isize(rep, sel!(pixelsHigh));
            if pw <= 0
                || ph <= 0
                || pw as usize > MAX_WALLPAPER_SOURCE_DIMENSION
                || ph as usize > MAX_WALLPAPER_SOURCE_DIMENSION
            {
                return Err(format!(
                    "decoded image must be 1..={MAX_WALLPAPER_SOURCE_DIMENSION} pixels per side"
                ));
            }
            let srgb = appkit::send_id(class(c"NSColorSpace").as_id(), sel!(sRGBColorSpace));
            if srgb.is_null() {
                return Err("could not convert the image to sRGB".to_string());
            }
            let convert: unsafe extern "C-unwind" fn(Id, Sel, Id, isize) -> Id =
                aterm_objc::checked_msg();
            let rep = convert(
                rep,
                sel!(bitmapImageRepByConvertingToColorSpace:renderingIntent:),
                srgb,
                NS_COLOR_RENDERING_INTENT_PERCEPTUAL,
            );
            if rep.is_null() {
                return Err("could not convert the image to sRGB".to_string());
            }
            let properties = appkit::send_id(class(c"NSDictionary").as_id(), sel!(dictionary));
            let representation: unsafe extern "C-unwind" fn(Id, Sel, usize, Id) -> Id =
                aterm_objc::checked_msg();
            let png = representation(
                rep,
                sel!(representationUsingType:properties:),
                NS_BITMAP_IMAGE_FILE_TYPE_PNG,
                properties,
            );
            if png.is_null() {
                return Err("could not re-encode the decoded image".to_string());
            }
            let png_len = appkit::send_usize(png, sel!(length));
            let mut png_bytes = vec![0_u8; png_len];
            if png_len != 0 {
                let get_bytes: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void, usize) =
                    aterm_objc::checked_msg();
                get_bytes(
                    png,
                    sel!(getBytes:length:),
                    png_bytes.as_mut_ptr().cast(),
                    png_len,
                );
            }
            aterm_png::decode_rgba8(&png_bytes, MAX_WALLPAPER_SOURCE_DIMENSION as u32)
                .ok_or_else(|| "could not read the decoded image pixels".to_string())
        }
    })
}

#[cfg(test)]
mod portable_wallpaper_tests {
    use super::{decode_wallpaper_bytes, decode_wallpaper_portable, wallpaper_image_filter};

    const QUADRANTS: &[u8] = include_bytes!("../../aterm-jpeg/tests/fixtures/b420.jpg");
    const PROGRESSIVE: &[u8] = include_bytes!("../../aterm-jpeg/tests/fixtures/p420.jpg");

    fn dominant(rgba: &[u8], width: usize, x: usize, y: usize) -> char {
        let i = (y * width + x) * 4;
        let (r, g, b) = (rgba[i], rgba[i + 1], rgba[i + 2]);
        if r > g && r > b {
            'r'
        } else if g > r && g > b {
            'g'
        } else {
            'b'
        }
    }

    #[test]
    fn wallpaper_decodes_a_jpeg_on_every_platform() {
        for bytes in [QUADRANTS, PROGRESSIVE] {
            for (path, decoded) in [
                (
                    "portable",
                    decode_wallpaper_portable(bytes).ok_or("refused"),
                ),
                (
                    "platform",
                    decode_wallpaper_bytes(bytes).map_err(|_| "refused"),
                ),
            ] {
                let (rgba, width, height) = decoded.unwrap_or_else(|e| panic!("{path}: {e}"));
                assert_eq!((width, height), (33, 21), "{path}");
                assert_eq!(rgba.len(), 33 * 21 * 4, "{path}");
                assert_eq!(dominant(&rgba, width, 4, 4), 'r', "{path}: top left");
                assert_eq!(dominant(&rgba, width, 28, 4), 'g', "{path}: top right");
                assert_eq!(dominant(&rgba, width, 4, 17), 'b', "{path}: bottom left");
            }
        }
        assert!(decode_wallpaper_portable(b"GIF89a....").is_none());
        assert!(decode_wallpaper_bytes(b"not an image").is_err());
    }

    #[test]
    fn the_image_picker_row_lists_what_the_decoder_reads() {
        let (name, patterns) = wallpaper_image_filter(&[]);
        assert_eq!(name, "Images");
        assert_eq!(patterns, "*.png;*.jpg;*.jpeg;*.jpe;*.jfif");
        let platform = ["bmp", "JPG", "heic", "bmp"].map(str::to_string);
        let (_, patterns) = wallpaper_image_filter(&platform);
        assert_eq!(patterns, "*.png;*.jpg;*.jpeg;*.jpe;*.jfif;*.bmp;*.heic");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod appkit_wallpaper_tests {
    use super::decode_wallpaper_appkit;

    fn two_by_two_bmp() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"BM");
        v.extend_from_slice(&70_u32.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&54_u32.to_le_bytes());
        v.extend_from_slice(&40_u32.to_le_bytes());
        v.extend_from_slice(&2_i32.to_le_bytes());
        v.extend_from_slice(&2_i32.to_le_bytes());
        v.extend_from_slice(&1_u16.to_le_bytes());
        v.extend_from_slice(&24_u16.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&16_u32.to_le_bytes());
        v.extend_from_slice(&2835_i32.to_le_bytes());
        v.extend_from_slice(&2835_i32.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&0_u32.to_le_bytes());
        v.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]);
        v.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        v
    }

    fn dominant(px: &[u8]) -> (usize, u8) {
        let (mut best, mut best_v) = (0, px[0]);
        for (i, &v) in px.iter().take(3).enumerate() {
            if v > best_v {
                best = i;
                best_v = v;
            }
        }
        (best, px[3])
    }

    #[test]
    fn appkit_decodes_a_bmp_with_the_right_geometry_and_channel_order() {
        let (rgba, w, h) =
            decode_wallpaper_appkit(&two_by_two_bmp()).expect("AppKit decoded the BMP");
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgba.len(), 2 * 2 * 4);
        let px = |i: usize| &rgba[i * 4..i * 4 + 4];
        assert_eq!(dominant(px(0)).0, 0, "top-left is red");
        assert_eq!(dominant(px(1)).0, 1, "top-right is green");
        assert_eq!(dominant(px(2)).0, 2, "bottom-left is blue");
        for i in 0..4 {
            assert_eq!(px(i)[3], 255, "pixel {i} lost its alpha");
        }
        let white = px(3);
        assert!(
            white[0] > 200 && white[1] > 200 && white[2] > 200,
            "bottom-right should be near-white, got {white:?}"
        );
    }
}

pub(crate) fn resolve_wallpaper_asset(raw: Option<&str>) -> WallpaperAsset {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return WallpaperAsset::None;
    };
    if raw.len() > MAX_KITTY_SOURCE_ID_BYTES {
        return invalid_wallpaper(raw, "configured wallpaper source is too long");
    }
    let source_id = std::sync::Arc::<str>::from(raw);
    let path = sparkle_expand_tilde(raw);
    let bytes = match aterm_effects::file_feed::read_bounded_regular_file(
        &path,
        MAX_WALLPAPER_FILE_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(error) => return invalid_wallpaper(&source_id, format!("unreadable ({error})")),
    };
    let (mut rgba, mut w, mut h) = match decode_wallpaper_bytes(&bytes) {
        Ok(decoded) => decoded,
        Err(reason) => return invalid_wallpaper(&source_id, reason),
    };
    if w == 0 || h == 0 || rgba.len() != w.saturating_mul(h).saturating_mul(4) {
        return invalid_wallpaper(&source_id, "decoded image has inconsistent dimensions");
    }
    let longest = w.max(h);
    if longest > MAX_WALLPAPER_KEEP_DIMENSION {
        let dw = (w * MAX_WALLPAPER_KEEP_DIMENSION / longest).max(1);
        let dh = (h * MAX_WALLPAPER_KEEP_DIMENSION / longest).max(1);
        rgba = aterm_render::resample_rgba(&rgba, w, h, dw, dh);
        (w, h) = (dw, dh);
    }
    let (Ok(w32), Ok(h32)) = (u32::try_from(w), u32::try_from(h)) else {
        return invalid_wallpaper(&source_id, "decoded dimensions are out of range");
    };
    let mut identity = Vec::with_capacity(source_id.len() + 8);
    identity.extend_from_slice(source_id.as_bytes());
    identity.extend_from_slice(&w32.to_le_bytes());
    identity.extend_from_slice(&h32.to_le_bytes());
    let fp = stable_asset_fingerprint(0x57, &identity, 0, 0, &rgba);
    WallpaperAsset::Ready {
        source_id,
        w: w32,
        h: h32,
        rgba: std::sync::Arc::from(rgba),
        fp,
    }
}

/// Escape `w` as a TOML basic string (with quotes), including control characters.
pub(crate) fn toml_basic_string(w: &str) -> String {
    let mut s = String::with_capacity(w.len() + 2);
    s.push('"');
    for c in w.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\u{0008}' => s.push_str("\\b"),
            '\t' => s.push_str("\\t"),
            '\n' => s.push_str("\\n"),
            '\u{000C}' => s.push_str("\\f"),
            '\r' => s.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{007F}' => {
                s.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

fn append_extra_words_entry(out: &mut String, class: &str, words: Option<&[String]>) {
    let Some(words) = words.filter(|w| !w.is_empty()) else {
        return;
    };
    let forms: Vec<String> = words.iter().map(|w| toml_basic_string(w)).collect();
    out.push_str(&format!(
        "\n[[entry]]\nclass = \"{class}\"\nlang = \"en\"\nmode = \"forms\"\nforms = [{}]\n",
        forms.join(", ")
    ));
}

impl Config {
    /// The engine config's non-colour keys (cursor shape, scrollback, BiDi, width, faint, security opt-ins) over the engine defaults.
    pub(crate) fn engine_behaviour_config(&self) -> aterm_core::config::TerminalConfig {
        let mut tc = aterm_core::config::TerminalConfig::default();
        if let Some(n) = self.scrollback_lines {
            tc.scrollback_limit = (n != 0).then_some(n);
        }
        if self.cursor_style.is_some() || self.cursor_blink.is_some() {
            let blink = self.cursor_blink.unwrap_or(true);
            let cursor_style = self.cursor_style.as_deref().unwrap_or("block").trim();
            tc.cursor_style = if cursor_style.eq_ignore_ascii_case("block") {
                if blink {
                    CursorStyle::BlinkingBlock
                } else {
                    CursorStyle::SteadyBlock
                }
            } else if cursor_style.eq_ignore_ascii_case("bar")
                || cursor_style.eq_ignore_ascii_case("beam")
            {
                if blink {
                    CursorStyle::BlinkingBar
                } else {
                    CursorStyle::SteadyBar
                }
            } else {
                crate::logging::stderr_line!(
                    "aterm-gui: config cursor_style: expected block|bar, got {cursor_style:?}"
                );
                if blink {
                    CursorStyle::BlinkingBlock
                } else {
                    CursorStyle::SteadyBlock
                }
            };
            tc.cursor_blink = blink;
        }
        if let Some(b) = self.bidi.as_deref() {
            use aterm_core::config::BiDiMode;
            match b.trim().to_ascii_lowercase().as_str() {
                "disabled" | "off" => tc.bidi.mode = BiDiMode::Disabled,
                "implicit" | "on" => tc.bidi.mode = BiDiMode::Implicit,
                "explicit" => tc.bidi.mode = BiDiMode::Explicit,
                other => crate::logging::stderr_line!(
                    "aterm-gui: config bidi: expected implicit|disabled|explicit, got {other:?}"
                ),
            }
        }
        if let Some(w) = self.ambiguous_width.as_deref() {
            match w.trim().to_ascii_lowercase().as_str() {
                "narrow" | "single" => tc.ambiguous_width_double = false,
                "wide" | "double" => tc.ambiguous_width_double = true,
                other => crate::logging::stderr_line!(
                    "aterm-gui: config ambiguous_width: expected narrow|wide, got {other:?}"
                ),
            }
        }
        if self.faint_opacity.is_some() {
            tc.faint_opacity = self.faint_opacity_or_default();
        }
        tc.indexed_minimum_contrast = self.indexed_minimum_contrast_or_default();
        if let Some(v) = self.allow_osc52_query {
            tc.allow_osc52_query = v;
        }
        if let Some(v) = self.allow_window_ops {
            tc.allow_window_ops = v;
        }
        if let Some(v) = self.allow_notifications {
            tc.allow_notifications = v;
        }
        tc.allow_palette_reconfigure = self.allow_palette_reconfigure_or_default();
        tc
    }

    #[cfg(test)]
    pub(crate) fn applied_terminal_config(&self) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_for(aterm_types::Appearance::Dark)
    }

    #[cfg(test)]
    pub(crate) fn applied_terminal_config_for(
        &self,
        appearance: aterm_types::Appearance,
    ) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_for_with_assets(appearance, &ThemeCatalog::default())
    }

    #[cfg(test)]
    pub(crate) fn applied_terminal_config_for_with_assets(
        &self,
        appearance: aterm_types::Appearance,
        themes: &ThemeCatalog,
    ) -> aterm_core::config::TerminalConfig {
        self.applied_terminal_config_from(&self.resolved_colors(appearance, themes, None))
    }
}

/// Resolve the config file path without creating anything, via `aterm_types::dirs::aterm_config_path` (`$XDG_CONFIG_HOME`, then `%APPDATA%` on Windows, then `$HOME/.config`), the resolver the CLI's `explain-config` and `doctor` also use.
pub(crate) fn config_path() -> Option<std::path::PathBuf> {
    aterm_types::dirs::aterm_config_path()
}

/// One launch override that outranks aterm.toml, shared by Settings and Manual's host diagnostics so they agree on pinned values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveEnvironmentOverride {
    /// The launch flag that set it (`--font-px`, `--columns`).
    pub(crate) variable: &'static str,
    pub(crate) effective: String,
}

/// Resolve an active launch override for one config key.
pub(crate) fn active_environment_override(key: &str) -> Option<ActiveEnvironmentOverride> {
    let resolved = |variable, effective| ActiveEnvironmentOverride {
        variable,
        effective,
    };
    match key {
        "columns" => explicit_initial_columns()
            .map(|value| resolved("--columns", value.clamp(20, 500).to_string())),
        "lines" => explicit_initial_lines()
            .map(|value| resolved("--lines", value.clamp(5, 300).to_string())),
        "font_px" => launch::flags()
            .font_px
            .map(|value| resolved("--font-px", value.to_string())),
        "font_family" => launch::flags()
            .font_family
            .clone()
            .map(|value| resolved("--font", value)),
        "gpu" => launch::flags().renderer.map(|flag| match flag {
            launch::RendererFlag::Cpu => resolved("--cpu", "CPU".to_string()),
            launch::RendererFlag::Gpu => resolved("--gpu", "GPU".to_string()),
        }),
        "shell" => crate::cli::launch_flags()
            .shell
            .filter(|value| !value.is_empty())
            .map(|value| resolved("--shell", value)),
        _ => None,
    }
}

/// Load the user config.
pub(crate) fn load_config() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    let observation =
        match crate::native_config_service::VersionedConfigService::observe_path(&path, true) {
            Ok(observation) => observation,
            Err(error) => {
                let notice = launch_config_notice(&path, LaunchConfigProblem::Unreadable(&error));
                crate::logging::stderr_line!("aterm-gui: {notice}");
                note_launch_load_failed(notice);
                return Config::default();
            }
        };
    Config::parse(&observation.text).unwrap_or_else(|e| {
        let notice = launch_config_notice(&path, LaunchConfigProblem::Invalid(&e));
        crate::logging::stderr_line!("aterm-gui: {notice}");
        note_launch_load_failed(notice);
        Config::stand_in(&observation.text)
    })
}

/// Why the launch could not take `aterm.toml`.
pub(crate) enum LaunchConfigProblem<'a> {
    Unreadable(&'a dyn std::fmt::Display),
    Invalid(&'a dyn std::fmt::Display),
}

/// Launch-time config notice, shown in the window banner and on the console when `aterm.toml` did not load at start.
pub(crate) fn launch_config_notice(
    path: &std::path::Path,
    problem: LaunchConfigProblem<'_>,
) -> String {
    match problem {
        LaunchConfigProblem::Unreadable(error) => format!(
            "aterm.toml could not be read at launch ({error}) — every setting is running at its \
             default. Fix {} and it loads on the next change.",
            path.display()
        ),
        LaunchConfigProblem::Invalid(error) => format!(
            "aterm.toml is not a valid configuration ({error}) — the window's settings except \
             [harness] are at their defaults. Fix {} and it loads on the next change.",
            path.display()
        ),
    }
}

static LAUNCH_LOAD_FAILURE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn note_launch_load_failed(notice: String) {
    crate::message_inbox::queue_message(crate::message_reporters::launch_load_failure(&notice));
    let _ = LAUNCH_LOAD_FAILURE.set(notice);
}

/// Why this process launched without its `aterm.toml`.
pub(crate) fn launch_load_failure() -> Option<&'static str> {
    LAUNCH_LOAD_FAILURE.get().map(String::as_str)
}

/// The `agents_auto_prime` setting, read without [`load_config`]'s stderr lines or notices (the auto-prime thread must stay quiet).
pub(crate) fn agents_auto_prime_setting() -> bool {
    let Some(path) = config_path() else {
        return true;
    };
    match crate::native_config_service::VersionedConfigService::observe_path(&path, true) {
        Ok(observation) => agents_auto_prime_from_text(&observation.text),
        Err(_) => true,
    }
}

/// Read source and application policy from one persisted config observation.
pub(crate) fn update_check_settings_setting() -> Option<aterm_update::CheckSettings> {
    let config = if let Some(path) = config_path() {
        match crate::native_config_service::VersionedConfigService::observe_path(&path, true) {
            Ok(observation) => return update_check_settings_from_text(&observation.text),
            Err(_) => match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Config::default(),
                _ => return None,
            },
        }
    } else {
        Config::default()
    };
    Some(update_check_settings(&config))
}

fn update_check_settings_from_text(text: &str) -> Option<aterm_update::CheckSettings> {
    aterm_toml::from_str::<Config>(text)
        .ok()
        .map(|config| update_check_settings(&config))
}

pub(crate) fn update_check_settings(config: &Config) -> aterm_update::CheckSettings {
    let update = config.update.as_ref();
    aterm_update::CheckSettings {
        source: aterm_update::Source::resolve(
            update.and_then(|u| u.owner.as_deref()),
            update.and_then(|u| u.repo.as_deref()),
        ),
        auto_apply: update.and_then(|u| u.auto_apply).unwrap_or(true),
    }
}

/// Pure core of [`agents_auto_prime_setting`].
pub(crate) fn agents_auto_prime_from_text(text: &str) -> bool {
    match aterm_toml::from_str::<Config>(text) {
        Ok(config) => config.agents_auto_prime_or_default(),
        Err(_) => true,
    }
}

/// Resolve the font size in px at 1× with the canonical precedence `--font-px > config.font_px > FONT_PX default`.
pub(crate) fn resolve_font_px(config: &Config) -> f32 {
    resolve_font_px_with(launch::flags().font_px, config.font_px)
}

/// [`resolve_font_px`] times the text size scale of `view` (the config with any theme typography filled in).
pub(crate) fn resolve_scaled_font_px(view: &Config) -> f32 {
    let base = resolve_font_px(view);
    if launch::flags()
        .font_px
        .is_some_and(|px| font_px_in_range(&px))
    {
        base
    } else {
        crate::scaled_font_px(base, view.font_size_scale_milli_or_default())
    }
}

/// Banner line for a `font_px` that [`resolve_font_px`] ignores (outside `FONT_PX_MIN..=FONT_PX_MAX` or not finite), or `None` when unset or admissible.
pub(crate) fn font_px_load_notice(config: &Config) -> Option<String> {
    let px = config.font_px.filter(|px| !font_px_in_range(px))?;
    Some(format!(
        "config font_px: {px:?} is not accepted (expected {FONT_PX_MIN}–{FONT_PX_MAX}; the \
         value is ignored)"
    ))
}

/// Banner line for a `font_size_scale` outside 0.8..=1.25 or not finite (clamped, or `1.0` when not finite), `None` when unset or in range.
pub(crate) fn font_size_scale_load_notice(config: &Config) -> Option<String> {
    let scale = config
        .font_size_scale
        .filter(|v| !(v.is_finite() && (0.8..=1.25).contains(v)))?;
    let used = f32::from(config.font_size_scale_milli_or_default()) / 1000.0;
    Some(format!(
        "config font_size_scale: {scale:?} is not accepted (expected 0.8–1.25; using {used})"
    ))
}

/// The size notices every config load restates.
pub(crate) fn font_size_load_notices(config: &Config) -> Vec<String> {
    font_px_load_notice(config)
        .into_iter()
        .chain(font_size_scale_load_notice(config))
        .collect()
}

/// Everything about the configured `theme` the window cannot draw as written.
pub(crate) fn theme_misses(config: &Config, themes: &ThemeCatalog) -> Vec<String> {
    theme_selector_notice(config.theme.as_deref())
        .into_iter()
        .chain(theme_catalog_misses(config, themes))
        .collect()
}

/// Each configured `theme` name the window cannot use against `themes`, and that it draws Default instead.
pub(crate) fn theme_catalog_misses(config: &Config, themes: &ThemeCatalog) -> Vec<String> {
    let mut misses = Vec::new();
    for appearance in [
        aterm_types::Appearance::Dark,
        aterm_types::Appearance::Light,
    ] {
        if let Some(name) = config.resolve_theme_name(appearance)
            && !name.eq_ignore_ascii_case("default")
            && let Err(error) = themes.resolve(&name)
        {
            let miss = format!("theme: {name:?} is unavailable ({error}); Default is used");
            if !misses.contains(&miss) {
                misses.push(miss);
            }
        }
    }
    misses
}

/// [`theme_misses`] for the band's unaccepted-values row, pushed at startup, every reload and every theme-directory change.
pub(crate) fn theme_load_notices(config: &Config, themes: &ThemeCatalog) -> Vec<String> {
    prefixed_notices(theme_misses(config, themes))
}

/// Whether a valid flag/config size names the font size (rather than the built-in default).
pub(crate) fn font_px_is_explicit(config: &Config) -> bool {
    font_px_is_explicit_with(launch::flags().font_px, config.font_px)
}

/// Whether to request the GPU renderer, with the same precedence as `main`'s backend selection.
pub(crate) fn resolve_want_gpu(config: &Config) -> bool {
    let flag = launch::flags().renderer;
    resolve_want_gpu_with(
        flag == Some(launch::RendererFlag::Cpu),
        flag == Some(launch::RendererFlag::Gpu),
        config.gpu,
    )
}

/// Effective shell for each new session.
pub(crate) fn resolve_shell_override(config: &Config) -> Option<String> {
    resolve_shell_override_with(crate::cli::launch_flags().shell, config.shell.as_deref())
}

fn resolve_shell_override_with(flag: Option<String>, configured: Option<&str>) -> Option<String> {
    flag.filter(|value| !value.is_empty())
        .or_else(|| configured.map(str::to_string))
}

/// Pure precedence core for [`resolve_want_gpu`], with the two flags and the config value passed in.
pub(crate) const fn resolve_want_gpu_with(
    force_cpu: bool,
    force_gpu: bool,
    config_gpu: Option<bool>,
) -> bool {
    !force_cpu
        && match (force_gpu, config_gpu) {
            (true, _) => true,
            (false, Some(explicit)) => explicit,
            (false, None) => true,
        }
}

/// Notices for changed restart-only keys, which config reload does not hot-apply.
pub(crate) fn restart_notices(old: &Config, new: &Config) -> Vec<String> {
    let mut out = Vec::new();
    if old.columns != new.columns || old.lines != new.lines {
        out.push(
            "columns/lines applies on next launch (resize the window to change size now)"
                .to_string(),
        );
    }
    if old.gpu != new.gpu {
        out.push(
            "gpu applies on next launch (the renderer backend is chosen at startup)".to_string(),
        );
    }
    out
}

/// The band's view of [`crate::native_config_language::key_warnings`], added to `warns` in two families, each sentence prefixed `config …`.
pub(crate) fn collect_key_notices(
    warns: &mut crate::message_reporters::ConfigWarnings,
    text: &str,
) {
    use crate::message_reporters::ConfigFamily;
    let keys = crate::native_config_language::key_warnings(text);
    warns.extend(ConfigFamily::IgnoredKeys, prefixed_notices(keys.ignored));
    warns.extend(ConfigFamily::RetiredKeys, prefixed_notices(keys.retired));
}

/// The supervisor's `[harness]` notices ([`Config::harness_notices`]) added to `warns` as `ConfigFamily::UnacceptedValues`.
pub(crate) fn collect_harness_notices(
    warns: &mut crate::message_reporters::ConfigWarnings,
    config: &Config,
) {
    use crate::message_reporters::ConfigFamily;
    warns.extend(ConfigFamily::UnacceptedValues, config.harness_notices());
}

/// The `ConfigFamily::UnacceptedValues` family, assembled one way at launch and at every reload.
pub(crate) fn collect_unaccepted_value_notices(
    warns: &mut crate::message_reporters::ConfigWarnings,
    config: &Config,
    themes: &ThemeCatalog,
    text: &str,
) {
    use crate::message_reporters::ConfigFamily;
    collect_harness_notices(warns, config);
    warns.extend_shown_in_settings(
        ConfigFamily::UnacceptedValues,
        font_size_load_notices(config),
    );
    warns.extend_shown_in_settings(
        ConfigFamily::UnacceptedValues,
        theme_load_notices(config, themes),
    );
    let unaccepted = unaccepted_value_notices(text, &warns.told());
    warns.extend(ConfigFamily::UnacceptedValues, unaccepted);
}

/// What a reload saw happen to each config problem it may find fixed, as the words that problem's ✓ record takes, by `config.` key ([`crate::message_reporters::config_fix_words`]).
pub(crate) fn config_fix_observations(
    old: &Config,
    old_assets: &ConfigAssetCatalog,
    new: &Config,
    new_assets: &ConfigAssetCatalog,
) -> std::collections::BTreeMap<String, String> {
    use crate::message_reporters::{ConfigFamily, ConfigFix, KEY_FONT_FAMILY, config_fix_words};
    let ended = |was_refused: bool, refused: bool, source: Option<&str>| {
        (was_refused && !refused).then_some(if source.is_none() {
            ConfigFix::Removed
        } else {
            ConfigFix::Applied
        })
    };
    let mut out = std::collections::BTreeMap::new();
    let images: Vec<(&str, ConfigFix)> = [
        (
            "Wallpaper",
            ended(
                old_assets.wallpaper.diagnostic().is_some(),
                new_assets.wallpaper.diagnostic().is_some(),
                new_assets.wallpaper.source_id(),
            ),
        ),
        (
            "Cursor sprite",
            ended(
                old_assets.kitty_sprite.diagnostic().is_some(),
                new_assets.kitty_sprite.diagnostic().is_some(),
                new_assets.kitty_sprite.source_id(),
            ),
        ),
    ]
    .into_iter()
    .filter_map(|(subject, how)| how.map(|how| (subject, how)))
    .collect();
    if let Some(words) = config_fix_words(ConfigFamily::Assets, &images) {
        out.insert(ConfigFamily::Assets.key().to_string(), words);
    }
    let requests = |c: &Config| {
        [
            c.font_family.clone(),
            c.font_family_bold.clone(),
            c.font_family_italic.clone(),
            c.font_family_bold_italic.clone(),
            c.fallback_fonts.as_ref().map(|list| list.0.join(", ")),
        ]
    };
    let (before, after) = (requests(old), requests(new));
    let fonts_removed = before
        .iter()
        .zip(&after)
        .any(|(b, a)| b.is_some() && a.is_none())
        && before
            .iter()
            .zip(&after)
            .all(|(b, a)| a.is_none() || a == b);
    let how = |removed: bool| {
        if removed {
            ConfigFix::Removed
        } else {
            ConfigFix::Applied
        }
    };
    if let Some(words) = config_fix_words(ConfigFamily::Fonts, &[("Font", how(fonts_removed))]) {
        out.insert(ConfigFamily::Fonts.key().to_string(), words);
    }
    let family_removed = old.font_family.is_some() && new.font_family.is_none();
    if let Some(words) = config_fix_words(ConfigFamily::Fonts, &[("Font", how(family_removed))]) {
        out.insert(KEY_FONT_FAMILY.to_string(), words);
    }
    let trail_removed = old.cursor_trail_style.is_some() && new.cursor_trail_style.is_none();
    if let Some(words) = config_fix_words(
        ConfigFamily::CursorTrail,
        &[("Cursor trail", how(trail_removed))],
    ) {
        out.insert(ConfigFamily::CursorTrail.key().to_string(), words);
    }
    out
}

#[cfg(test)]
pub(crate) fn ignored_key_notices(text: &str) -> Vec<String> {
    prefixed_notices(crate::native_config_language::key_warnings(text).ignored)
}

/// The band's view of [`crate::native_config_language::unaccepted_value_warnings`].
pub(crate) fn unaccepted_value_notices(text: &str, already_told: &[String]) -> Vec<String> {
    prefixed_notices(crate::native_config_language::unaccepted_value_warnings(
        text,
        already_told,
    ))
}

fn prefixed_notices(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| format!("config {line}"))
        .collect()
}

/// Fresh-launch terminal width after the documented flag > config > default precedence and the same safety clamp used by window construction.
pub(crate) fn resolve_initial_columns(config: &Config) -> u16 {
    explicit_initial_columns()
        .or(config.columns)
        .unwrap_or(80)
        .clamp(20, 500)
}

/// Fresh-launch terminal height.
pub(crate) fn resolve_initial_lines(config: &Config) -> u16 {
    explicit_initial_lines()
        .or(config.lines)
        .unwrap_or(24)
        .clamp(5, 300)
}

/// Explicit launch grid overrides only (`--columns`/`--lines`), without the config fallback.
pub(crate) fn explicit_initial_columns() -> Option<u16> {
    crate::cli::launch_flags().columns
}

/// Row twin of [`explicit_initial_columns`].
pub(crate) fn explicit_initial_lines() -> Option<u16> {
    crate::cli::launch_flags().lines
}

/// Explicit render scale from `--scale` (finite, positive).
pub(crate) fn resolve_force_scale() -> Option<f64> {
    launch::flags().scale
}

/// Pure precedence core for [`resolve_font_px`], with the `--font-px` flag value and the config value passed in explicitly so it is deterministically unit-testable.
pub(crate) fn resolve_font_px_with(flag: Option<f32>, config: Option<f32>) -> f32 {
    admitted_font_px(flag, config).unwrap_or(FONT_PX)
}

fn font_px_in_range(value: &f32) -> bool {
    value.is_finite() && *value >= FONT_PX_MIN && *value <= FONT_PX_MAX
}

fn admitted_font_px(flag: Option<f32>, config: Option<f32>) -> Option<f32> {
    flag.filter(font_px_in_range)
        .or(config.filter(font_px_in_range))
}

/// Named-size counterpart of [`resolve_font_px_with`], on the same admission helper so an invalid value never counts as named.
pub(crate) fn font_px_is_explicit_with(flag: Option<f32>, config: Option<f32>) -> bool {
    admitted_font_px(flag, config).is_some()
}

/// A font size or device px as the window carry writes it.
#[cfg(unix)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "admitted to FONT_PX_MIN..=FONT_PX_MAX first, so the product is a positive \
              integer of at most 200 000"
)]
pub(crate) fn font_px_milli(px: f32) -> Option<u32> {
    font_px_in_range(&px).then(|| (px * 1000.0).round() as u32)
}

/// The launch font of a process that adopted a seamless update's sessions, before its first window is sized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LaunchFont {
    pub(crate) size: f32,
    pub(crate) reset: f32,
    pub(crate) from_device: bool,
}

impl LaunchFont {
    /// The device px a backend is built at before any window exists.
    pub(crate) fn build_px(&self, headless_scale: Option<f64>) -> f32 {
        match headless_scale {
            Some(scale) if !self.from_device => crate::font_px_for_scale(self.size, scale),
            _ => self.size,
        }
    }
}

/// A carried font zoom, in thousandths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CarriedZoom {
    pub(crate) size: Option<(u32, u32)>,
    pub(crate) device: Option<(u32, u32)>,
}

impl CarriedZoom {
    /// The zoom an authenticated window carry holds.
    pub(crate) fn of_carry(carry: &crate::session_store::WindowCarry) -> Self {
        Self {
            size: carry.font_size_milli.zip(carry.font_reset_size_milli),
            device: carry.font_px_milli.zip(carry.font_reset_px_milli),
        }
    }
}

/// The carried zoom, or the config's font.
pub(crate) fn successor_font_px(
    resolved: f32,
    carry: Option<&crate::session_store::WindowCarry>,
) -> LaunchFont {
    launch_font_for_zoom(
        resolved,
        carry.map(CarriedZoom::of_carry).unwrap_or_default(),
    )
}

/// [`successor_font_px`] over the zoom alone, for the warm prologue that runs before the dial (`WarmOrder::BeforeDial`) using the launch hint (`handoff_warm_hint`).
#[allow(
    clippy::cast_precision_loss,
    reason = "a carried size is a u32 of thousandths; any value past f32's exact range is far \
              outside the admitted bounds and is refused by them"
)]
pub(crate) fn launch_font_for_zoom(resolved: f32, zoom: CarriedZoom) -> LaunchFont {
    let admitted = |(px, reset): (u32, u32)| {
        let (px, reset) = (px as f32 / 1000.0, reset as f32 / 1000.0);
        (font_px_in_range(&px) && font_px_in_range(&reset)).then_some((px, reset))
    };
    if let Some((size, reset)) = zoom.size.and_then(admitted) {
        LaunchFont {
            size,
            reset,
            from_device: false,
        }
    } else if let Some((size, reset)) = zoom.device.and_then(admitted) {
        LaunchFont {
            size,
            reset,
            from_device: true,
        }
    } else {
        LaunchFont {
            size: resolved,
            reset: resolved,
            from_device: false,
        }
    }
}

/// Resolve one raw style.
pub(crate) fn resolve_trail_style(raw: &str) -> ResolvedTrailStyle {
    let raw = raw.trim();
    let Some(canonical) = crate::prefs::cursor_trail_style_canonical(raw) else {
        return ResolvedTrailStyle {
            canonical: Some(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
            style: Some(aterm_effects::cursor_glow::GlowStyle::parse(
                crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
            )),
            unknown: true,
        };
    };
    if canonical == "off" {
        return ResolvedTrailStyle::off();
    }
    ResolvedTrailStyle {
        canonical: Some(canonical),
        style: Some(aterm_effects::cursor_glow::GlowStyle::parse(canonical)),
        unknown: false,
    }
}

/// Resolve every string-keyed trail presentation fork in one pass, per config generation.
pub(crate) fn resolve_trail_presentation(raw: &str) -> ResolvedTrailPresentation {
    resolve_trail_presentation_from_style(raw, resolve_trail_style(raw))
}

/// Presentation half for preview surfaces that already hold a synthetic resolution.
pub(crate) fn resolve_trail_presentation_from_style(
    raw: &str,
    style: ResolvedTrailStyle,
) -> ResolvedTrailPresentation {
    use aterm_effects::cursor_glow::{GlowStyle, style_has_beam_of};

    let token = style.canonical.unwrap_or(raw.trim());
    let glow_style = style.style.unwrap_or(GlowStyle::Lumen);
    let pet_species = (style.style == Some(GlowStyle::RainbowKitty)
        && GlowStyle::style_names_any_pet(token))
    .then(|| {
        if GlowStyle::style_names_dog_pet(token) {
            aterm_effects::kitty_pet::PetSpecies::Dog
        } else {
            aterm_effects::kitty_pet::PetSpecies::Cat
        }
    });
    ResolvedTrailPresentation {
        style,
        beam: style_has_beam_of(glow_style, token),
        ribbon_tall: !GlowStyle::style_names_underline_ribbon(token),
        ribbon_flat: GlowStyle::style_names_flat_ribbon(token),
        classic_mono: style.style == Some(GlowStyle::Classic)
            && GlowStyle::style_names_classic_mono(raw.trim()),
        pet_species,
        comet: style.style == Some(GlowStyle::Comet),
    }
}

#[cfg(test)]
pub(crate) fn effective_trail_style_token(raw: &str) -> &str {
    let raw = raw.trim();
    crate::prefs::cursor_trail_style_canonical(raw)
        .unwrap_or(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
}

/// Cold-path construction of the engine's `GlowConfig` through [`aterm_effects::cursor_fx::resolve_glow_config`] (shared with the web pipeline).
pub(crate) fn resolve_cursor_glow(
    inputs: CursorGlowInputs,
    presentation: ResolvedTrailPresentation,
    theme_cursor: u32,
    dark_theme: bool,
    theme_fg: u32,
    theme_bg: u32,
    head_dx: f32,
) -> aterm_effects::cursor_glow::GlowConfig {
    aterm_effects::cursor_fx::resolve_glow_config(
        aterm_effects::cursor_fx::GlowKnobs {
            enabled: inputs.enabled,
            color: inputs.color,
            accent: inputs.accent,
            duration_ms: inputs.duration_ms,
            length: inputs.length,
            intensity: inputs.intensity,
            radius: inputs.radius,
            ring: inputs.ring,
        },
        aterm_effects::cursor_fx::GlowSpelling {
            style: presentation.style.style,
            beam: presentation.beam,
            ribbon_tall: presentation.ribbon_tall,
            ribbon_flat: presentation.ribbon_flat,
            classic_mono: presentation.classic_mono,
        },
        theme_cursor,
        dark_theme,
        theme_fg,
        theme_bg,
        head_dx,
    )
}

fn sparkle_logged_warnings(
    conflicts: &[String],
    cjk_single_char: bool,
) -> impl Iterator<Item = &String> {
    conflicts
        .iter()
        .filter(move |w| aterm_effects::pipeline::lexicon_warning_applies(w, cjk_single_char))
}

struct ReloadRenderSnapshot {
    config: Config,
    default_font_size: f32,
    font_size: f32,
    font_px: f32,
    theme: Theme,
    font_family: Option<String>,
    text_shaping: aterm_render::TextShapingConfig,
    text_blending: aterm_render::TextBlending,
    font_thicken: bool,
    stem_gamma: f32,
    font_hinting: String,
    font_subpixel: String,
    font_variations: Vec<(u32, f32)>,
    font_weight_dark_nudge: f32,
    render_knobs: RenderKnobs,
    font_config: FontConfig,
    typography: crate::resolved_typography::ResolvedTypography,
    font_size_scale_milli: u16,
    window_metrics: Vec<(WindowId, crate::MetricsView)>,
}

impl ReloadRenderSnapshot {
    fn capture(app: &App) -> Self {
        Self {
            config: app.config.clone(),
            default_font_size: app.default_font_size,
            font_size: app.font_size,
            font_px: app.font_px,
            theme: app.theme,
            font_family: app.font_family.clone(),
            text_shaping: app.text_shaping.clone(),
            text_blending: app.text_blending,
            font_thicken: app.font_thicken,
            stem_gamma: app.stem_gamma,
            font_hinting: app.font_hinting.clone(),
            font_subpixel: app.font_subpixel.clone(),
            font_variations: app.font_variations.clone(),
            font_weight_dark_nudge: app.font_weight_dark_nudge,
            render_knobs: app.render_knobs,
            font_config: app.font_config.clone(),
            typography: app.typography.clone(),
            font_size_scale_milli: app.font_size_scale_milli,
            window_metrics: app
                .windows
                .iter()
                .map(|(wid, ws)| (*wid, ws.metrics))
                .collect(),
        }
    }

    fn rollback(self, app: &mut App) {
        restore_render_config_fields(&mut app.config, &self.config);
        app.default_font_size = self.default_font_size;
        app.font_size = self.font_size;
        app.font_px = self.font_px;
        app.theme = self.theme;
        app.font_family = self.font_family;
        app.text_shaping = self.text_shaping;
        app.text_blending = self.text_blending;
        app.font_thicken = self.font_thicken;
        app.stem_gamma = self.stem_gamma;
        app.font_hinting = self.font_hinting;
        app.font_subpixel = self.font_subpixel;
        app.font_variations = self.font_variations;
        app.font_weight_dark_nudge = self.font_weight_dark_nudge;
        app.render_knobs = self.render_knobs;
        app.font_config = self.font_config;
        app.typography = self.typography;
        app.font_size_scale_milli = self.font_size_scale_milli;
        for (wid, metrics) in self.window_metrics {
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.metrics = metrics;
            }
        }

        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::Rollback);
        app.sparkle_dirty = true;
        app.rain_dirty = true;
    }
}

fn restore_render_config_fields(target: &mut Config, previous: &Config) {
    macro_rules! restore {
        ($($field:ident),+ $(,)?) => {
            $(target.$field.clone_from(&previous.$field);)+
        };
    }
    restore!(
        font_px,
        foreground,
        background,
        cursor_color,
        selection_color,
        palette,
        palette_overrides,
        theme,
        font_family,
        font_family_bold,
        font_family_italic,
        font_family_bold_italic,
        font_synthetic_style,
        fallback_fonts,
        symbol_font,
        emoji_font,
        ligatures,
        merged_ligatures,
        font_features,
        text_blending,
        font_thicken,
        font_variation,
        font_weight,
        font_weight_dark_nudge,
        stem_gamma,
        font_hinting,
        font_subpixel,
        line_height,
        font_size_scale,
        adjust_baseline,
        adjust_underline_position,
        adjust_underline_thickness,
        underline_skip_descenders,
        minimum_contrast,
        selection_foreground,
        selection_inactive,
        cursor_break_ligatures,
        background_opacity,
        background_material,
        window_padding,
        window_padding_top,
    );
}

impl App {
    /// Activate [`App::sparkle`] from the prepared generation under the Serious Mode policy.
    pub(crate) fn recompute_sparkle(&mut self) {
        self.sparkle_dirty = false;
        self.sparkle = if !self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::WordDecorations)
        {
            None
        } else {
            self.prepared_sparkle.resolved.clone()
        };
    }

    /// Resolve the sparkle runtime if still dirty.
    pub(crate) fn ensure_sparkle(&mut self) {
        if self.sparkle_dirty {
            self.recompute_sparkle();
        }
    }

    /// Whether the Kitty Log recorder is on (`[sparkle_words.feline] log`, default true, config-file only).
    pub(crate) fn kitty_log_enabled(&self) -> bool {
        self.config
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.feline.as_ref())
            .and_then(|f| f.log)
            .unwrap_or(true)
    }

    /// Curse-bonk host gate (`[sparkle_words.profanity] bonk`, default on).
    pub(crate) fn curse_bonk_enabled(&self) -> bool {
        self.config
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.profanity.as_ref())
            .and_then(|p| p.bonk)
            .unwrap_or(true)
    }

    /// Detonation-edge opt-in (`bonk_detonation`, default off).
    pub(crate) fn curse_bonk_detonation_enabled(&self) -> bool {
        self.config
            .sparkle_words
            .as_ref()
            .and_then(|sw| sw.profanity.as_ref())
            .and_then(|p| p.bonk_detonation)
            .unwrap_or(false)
    }

    /// Rebuild [`App::rain`] parameters from config and live theme when `rain_dirty` is set, never per frame.
    pub(crate) fn recompute_matrix_rain(&mut self) {
        self.rain_dirty = false;
        let bg = self.theme.bg & 0x00FF_FFFF;
        let fg = self.theme.fg & 0x00FF_FFFF;
        let cfg = self.config.matrix_rain_params(bg, fg, self.grid_accent());
        self.rain = Some(cfg);
        for (wid, ws) in self.windows.iter_mut() {
            if let Some(e) = ws.fx.rain_mut() {
                e.set_config(crate::rain_config_for_window(cfg, *wid));
            }
        }
    }

    /// Effective rain state for session `sid`.
    pub(crate) fn session_rain_enabled(&self, sid: u64) -> bool {
        self.serious_mode_policy()
            .allows(crate::motion::SeriousEffect::MatrixRain)
            && self
                .pool
                .rain_override(sid)
                .unwrap_or_else(|| self.config.matrix_rain_enabled())
    }

    /// Set or clear the rain override for `sid`, mark the resolve stale and wake every window whose front session is `sid`.
    pub(crate) fn set_session_rain_override(&mut self, sid: u64, over: Option<bool>) {
        self.pool.set_rain_override(sid, over);
        self.rain_dirty = true;
        for ws in self.windows.values() {
            if ws.front_terminal().is_some_and(|t| t.session == sid)
                && let Some(w) = ws.os_window.as_ref()
            {
                w.request_redraw();
            }
        }
        self.palette_refresh_live();
    }

    /// Toggle matrix rain for the front session of window `wid` (action, menu, palette, `aterm-ctl rain toggle`).
    pub(crate) fn toggle_matrix_rain(&mut self, wid: crate::WindowId) {
        let Some(sid) = self.front_terminal(wid).map(|t| t.session) else {
            return;
        };
        let effective = self.session_rain_enabled(sid);
        self.set_session_rain_override(sid, Some(!effective));
    }

    /// The `fx` control verb ([`crate::Wake::FxControl`]) on the focused window's front session.
    pub(crate) fn fx_control(&mut self, op: crate::FxCtlOp) -> Result<String, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no focused window".to_string());
        };
        let Some(sid) = self.front_terminal(wid).map(|t| t.session) else {
            return Err("front content is not a terminal (no session to arm)".to_string());
        };
        let glow = self.glow_config();
        let rainbow = matches!(glow.style, crate::cursor_glow::GlowStyle::RainbowKitty);
        let allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorCat);
        let now = std::time::Instant::now();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return Err("no focused window".to_string());
        };
        if let crate::FxCtlOp::Celebrate { sig, bars, on } = op {
            if !allowed {
                return Err("Serious Mode suppresses cursor companions".to_string());
            }
            if !glow.enabled {
                return Err("cursor trail is disabled (nothing to celebrate with)".to_string());
            }
            if !rainbow {
                return Err(
                    "cursor trail is not the rainbow kitty (nothing to celebrate with)".to_string(),
                );
            }
            ws.fx
                .companions_mut()
                .kitty_sing
                .arm_external(now, sid, sig, bars, on)
                .map_err(|refusal| refusal.to_string())?;
        }
        let sing = &ws.fx.companions_mut().kitty_sing;
        let (armed, sig, bars) = sing
            .armed_external()
            .map_or(("none", 0, 0), |arm| (arm.on.as_str(), arm.sig, arm.bars));
        Ok(format!(
            "armed={armed} sig={sig:08x} bars={bars} live={} bar={} drive={:.2} cooldown_ms={} style={}",
            sing.external_live(),
            sing.bar(now).map_or("none".to_string(), |b| b.to_string()),
            sing.drive(now),
            sing.cooldown_remaining(now).as_millis(),
            if rainbow { "rainbow-kitty" } else { "other" },
        ))
    }

    /// The `done` edge of `fx celebrate on=done`.
    pub(crate) fn note_program_done(&mut self, session: u64, now: std::time::Instant) {
        for ws in self.windows.values_mut() {
            if ws
                .fx
                .companions_mut()
                .kitty_sing
                .note_program_done(now, session)
                && let Some(w) = ws.os_window.as_ref()
            {
                w.request_redraw();
            }
        }
    }

    /// The `rain` control verb ([`crate::Wake::RainControl`]) on the focused window's front session.
    pub(crate) fn rain_control(&mut self, op: crate::RainCtlOp) -> Result<String, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no focused window".to_string());
        };
        let Some(sid) = self.front_terminal(wid).map(|t| t.session) else {
            return Err("front content is not a terminal (no session to act on)".to_string());
        };
        match op {
            crate::RainCtlOp::Status => {}
            crate::RainCtlOp::On => self.set_session_rain_override(sid, Some(true)),
            crate::RainCtlOp::Off => self.set_session_rain_override(sid, Some(false)),
            crate::RainCtlOp::Toggle => {
                let effective = self.session_rain_enabled(sid);
                self.set_session_rain_override(sid, Some(!effective));
            }
        }
        let over = match self.pool.rain_override(sid) {
            None => "none",
            Some(true) => "on",
            Some(false) => "off",
        };
        let (engine, active, diag) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.fx.rain())
            .map_or(("none", false, String::new()), |e| {
                ("live", e.is_active(), format!(" {}", e.diag_line()))
            });
        let scope = if self
            .active_tree(wid)
            .is_some_and(|t| t.len() > 1 && !t.is_zoomed())
        {
            "focused-pane"
        } else {
            "window"
        };
        let raw_focused = self.windows.get(&wid).is_some_and(|ws| ws.focused);
        let focused = self.motion_focus(wid, raw_focused);
        let motion = self.motion_policy(focused);
        Ok(format!(
            "config_enabled={} session_override={} effective={} engine={} active={} scope={} \
             focused={} animating={}{}",
            self.config.matrix_rain_enabled(),
            over,
            self.session_rain_enabled(sid),
            engine,
            active,
            scope,
            focused,
            motion.animate(crate::motion::MotionEffect::MatrixRain),
            diag,
        ))
    }

    /// Throttled entry for interactive window resizes.
    pub(crate) fn on_resize_throttled(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        let now = std::time::Instant::now();
        let reflows = self.resize_changes_columns(wid, size);
        let apply_now = !reflows
            || self.windows.get(&wid).is_none_or(|ws| {
                ws.last_resize_at
                    .is_none_or(|t| now.saturating_duration_since(t) >= crate::RESIZE_THROTTLE)
            });
        if apply_now {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.last_resize_at = Some(now);
                ws.pending_resize = None;
                ws.next_resize_settle = None;
            }
            self.on_resize(wid, size);
            if !reflows
                && let Some(ws) = self.windows.get_mut(&wid)
                && ws.next_resize_settle.is_none()
            {
                ws.pending_resize = Some(size);
                ws.next_resize_settle = Some(now + crate::RESIZE_THROTTLE);
            }
        } else if let Some(ws) = self.windows.get_mut(&wid) {
            ws.pending_resize = Some(size);
            let base = ws.last_resize_at.unwrap_or(now);
            ws.next_resize_settle = Some(base + crate::RESIZE_THROTTLE);
        }
    }

    /// Apply a coalesced resize whose settle deadline fired.
    pub(crate) fn flush_pending_resize(&mut self, wid: WindowId) {
        let Some(size) = self.windows.get(&wid).and_then(|ws| ws.pending_resize) else {
            return;
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_resize_at = Some(std::time::Instant::now());
            ws.pending_resize = None;
            ws.next_resize_settle = None;
        }
        self.on_resize(wid, size);
        if self.windows.get(&wid).is_some_and(|ws| ws.panes_stale) {
            self.resize_panes_scoped(wid, false);
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
            w.request_redraw();
        }
    }

    /// Resize entry point for the ledger (`site=window`).
    #[track_caller]
    pub(crate) fn on_resize(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Window);
        self.on_resize_inner(wid, size);
    }

    fn on_resize_inner(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        let size = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map_or(size, |w| w.inner_size());
        if let Some(live) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map(|w| w.scale_factor())
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| (ws.scale - live).abs() > f64::EPSILON)
        {
            self.on_scale_factor_changed(wid, live);
        }
        if cfg!(target_os = "macos")
            && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone())
        {
            let fullscreen = w.fullscreen().is_some();
            let decorated = w.is_decorated();
            let measured_pts = self.apprt.titlebar_band_pts(&w);
            let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
            let (head_pts, band_memory_pts) = titlebar_band_decision(
                measured_pts,
                fullscreen,
                decorated,
                self.windows
                    .get(&wid)
                    .map_or(0.0, |ws| ws.last_windowed_band_pts),
            );
            let head = self.deck_head_px(wid, head_pts, scale, size.height);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.last_windowed_band_pts = band_memory_pts;
                if ws.head_pts != head_pts || ws.metrics.head != head {
                    ws.head_pts = head_pts;
                    ws.metrics.head = head;
                    self.backend.set_head(head);
                }
            }
        }
        self.sync_synthetic_strip_head(wid, size.height);
        let (rows, cols) = self.grid_dims_for(wid, size);
        self.input(
            wid,
            InputEvent::Resize {
                rows,
                cols,
                echo_to_window: false,
            },
            Source::Human,
        );
    }

    fn sync_synthetic_strip_head(&mut self, wid: WindowId, frame_h_px: u32) {
        if cfg!(any(windows, target_os = "linux"))
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.os_window.is_some())
        {
            let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
            let cell_h = self.win_cell_size(wid).1;
            let strip = self.synthetic_strip_head_px(scale, cell_h);
            let strip_pts = strip as f64 / if scale > 0.0 { scale } else { 1.0 };
            let head = self.deck_head_px(wid, strip_pts, scale, frame_h_px);
            if let Some(ws) = self.windows.get_mut(&wid)
                && ws.metrics.head != head
            {
                ws.head_pts = strip_pts;
                ws.metrics.head = head;
                self.backend.set_head(head);
            }
        }
    }

    /// Chrome rows changed (tab strip or status bar).
    #[track_caller]
    pub(crate) fn regrid_for_chrome_rows(&mut self) {
        if let Some(batch) = self.chrome_regrid_batch.as_mut() {
            batch.all();
            return;
        }
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Chrome);
        self.regrid_for_chrome_rows_inner(None);
    }

    /// The tab strip takes `rows` rows at `now` (a config load or reload).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "StripRise",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "StripUnwant",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "StripBack",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    pub(crate) fn set_tab_strip_rows(&mut self, rows: u16, now: std::time::Instant) {
        self.strip_fall = None;
        if rows == self.tab_strip_rows {
            return;
        }
        if aterm_messages::chrome::is_rise(self.tab_strip_rows, rows) {
            self.note_chrome_rise_everywhere(now);
        } else if !aterm_messages::chrome::fall_allowed(self.band_regrow_floor(), now) {
            self.strip_fall = Some(rows);
            return;
        }
        self.tab_strip_rows = rows;
        self.regrid_for_chrome_rows();
    }

    /// The tab strip's waiting fall ([`Self::set_tab_strip_rows`]) once the windows' regrow floor has run at `now`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ChromeRegrowFloor",
            action = "StripFall",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    pub(crate) fn settle_strip_fall(&mut self, now: std::time::Instant) -> bool {
        let Some(rows) = self.strip_fall else {
            return false;
        };
        if !aterm_messages::chrome::fall_allowed(self.band_regrow_floor(), now) {
            return false;
        }
        self.strip_fall = None;
        if rows == self.tab_strip_rows {
            return false;
        }
        self.tab_strip_rows = rows;
        self.regrid_for_chrome_rows();
        true
    }

    /// When the tab strip's waiting fall may commit.
    pub(crate) fn strip_fall_due(&self) -> Option<std::time::Instant> {
        self.strip_fall.and(self.band_regrow_floor())
    }

    /// [`Self::regrid_for_chrome_rows`] but for window `settling`, which its caller fits ([`Self::sync_strip_header_row`]).
    #[cfg(any(test, target_os = "linux"))]
    #[track_caller]
    pub(crate) fn regrid_for_chrome_rows_but(&mut self, settling: WindowId) {
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Chrome);
        self.regrid_for_chrome_rows_inner(Some(settling));
    }

    fn regrid_for_chrome_rows_inner(&mut self, settling: Option<WindowId>) {
        #[cfg(test)]
        {
            crate::resize_ledger::note_chrome_regrid_site_for_test();
            let windows: Vec<WindowId> = self
                .windows
                .keys()
                .copied()
                .filter(|wid| Some(*wid) != settling)
                .collect();
            for wid in windows {
                crate::app_presence::note_chrome_regrid_applied(self, wid);
            }
        }
        let sized: Vec<(WindowId, PhysicalSize<u32>)> = self
            .windows
            .iter_mut()
            .filter_map(|(wid, ws)| {
                ws.tab_segments.clear();
                ws.last_strip_fp = None;
                ws.last_present = None;
                ws.os_window.as_ref().map(|w| (*wid, w.inner_size()))
            })
            .collect();
        for (wid, size) in sized {
            if Some(wid) == settling {
                self.sync_synthetic_strip_head(wid, size.height);
            } else {
                self.on_resize(wid, size);
            }
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                #[cfg(target_os = "linux")]
                w.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
                w.request_redraw();
            }
        }
        self.refresh_band_hover();
    }

    /// The `(rows, cols)` window `wid` gets for raw `size`.
    pub(crate) fn grid_dims_for(&self, wid: WindowId, size: PhysicalSize<u32>) -> (u16, u16) {
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let cols = aterm_render::pad_split(size.width as usize, pad, cw).cells as u16;
        let win_rows = crate::asymmetric_pad_cells(
            (size.height as usize).saturating_sub(self.win_head(wid)),
            self.win_pad_top(wid),
            pad,
            ch,
        ) as u16;
        let rows = win_rows.saturating_sub(self.chrome_rows(wid)).max(1);
        (rows, cols)
    }

    /// Minimum inner size that aligns the resize-increment lattice with the whole-cell frame lattice for window `wid`.
    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn whole_cell_min_size(&self, wid: WindowId) -> PhysicalSize<u32> {
        let (cw, ch) = self.win_cell_size(wid);
        let (cw, ch) = (cw.max(1), ch.max(1));
        let pad = self.win_pad(wid);
        let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
        let floor_w = (164.0 * scale).round().max(0.0) as usize;
        let floor_h = (98.0 * scale).round().max(0.0) as usize;
        let base_w = 2 * pad + cw;
        let min_w = base_w + floor_w.saturating_sub(base_w).div_ceil(cw) * cw;
        let base_h = self.win_head(wid)
            + self.win_pad_top(wid)
            + pad
            + (1 + usize::from(self.chrome_rows(wid))) * ch;
        let min_h = base_h + floor_h.saturating_sub(base_h).div_ceil(ch) * ch;
        PhysicalSize::new(
            u32::try_from(min_w).unwrap_or(u32::MAX),
            u32::try_from(min_h).unwrap_or(u32::MAX),
        )
    }

    fn resize_changes_columns(&self, wid: WindowId, size: PhysicalSize<u32>) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return true;
        };
        let size = ws.os_window.as_ref().map_or(size, |w| w.inner_size());
        let (_rows, cols) = self.grid_dims_for(wid, size);
        cols != ws.cols
    }

    /// Live font zoom (Cmd-+/Cmd--/Cmd-0).
    pub(crate) fn set_font_size(&mut self, size: f32) {
        let _ = self.set_font_size_with(size, Self::rebuild_backend);
    }

    /// Transactional core of [`Self::set_font_size`].
    pub(crate) fn set_font_size_with(
        &mut self,
        size: f32,
        rebuild: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        let size = size.clamp(FONT_PX_MIN, FONT_PX_MAX);
        if (size - self.font_size).abs() < 0.01 {
            return true;
        }
        let previous = (self.font_size, self.font_px);
        self.font_size = size;
        self.font_px = crate::font_px_for_scale(size, self.front_scale());
        self.refresh_all_window_metrics();
        if rebuild(self) {
            true
        } else {
            (self.font_size, self.font_px) = previous;
            self.refresh_all_window_metrics();
            false
        }
    }

    /// The display scale the shared backend's px is derived at outside a window's own redraw.
    pub(crate) fn front_scale(&self) -> f64 {
        resolve_force_scale()
            .or_else(|| self.front().map(|ws| ws.scale))
            .unwrap_or(1.0)
    }

    fn finish_reload_render_transaction_with(
        &mut self,
        previous: ReloadRenderSnapshot,
        rebuild: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        if rebuild(self) {
            true
        } else {
            previous.rollback(self);
            aterm_log::warn!(
                "config reload: renderer rejected font/geometry changes; kept prior render state"
            );
            false
        }
    }

    /// The live glyph-affecting knobs, input to `Backend::pin_glyph_raster_knobs`.
    pub(crate) fn glyph_raster_knobs(&self) -> GlyphRasterKnobs {
        GlyphRasterKnobs {
            text_shaping: self.text_shaping.clone(),
            font_thicken: self.font_thicken,
            stem_gamma: self.stem_gamma,
            font_hinting: self.font_hinting.clone(),
            line_height: self.render_knobs.line_height,
            adjust_baseline: self.render_knobs.adjust_baseline,
        }
    }

    /// Re-pin every memory-backed App-owned renderer setting onto a new or re-faced backend.
    pub(crate) fn pin_backend_render_config_core(&mut self) {
        let raster = self.glyph_raster_knobs();
        self.backend.pin_glyph_raster_knobs(&raster);
        self.backend.set_text_blending(self.text_blending);
        let subpixel = self.font_subpixel.clone();
        self.backend.set_font_subpixel(&subpixel);
        self.backend
            .set_minimum_contrast(self.render_knobs.minimum_contrast);
        self.backend
            .set_selection_fg(self.render_knobs.selection_fg);
        let (upos, uthick) = self.render_knobs.adjust_underline;
        self.backend.set_adjust_underline(upos, uthick);
        self.backend
            .set_underline_skip_descenders(self.render_knobs.underline_skip_descenders);
        let _ = self.video_abort_translucent_swapchain(
            self.render_knobs.background_opacity,
            crate::HOST_CAPTURE.compositor_blends_client,
        );
        self.backend
            .set_background_opacity(self.render_knobs.background_opacity);
        if !self.backend.is_gpu() && !self.backend_kind_undecided() {
            if self.render_knobs.background_opacity < 1.0 {
                warn_background_opacity_unimplemented_once();
            }
            if self.render_knobs.background_material != BackgroundMaterial::None {
                warn_background_material_unimplemented_once();
            }
        }
    }

    /// Rebuild the [`Backend`] from the current `font_px` and `theme`, re-grid and repaint (font zoom, [`Self::set_font_size`]; config reload runs the same body with worker-prepared fonts).
    #[must_use = "a failed rebuild leaves the prior renderer live and requires caller policy"]
    pub(crate) fn rebuild_backend(&mut self) -> bool {
        self.rebuild_backend_with_prepared(None)
    }

    fn rebuild_backend_with_prepared(
        &mut self,
        prepared: Option<(
            aterm_render::Renderer,
            crate::tray_raster::PreparedChromeFonts,
        )>,
    ) -> bool {
        let pad = self.backend.pad();
        let pad_top = self.backend.pad_top();
        let head = self.backend.head();
        let mut prepared_chrome = None;
        if let Some((mut renderer, chrome)) = prepared {
            prepared_chrome = Some(chrome);
            renderer.set_px(self.font_px);
            self.backend.ready_mut().install_prepared_font(
                renderer,
                self.font_family.clone(),
                self.theme,
            );
            self.settle_deferred_font_seal();
        } else {
            self.redeem_deferred_font_seal();
            if let Err(error) = self
                .backend
                .rebuild_font_from_admitted(self.font_px, self.theme)
            {
                crate::logging::stderr_line!(
                    "aterm-gui: resident font generation rebuild failed: {error}"
                );
                return false;
            }
        }
        self.backend.set_pad(pad);
        self.backend.set_pad_top(pad_top);
        self.backend.set_head(head);
        self.pin_backend_render_config_core();
        let _ = self.video_abort_backend_rebuild();
        self.reset_window_raster_caches();
        let sized: Vec<(WindowId, PhysicalSize<u32>)> = self
            .windows
            .iter()
            .filter_map(|(wid, ws)| ws.os_window.as_ref().map(|w| (*wid, w.inner_size())))
            .collect();
        for (wid, size) in sized {
            let (cw, ch) = self.win_cell_size(wid);
            #[cfg(target_os = "linux")]
            let min_size = self.whole_cell_min_size(wid);
            if let Some(ws) = self.windows.get_mut(&wid) {
                if size.width > 0 && size.height > 0 {
                    ws.win_px = Some(size);
                }
                if let Some(w) = ws.os_window.as_ref() {
                    w.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
                    #[cfg(target_os = "linux")]
                    w.set_min_inner_size(Some(min_size));
                }
            }
            self.on_resize(wid, size);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.rearm_present(true);
            }
        }
        if let Some(chrome) = prepared_chrome {
            crate::tray_raster::set_prepared_chrome_fonts(chrome);
            self.redraw_native_leaves_for_landed_fonts();
        } else {
            self.sync_chrome_fonts();
        }
        if self.backend.is_gpu() {
            self.apply_window_vibrancy();
        }
        true
    }

    /// Reset every per-window raster cache after the face or device changed.
    pub(crate) fn reset_window_raster_caches(&mut self) {
        self.introspect_gpu = aterm_gpu::WindowGpu::new();
        for ws in self.windows.values_mut() {
            ws.invalidate_raster_caches();
            if let Some(
                PresentTarget::Gpu { window_gpu, .. } | PresentTarget::Virtual { window_gpu },
            ) = &mut ws.present
            {
                let edr = window_gpu.edr_max();
                let sdr_white_scale = window_gpu.sdr_white_scale();
                let capture_color_space = window_gpu.capture_color_space();
                *window_gpu = aterm_gpu::WindowGpu::new();
                window_gpu.set_edr_max(edr);
                window_gpu.set_sdr_white_scale(sdr_white_scale);
                window_gpu.set_capture_color_space(capture_color_space);
            }
            if ws.os_window.is_none() {
                let _ = ws.present_retry.on_external_stimulus();
            }
        }
    }

    /// Hand the renderer's primary face bytes (and the discovered `-Bold` sibling) to [`crate::tray_raster`] so the Settings/About/Palette overlays use the user's terminal font.
    pub(crate) fn sync_chrome_fonts(&mut self) {
        let (font_px, theme) = (self.font_px, self.theme);
        let (primary, bold, semantic) = match self.backend.ready_mut() {
            Backend::Cpu(r) => (
                r.chrome_primary_face(),
                r.chrome_bold_face(),
                r.fork_semantic_surface(font_px, theme),
            ),
            Backend::Gpu(g) => (
                g.chrome_primary_face(),
                g.chrome_bold_face(),
                g.fork_semantic_surface(font_px, theme),
            ),
        };
        crate::tray_raster::set_chrome_fonts(primary, bold, semantic);
        // A native page drawn before these faces is stale under its stamp
        // ([`crate::tray_raster::chrome_coverage_epoch`]): the first present draws before
        // they are installed.
        self.redraw_native_leaves_for_landed_fonts();
    }

    /// Route one renderer knob change to exactly one renderer call (pinned by the config round-trip test).
    pub(crate) fn apply_render_knob(&mut self, change: KnobChange) {
        match change {
            KnobChange::LineHeight(v) => self.backend.set_line_height(v),
            KnobChange::MinimumContrast(v) => self.backend.set_minimum_contrast(v),
            KnobChange::SelectionFg(v) => self.backend.set_selection_fg(v),
            KnobChange::SelectionInactive(_) => {}
            KnobChange::AdjustBaseline(v) => self.backend.set_adjust_baseline(v),
            KnobChange::AdjustUnderline(p, t) => self.backend.set_adjust_underline(p, t),
            KnobChange::UnderlineSkipDescenders(v) => {
                self.backend.set_underline_skip_descenders(v);
            }
            KnobChange::BackgroundOpacity(v) => {
                let _ = self.video_abort_translucent_swapchain(
                    v,
                    crate::HOST_CAPTURE.compositor_blends_client,
                );
                self.backend.set_background_opacity(v);
                if self.backend.is_gpu() {
                    self.apply_window_vibrancy();
                } else if v < 1.0 && !self.backend_kind_undecided() {
                    warn_background_opacity_unimplemented_once();
                }
            }
            KnobChange::BackgroundMaterial(m) => {
                if self.backend.is_gpu() {
                    // Windows Mica/Acrylic: update the GPU renderer first so the
                    // redraw reconciles the swapchain composite mode (Opaque ⇄
                    // PreMultiplied on a DComp visual). Material → none must go
                    // opaque; none → material re-engages only if the instance was
                    // built as a visual. A plain HWND swapchain stays DWM-chrome
                    // only, which `window_set_vibrancy` diagnoses.
                    if let Some(g) = self.backend.gpu_mut() {
                        g.set_backdrop_margins(m != BackgroundMaterial::None);
                    }
                    self.apply_window_vibrancy();
                } else if m != BackgroundMaterial::None && !self.backend_kind_undecided() {
                    warn_background_material_unimplemented_once();
                }
            }
        }
    }

    /// Apply the backdrop (macOS `NSVisualEffectView`, Windows dwm) and the window/layer opacity to every attached window from the resolved knobs, at attach and on a `background_opacity` / `background_material` reload.
    pub(crate) fn apply_window_vibrancy(&self) {
        let material = self.render_knobs.background_material;
        let translucent =
            aterm_render::vibrancy::is_translucent(self.render_knobs.background_opacity);
        let bg = self.theme.bg;
        let apprt = &self.apprt;
        for ws in self.windows.values() {
            if let Some(w) = ws.os_window.as_ref() {
                apprt.window_set_vibrancy(w, material, translucent, bg);
                w.request_redraw();
            }
        }
    }

    /// Live theme-only swap, the cheap sibling of [`Self::rebuild_backend`], reached through [`Self::install_colors`] (config reload, [`Self::sync_app_theme_to_appearance`]).
    pub(crate) fn apply_theme_live(&mut self, new_theme: Theme) {
        self.theme = new_theme;
        self.rain_dirty = true;
        match self.backend.ready_mut() {
            Backend::Cpu(r) => r.set_theme(new_theme),
            Backend::Gpu(g) => g.set_theme(new_theme),
        }
        self.introspect_gpu.invalidate_present();
        self.publish_theme_chrome();
        self.sync_chrome_fonts();
    }

    /// What the native (macOS toolbar) tab strip pins from the authored chrome.
    pub(crate) fn native_strip_chrome(
        &self,
    ) -> (aterm_messages::tokens::AuthoredChrome, Option<[u8; 3]>) {
        let authored = self.installed_colors.authored_chrome();
        let bg = crate::settings::u32_rgb(self.theme.bg);
        let accent = authored.accent.map(|a| {
            aterm_messages::ink::ensure_contrast(a, bg, aterm_messages::tokens::ACCENT_FLOOR)
        });
        (authored, accent)
    }

    /// Push the installed [`Self::theme`] to the native chrome around every window.
    pub(crate) fn publish_theme_chrome(&mut self) {
        #[cfg(target_os = "linux")]
        let linux_chrome_theme = self.chrome_theme_for_apprt();
        let bg = self.theme.bg;
        let apprt = &self.apprt;
        let toolbars = &self._toolbars;
        let strip_dark = crate::tab_bar::theme_is_dark(bg);
        let (authored, strip_accent) = self.native_strip_chrome();
        crate::tray_raster::forget_kept_glyphs();
        for (wid, ws) in self.windows.iter_mut() {
            ws.last_strip_fp = None;
            ws.invalidate_raster_caches();
            if let Some(w) = ws.os_window.as_ref() {
                apprt.window_set_background_color(w, bg);
                #[cfg(target_os = "linux")]
                apprt.window_set_appearance(w, linux_chrome_theme);
                w.request_redraw();
            }
            if let Some(handle) = toolbars.get(wid) {
                crate::toolbar::set_strip_dark(handle, strip_dark);
                crate::toolbar::set_active_tab_color(handle, authored.active_tab);
                crate::toolbar::set_strip_accent(handle, strip_accent);
            }
        }
    }

    /// Handle `WindowEvent::ScaleFactorChanged`.
    pub(crate) fn on_scale_factor_changed(&mut self, wid: WindowId, scale: f64) {
        // A force-pinned scale ⇒ DPI is intentionally fixed; ignore.
        if resolve_force_scale().is_some() {
            return;
        }
        let size = self.font_size;
        let (pad_l, pad_top_l) = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );
        let mut attached = None;
        if let Some(ws) = self.windows.get_mut(&wid) {
            if (ws.scale - scale).abs() > f64::EPSILON {
                crate::tray_raster::forget_kept_glyphs();
            }
            ws.scale = scale;
            if ws.os_window.is_some() {
                attached = Some(ws.head_pts);
                ws.metrics = crate::MetricsView::for_scale_padded(size, scale, pad_l, pad_top_l);
            }
        }
        if let Some(head_pts) = attached {
            let head = self.deck_head_px(wid, head_pts, scale, 0);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.metrics.head = head;
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        let (cw, ch) = self.win_cell_size(wid);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
            #[cfg(target_os = "linux")]
            w.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
        }
    }

    /// Re-tune the shared backend's font size and pad to window `wid`'s scale before it composes (top of `redraw_window`), giving per-window DPI on one renderer.
    pub(crate) fn apply_window_scale(&mut self, wid: WindowId) {
        self.sync_cell_pixel_size(wid);
        if let Some(m) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.os_window.is_some())
            .map(|ws| ws.metrics)
        {
            if m.pad != self.backend.pad() {
                self.backend.set_pad(m.pad);
            }
            if m.pad_top != self.backend.pad_top() {
                self.backend.set_pad_top(m.pad_top);
            }
            if m.head != self.backend.head() {
                self.backend.set_head(m.head);
            }
        }
        if resolve_force_scale().is_some() {
            return;
        }
        let Some(m) = self.windows.get(&wid).map(|ws| ws.metrics) else {
            return;
        };
        if (m.font_px - self.font_px).abs() < 0.5
            && m.pad == self.backend.pad()
            && m.pad_top == self.backend.pad_top()
            && m.head == self.backend.head()
        {
            return;
        }
        self.backend.set_pad(m.pad);
        self.backend.set_pad_top(m.pad_top);
        self.backend.set_head(m.head);
        self.font_px = m.font_px;
        self.backend.activate_px(m.font_px);
        if let Some((size, cur)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.win_px.map(|s| (s, (ws.rows, ws.cols))))
        {
            let (rows, cols) = self.grid_dims_for(wid, size);
            if (rows, cols) != cur {
                self.on_resize(wid, size);
            }
        }
    }

    /// Headless boot seal: copy the applied boot metrics (`--scale` pad, `$ATERM_DEV_HEADROOM_PX` head, resolved font) into every never-attached window's record, so [`Self::apply_window_scale`]'s auto-font arm does not re-tune from scale-1 defaults on the first recorded redraw.
    pub(crate) fn seed_headless_boot_metrics(&mut self) {
        let m = crate::MetricsView::applied(
            self.font_px,
            self.backend.pad(),
            self.backend.pad_top(),
            self.backend.head(),
        );
        for ws in self.windows.values_mut() {
            if ws.os_window.is_none() {
                ws.metrics = m;
            }
        }
    }

    /// Toggle borderless full screen on the current monitor (View ▸ Enter Full Screen).
    pub(crate) fn toggle_fullscreen(&self) -> Result<(), &'static str> {
        let Some(wid) = self.frontmost_window else {
            return Err("no window is on screen to full-screen");
        };
        self.toggle_fullscreen_in(wid)
    }

    /// Toggle borderless full screen for the window that received the command.
    pub(crate) fn toggle_fullscreen_in(&self, wid: WindowId) -> Result<(), &'static str> {
        let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) else {
            return Err("no window is on screen to full-screen");
        };
        let next = match w.fullscreen() {
            Some(_) => None,
            None => Some(aterm_window::window::Fullscreen::Borderless(None)),
        };
        w.set_fullscreen(next);
        Ok(())
    }

    fn refresh_path_feeds(&mut self, fresh: PathFeedFps) {
        if fresh == self.path_feed_fps {
            return;
        }
        self.path_feed_fps = fresh;
        self.sparkle_dirty = true;
        for ws in self.windows.values_mut() {
            ws.fx.decos_mut().hard_reset_words();
        }
        for ws in self.windows.values() {
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Publish one admitted asset generation to the App and every window.
    pub(crate) fn publish_config_assets(
        &mut self,
        assets: std::sync::Arc<ConfigAssetCatalog>,
    ) -> usize {
        if std::sync::Arc::ptr_eq(&self.config_assets, &assets) {
            return 0;
        }
        self.config_assets = assets;
        let window_ids = self.windows.keys().copied().collect::<Vec<_>>();
        let installed = window_ids
            .into_iter()
            .filter(|id| self.install_window_config_assets(*id))
            .count();
        self.request_redraw_all_windows();
        installed
    }

    /// Publish a theme-directory generation from the background watcher.
    pub(crate) fn reload_theme_catalog(&mut self, themes: std::sync::Arc<ThemeCatalog>) {
        let before = self.native_config_service.snapshot();
        let snapshot = self.native_config_service.replace_theme_catalog(themes);
        if snapshot.revision == before.revision {
            return;
        }
        self.theme_catalog_generation = self.theme_catalog_generation.saturating_add(1);
        self.publish_config_assets(std::sync::Arc::clone(&snapshot.assets));
        let reason = crate::app_colorscheme::ThemeChange::CatalogReload;
        if !self.request_theme_typography_refresh(reason) {
            self.install_resolved_colors(reason);
        }
        self.publish_native_config_snapshot(&snapshot);

        if *snapshot.config == self.config
            && theme_misses(&self.config, &before.assets.themes)
                != theme_misses(&self.config, &snapshot.assets.themes)
        {
            use crate::message_reporters::ConfigFamily;
            let mut warns = crate::message_reporters::ConfigWarnings::default();
            collect_unaccepted_value_notices(
                &mut warns,
                &self.config,
                &snapshot.assets.themes,
                &snapshot.text,
            );
            for w in warns.sentences() {
                crate::logging::stderr_line!("aterm-gui: {w}");
            }
            let scope = [ConfigFamily::UnacceptedValues.key()];
            self.note_config_settings_shows(&scope, &warns.settings_shown_keys());
            let messages = warns.into_messages_routed(self.settings_in_front());
            self.replace_config_messages_keyed(&scope, messages);
        }
    }

    fn request_font_catalog_generation(
        &mut self,
        mut prepared: crate::native_config_service::PreparedConfigObservation,
        reason: crate::app_colorscheme::ThemeChange,
    ) -> bool {
        let Some(primary) = self.backend.ready().primary_seed() else {
            self.native_config_service.mark_reconciliation_required();
            self.reject_config_watch_admission_for(
                &prepared.observation.baseline,
                crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
            );
            aterm_log::warn!("config reload: active renderer has no immutable primary font bytes");
            return false;
        };
        let Some(previous_sources) = self.backend.admitted_font_sources() else {
            self.native_config_service.mark_reconciliation_required();
            self.reject_config_watch_admission_for(
                &prepared.observation.baseline,
                crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
            );
            aterm_log::warn!("config reload: active renderer font generation is unavailable");
            return false;
        };
        let sequence = self.next_font_catalog_sequence.max(1);
        self.next_font_catalog_sequence = sequence.saturating_add(1);
        self.requested_font_catalog_sequence = sequence;
        if !std::sync::Arc::ptr_eq(&prepared.assets.themes, &self.config_assets.themes) {
            prepared.assets = std::sync::Arc::new(ConfigAssetCatalog {
                trail_style: std::sync::Arc::clone(&prepared.assets.trail_style),
                kitty_sprite: prepared.assets.kitty_sprite.clone(),
                wallpaper: prepared.assets.wallpaper.clone(),
                themes: std::sync::Arc::clone(&self.config_assets.themes),
                sparkle_spec_consumers: prepared.assets.sparkle_spec_consumers.clone(),
            });
        }
        let request = crate::native_font_catalog::Request {
            sequence,
            theme_generation: self.theme_catalog_generation,
            prepared,
            px: self.font_px,
            appearance: self.os_appearance,
            previous_family: self.font_family.clone(),
            previous_font_config: self.font_config.clone(),
            previous_sources,
            previous_variations: self.font_variations.clone(),
            previous_dark_nudge: self.font_weight_dark_nudge,
            primary,
            reason,
        };
        let baseline = request.prepared.observation.baseline.clone();
        if let Some(lane) = self.native_font_catalog.as_mut() {
            if lane.request(request) {
                return true;
            }
        } else if self.proxy.is_none() {
            self.finish_font_catalog_generation(crate::native_font_catalog::prepare(request));
            return true;
        }
        self.finished_font_catalog_sequence = self.finished_font_catalog_sequence.max(sequence);
        self.native_config_service.mark_reconciliation_required();
        self.reject_config_watch_admission_for(
            &baseline,
            crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
        );
        aterm_log::warn!("config reload: font catalog worker unavailable; keeping current config");
        false
    }

    /// The config every font reader sees.
    pub(crate) fn font_view(&self) -> std::borrow::Cow<'_, Config> {
        self.typography.overlay(&self.config)
    }

    fn font_generation_available(&self) -> bool {
        #[cfg(test)]
        if self.font_worker_refuses_for_test {
            return false;
        }
        self.backend.ready().primary_seed().is_some()
            && self.backend.admitted_font_sources().is_some()
            && (self.native_font_catalog.is_some() || self.proxy.is_none())
    }

    /// The theme typography state changed (a catalog reload or an OS flip).
    pub(crate) fn request_theme_typography_refresh(
        &mut self,
        reason: crate::app_colorscheme::ThemeChange,
    ) -> bool {
        let next = self
            .config
            .resolved_typography(self.os_appearance, &self.config_assets.themes);
        if next.face_key() == self.typography.face_key() {
            return false;
        }
        if self.finished_font_catalog_sequence < self.requested_font_catalog_sequence {
            self.held_theme_change = Some(reason.merged(self.held_theme_change));
            return true;
        }
        let Some(observation) = self.native_config_service.admitted_observation() else {
            return false;
        };
        if !self.font_generation_available() {
            return false;
        }
        let snapshot = self.native_config_service.snapshot();
        let prepared = crate::native_config_service::PreparedConfigObservation {
            observation,
            config: (*snapshot.config).clone(),
            values: (*snapshot.semantic_values).clone(),
            disk_faults: std::sync::Arc::clone(&snapshot.disk_faults),
            path_feeds: PreparedPathFeedGeneration {
                sparkle: self.prepared_sparkle.clone(),
                fingerprints: self.path_feed_fps,
            },
            assets: snapshot.assets,
        };
        self.held_theme_change = Some(reason.merged(self.held_theme_change));
        if self.request_font_catalog_generation(prepared, reason) {
            true
        } else {
            self.held_theme_change = None;
            false
        }
    }

    pub(crate) fn finish_font_catalog_generation(
        &mut self,
        mut completion: crate::native_font_catalog::Completion,
    ) {
        if let Some(lane) = self.native_font_catalog.as_mut() {
            lane.worker_drained();
        }
        self.finished_font_catalog_sequence =
            self.finished_font_catalog_sequence.max(completion.sequence);
        match crate::native_font_catalog::completion_disposition(
            self.requested_font_catalog_sequence,
            completion.sequence,
            self.theme_catalog_generation,
            completion.theme_generation,
            self.os_appearance,
            completion.appearance,
        ) {
            crate::native_font_catalog::CompletionDisposition::Publish => {}
            crate::native_font_catalog::CompletionDisposition::RejectStaleConfig => return,
            crate::native_font_catalog::CompletionDisposition::ReprepareLatestTheme => {
                let path_feeds = PreparedPathFeedGeneration {
                    sparkle: completion.sparkle.clone(),
                    fingerprints: completion.path_feed_fps,
                };
                let reason = completion.reason.merged(self.held_theme_change);
                let queued = self.request_font_catalog_generation(
                    crate::native_config_service::PreparedConfigObservation {
                        observation: completion.observation,
                        config: completion.config,
                        values: completion.values,
                        disk_faults: completion.disk_faults,
                        assets: completion.assets,
                        path_feeds,
                    },
                    reason,
                );
                if !queued && let Some(reason) = self.held_theme_change.take() {
                    self.install_resolved_colors(reason);
                }
                return;
            }
        }
        if let Some(fonts) = completion.fonts.as_mut() {
            fonts.renderer.set_px(self.font_px);
        }
        self.apply_prepared_config_generation(completion.into_generation());
    }

    #[cfg(all(test, unix))]
    pub(crate) fn reload_config(&mut self) {
        let Some(path) = config_path() else { return };
        let observation = match crate::native_config_service::VersionedConfigService::observe_path(
            &path, false,
        ) {
            Ok(observation) => observation,
            Err(error) => {
                aterm_log::warn!(
                    "config reload: {error}; keeping current config and Manual document"
                );
                return;
            }
        };
        self.reload_config_observation(observation);
    }

    /// Apply the exact generation sampled by the config host.
    pub(crate) fn reload_config_observation(
        &mut self,
        observation: crate::native_config_service::ConfigDiskObservation,
    ) {
        self.prepare_native_config_external_observation(observation);
    }

    /// Worker-prepared twin of [`Self::reload_config_observation`].
    pub(crate) fn reload_prepared_config_observation(
        &mut self,
        prepared: crate::native_config_service::PreparedConfigObservation,
    ) {
        if let Err(error) = self.refresh_open_config_editor_observation(&prepared.observation) {
            aterm_log::warn!("config reload: Manual refresh needs attention ({error})");
        }
        self.request_font_catalog_generation(
            prepared,
            crate::app_colorscheme::ThemeChange::ConfigReload,
        );
    }

    /// Admit a config generation.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HarnessPolicyOneWriter",
            action = "AdmitOn",
            project = "aterm_gui::config_watcher::tests::project_harness_writer"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HarnessPolicyOneWriter",
            action = "AdmitOff",
            project = "aterm_gui::config_watcher::tests::project_harness_writer"
        )
    )]
    pub(crate) fn apply_prepared_config_generation(
        &mut self,
        generation: crate::native_font_catalog::PreparedConfigGeneration,
    ) {
        if self.native_config_inflight || self.native_config_service.reconciliation_required() {
            self.defer_prepared_config_generation(generation);
            if !self.native_config_inflight
                && let Err(error) = self.pump_native_config()
            {
                self.surface_native_config_lane_error(error);
            }
            return;
        }
        self.apply_prepared_config_generation_unfenced(generation);
    }

    /// Admit a deferred generation after a reconciliation worker sampled the same baseline, which clears the write fence.
    pub(crate) fn apply_reconciled_prepared_config_generation(
        &mut self,
        generation: crate::native_font_catalog::PreparedConfigGeneration,
    ) {
        debug_assert!(!self.native_config_inflight);
        debug_assert!(self.native_config_service.reconciliation_required());
        self.apply_prepared_config_generation_unfenced(generation);
    }

    fn apply_prepared_config_generation_unfenced(
        &mut self,
        generation: crate::native_font_catalog::PreparedConfigGeneration,
    ) {
        let crate::native_font_catalog::PreparedConfigGeneration {
            observation,
            mut config,
            values,
            disk_faults,
            assets: prepared_assets,
            path_feed_fps: fresh_feeds,
            sparkle: prepared_sparkle,
            fonts: mut prepared_fonts,
            warnings: mut font_prepare_warnings,
            font_refusals,
            reason: generation_reason,
        } = generation;
        config.harness = self.harness_policy.get().clone();
        let admitted_baseline = observation.baseline.clone();
        let config_snapshot = match self.native_config_service.synchronize_observation_prepared(
            observation,
            config.clone(),
            values,
            disk_faults,
            prepared_assets,
        ) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.native_config_service.mark_reconciliation_required();
                self.reject_config_watch_admission_for(
                    &admitted_baseline,
                    crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
                );
                self.reject_pending_native_config(&format!(
                    "config generation could not be admitted: {error}"
                ));
                aterm_log::warn!("native config service rejected watcher snapshot: {error}");
                if let Some(reason) = self.held_theme_change.take() {
                    self.install_resolved_colors(reason);
                }
                return;
            }
        };
        self.prepared_sparkle = prepared_sparkle;
        let typography =
            config.resolved_typography(self.os_appearance, &config_snapshot.assets.themes);
        let released = self.held_theme_change.take();
        let install_reason = generation_reason.merged(released);

        if config == self.config
            && prepared_fonts.is_none()
            && typography.face_key() == self.typography.face_key()
        {
            self.typography = typography;
            if released.is_some() {
                self.install_resolved_colors(install_reason);
            }
            self.publish_config_assets(std::sync::Arc::clone(&config_snapshot.assets));
            self.publish_native_config_snapshot(&config_snapshot);
            self.publish_settings_font_refusals(font_refusals);
            self.refresh_path_feeds(fresh_feeds);
            self.finish_native_config_external_admission(&admitted_baseline);
            let mut warns = crate::message_reporters::ConfigWarnings::default();
            collect_key_notices(&mut warns, &config_snapshot.text);
            self.note_removed_settings(&config_snapshot.text);
            collect_unaccepted_value_notices(
                &mut warns,
                &config,
                &config_snapshot.assets.themes,
                &config_snapshot.text,
            );
            for w in warns.sentences() {
                crate::logging::stderr_line!("aterm-gui: {w}");
            }
            let scope = [
                crate::message_reporters::ConfigFamily::IgnoredKeys.key(),
                crate::message_reporters::ConfigFamily::RetiredKeys.key(),
                crate::message_reporters::ConfigFamily::UnacceptedValues.key(),
                crate::message_reporters::KEY_LAUNCH_LOAD,
            ];
            self.note_config_settings_shows(&scope, &warns.settings_shown_keys());
            let messages = warns.into_messages_routed(self.settings_in_front());
            self.replace_config_messages_keyed(&scope, messages);
            return;
        }
        let reload_render_before = ReloadRenderSnapshot::capture(self);

        let restart_notices = restart_notices(&self.config, &config);
        let fixed_as = config_fix_observations(
            &self.config,
            &self.config_assets,
            &config,
            &config_snapshot.assets,
        );

        let old_padding = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );
        let old_stats_bars = self.config.stats_bars_or_default();
        let old_deck_size = self.config.stats_deck_size_or_default();

        let sparkle_feed_changed = deco_feed_changed(
            &self.config,
            &config,
            fresh_feeds.deco,
            self.path_feed_fps.deco,
        );
        self.path_feed_fps = fresh_feeds;

        let audition_voice = self.typing_sound_to_audition_on_swap(&config);
        let claude_footer_changed = self.config.claude_footer != config.claude_footer;
        let ai_changed = self.config.ai != config.ai;
        self.config = config.clone();
        self.reconcile_consent_config();
        let _ = self.reconcile_consent_observer();
        crate::fleet_listen::configure_fleet_listener(self.config.fleet.as_ref());
        self.follow_fleet_watch();
        let view = typography.overlay(&config).into_owned();
        self.font_size_scale_milli = view.font_size_scale_milli_or_default();
        self.typography = typography;
        if claude_footer_changed {
            self.redraw_every_window();
        }
        if ai_changed {
            self.ai_config_changed();
        }
        let secure_input_refusal =
            crate::secure_input::set_desired(self.config.secure_keyboard_entry_or_default()).err();
        if self.config.secure_keyboard_entry_or_default() {
            self.aos_privacy_detach_all(crate::aos_host::SECURE_ENTRY);
        }
        if let Some(voice) = audition_voice {
            self.audition_typing_sound(voice);
        }
        {
            let (authored, strip_accent) = self.native_strip_chrome();
            for handle in self._toolbars.values() {
                crate::toolbar::set_active_tab_color(handle, authored.active_tab);
                crate::toolbar::set_strip_accent(handle, strip_accent);
            }
            for ws in self.windows.values_mut() {
                ws.last_strip_fp = None;
                ws.last_present = None;
            }
        }
        self.settle_reduced_scroll_motion(std::time::Instant::now());
        self.apply_serious_mode(config.serious_mode_or_default());
        self.publish_config_assets(std::sync::Arc::clone(&config_snapshot.assets));
        self.publish_native_config_snapshot(&config_snapshot);
        self.publish_settings_font_refusals(font_refusals);
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        self.refresh_tab_chrome_windows(windows);
        self.reconfigure_session_status();
        self.apply_desktop_alerts(config.desktop_alerts_or_default());
        if config.allow_palette_reconfigure_or_default() {
            self.except_clear(&crate::message_reporters::palette_refused_key());
        }
        self.predict_mode_cache = None;
        self.kitty_cursor_enabled_cache = None;
        self.sparkle_dirty = true;
        self.rain_dirty = true;
        self.adopt_presence_config();
        self.palette_refresh_live();
        self.publish_native_packages_state();
        if sparkle_feed_changed {
            for ws in self.windows.values_mut() {
                ws.fx.decos_mut().hard_reset_words();
            }
        }
        self.settle_theme_preview(install_reason);
        let colors = config.resolved_colors_previewing(
            self.os_appearance,
            &config_snapshot.assets.themes,
            self.theme_preview
                .as_ref()
                .map(|preview| preview.id.as_str()),
        );
        self.session_factory.allow_kitty_file_transfer =
            config.allow_kitty_file_transfer.unwrap_or(false);
        self.session_factory.shell_override = resolve_shell_override(&config);
        self.session_factory
            .shell_args
            .clone_from(&config.shell_args);
        self.mark_status_dirty();
        aterm_pty::set_console_host(aterm_pty::ConsoleHostChoice::from_config(
            config.conpty.as_deref(),
        ));
        self.session_factory.temporal_recording = config.temporal_recording.unwrap_or(false);
        self.spare_config_changed();
        crate::control::set_search_max_lines(search_index_depth(&config));

        use crate::message_reporters::ConfigFamily;
        let mut warns = crate::message_reporters::ConfigWarnings::default();
        let (keybindings, kb_warns) =
            keybinding::Keybindings::resolved_warn(config.keybindings.as_ref());
        let (key_sequences, ks_warns) =
            keybinding::KeySequences::from_config_warn(config.key_sequences.as_ref());
        self.keybindings = keybindings;
        self.key_sequences = key_sequences;
        warns.extend(ConfigFamily::Keybindings, kb_warns);
        warns.extend(ConfigFamily::Keybindings, ks_warns);
        warns.extend(ConfigFamily::Restart, restart_notices);
        collect_key_notices(&mut warns, &config_snapshot.text);
        self.note_removed_settings(&config_snapshot.text);
        if let Some(status) = secure_input_refusal {
            warns.push(
                ConfigFamily::SecureKeyboard,
                format!(
                    "secure_keyboard_entry: the OS refused the change (OSStatus {status}) \u{2014} \
                     Secure Keyboard Entry is NOT {}",
                    if self.config.secure_keyboard_entry_or_default() {
                        "on"
                    } else {
                        "off"
                    }
                ),
            );
        }
        let requested_effective_family = match prepared_fonts.as_ref() {
            Some(prepared) => prepared.family.clone(),
            None => self.font_family.clone(),
        };
        let family_admitted = prepared_fonts.is_some()
            && !font_prepare_warnings
                .iter()
                .any(|w| w.starts_with(crate::native_font_catalog::FAMILY_REFUSED));
        warns.extend_shown_in_settings(ConfigFamily::Fonts, font_prepare_warnings.drain(..));
        if let Some(w) = config.cursor_trail_style_warning() {
            warns.push(ConfigFamily::CursorTrail, format!("config {w}"));
        }
        if let Some(reason) = config_snapshot.assets.kitty_sprite.diagnostic() {
            let source = config_snapshot
                .assets
                .kitty_sprite
                .source_id()
                .unwrap_or("configured source");
            warns.push(
                ConfigFamily::Assets,
                format!("config cursor_nyan_sprite {source:?} invalid: {reason}"),
            );
        }
        if let Some(reason) = config_snapshot.assets.wallpaper.diagnostic() {
            let source = config_snapshot
                .assets
                .wallpaper
                .source_id()
                .unwrap_or("configured source");
            warns.push_shown_in_settings(
                ConfigFamily::Assets,
                format!("config wallpaper {source:?} invalid: {reason}"),
            );
        }
        let new_font_config = prepared_fonts
            .as_ref()
            .map(|prepared| prepared.config.clone())
            .unwrap_or_else(|| self.font_config.clone());
        collect_unaccepted_value_notices(
            &mut warns,
            &config,
            &config_snapshot.assets.themes,
            &config_snapshot.text,
        );
        for w in warns.sentences() {
            crate::logging::stderr_line!("aterm-gui: {w}");
        }
        let all_keys: Vec<&'static str> = ConfigFamily::ALL
            .iter()
            .map(|family| family.key())
            .chain([crate::message_reporters::KEY_FONT_FAMILY])
            .collect();
        let mut shown = warns.settings_shown_keys();
        shown.push(crate::message_reporters::KEY_FONT_FAMILY);
        self.note_config_settings_shows(&all_keys, &shown);
        let messages = warns.into_messages_routed(self.settings_in_front());
        self.replace_config_messages_after_font_work(messages, family_admitted, &fixed_as);
        self.option_as_meta = config.option_as_meta_or_default();
        self.confirm_multiline_paste = config.confirm_multiline_paste_or_default();
        self.copy_on_select = config.copy_on_select_or_default();

        self.set_tab_strip_rows(
            self.strip_rows_for(resolve_tab_strip_rows(&config)),
            std::time::Instant::now(),
        );

        let new_window_theme = config.window_theme_or_default();
        if new_window_theme != self.window_theme {
            self.window_theme = new_window_theme;
            for ws in self.windows.values_mut() {
                ws.last_strip_fp = None;
                ws.invalidate_raster_caches();
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        {
            let chrome_theme = self.chrome_theme_for_apprt();
            let apprt = &self.apprt;
            for ws in self.windows.values() {
                if let Some(w) = ws.os_window.as_ref() {
                    apprt.window_set_appearance(w, chrome_theme);
                }
            }
        }

        let new_colorspace = config.window_colorspace_or_default();
        if new_colorspace != self.window_colorspace {
            self.window_colorspace = new_colorspace;
            let apprt = &self.apprt;
            for ws in self.windows.values_mut() {
                let Some(w) = ws.os_window.as_ref() else {
                    continue;
                };
                let Some(PresentTarget::Gpu {
                    gpu_surface,
                    window_gpu,
                }) = ws.present.as_mut()
                else {
                    continue;
                };
                let surface_colorspace = crate::platform::resolve_surface_colorspace(
                    new_colorspace,
                    gpu_surface.is_hdr(),
                );
                let effective_colorspace =
                    apprt.window_set_surface_colorspace(w, surface_colorspace);
                let previous_capture_space = window_gpu.capture_color_space();
                let capture_space = crate::platform::capture_space_after_surface_tag(
                    effective_colorspace,
                    previous_capture_space,
                );
                window_gpu.set_capture_color_space(capture_space);
            }
        }

        let shimmer_reload = if self.serious_mode_enabled() {
            Some(false)
        } else if self.load_shed_active() {
            None
        } else {
            Some(config.cursor_fire_shimmer_or_default())
        };
        let bloom_reload = if self.serious_mode_enabled() {
            Some(false)
        } else if self.load_shed_active() {
            None
        } else {
            Some(config.cursor_trail_bloom_or_default())
        };
        let bloom_radius = config.cursor_trail_bloom_radius_or_default(self.trail_presentation());
        if let Backend::Gpu(g) = self.backend.ready_mut() {
            g.set_hdr_glow(config.hdr_glow_or_default());
            g.set_sdr_glow_boost(config.cursor_glow_sdr_boost_or_default());
            if let Some(on) = shimmer_reload {
                g.set_shimmer(on);
            }
            if let Some(on) = bloom_reload {
                g.set_bloom(on);
            }
            g.set_bloom_params(
                config.cursor_trail_bloom_strength_or_default(),
                bloom_radius,
            );
            if config.warms_effect_pipelines() {
                g.warm_effect_pipelines();
            }
        }

        self.recompute_focus_boost();

        let new_theme = colors.theme();
        let new_default_font_size = resolve_scaled_font_px(&view);
        let default_changed = (new_default_font_size - self.default_font_size).abs() > 0.01;
        self.default_font_size = new_default_font_size;
        let new_font_size = if default_changed {
            new_default_font_size
        } else {
            self.font_size
        };
        let new_font_px = crate::font_px_for_scale(new_font_size, self.front_scale());
        self.font_size = new_font_size;
        let theme_changed = new_theme != self.theme;
        let font_changed = (new_font_px - self.font_px).abs() >= 0.5;
        let effective_family = requested_effective_family;
        let family_changed = effective_family != self.font_family;
        let new_shaping = view.text_shaping();
        let shaping_changed = new_shaping != self.text_shaping;
        self.text_shaping = new_shaping;
        let new_blending = config.text_blending_or_default();
        let new_thicken = config.font_thicken_or_default();
        let new_stem_gamma = config.stem_gamma_or_default();
        let new_font_hinting = config.font_hinting_or_default();
        let new_font_subpixel = config.font_subpixel_or_default();
        let typography_changed = new_blending != self.text_blending
            || new_thicken != self.font_thicken
            || (new_stem_gamma - self.stem_gamma).abs() > f32::EPSILON
            || new_font_hinting != self.font_hinting
            || new_font_subpixel != self.font_subpixel;
        self.text_blending = new_blending;
        self.font_thicken = new_thicken;
        self.stem_gamma = new_stem_gamma;
        self.font_hinting = new_font_hinting;
        self.font_subpixel = new_font_subpixel;
        let new_knobs = RenderKnobs::from_config(&view, &colors);
        let knob_changes = self.render_knobs.diff(&new_knobs);
        self.render_knobs = new_knobs;
        let font_cfg_changed = new_font_config != self.font_config;
        self.font_config = new_font_config;
        let (new_variations, _vf_warns) = view.font_variation_requests();
        let new_dark_nudge = view.font_weight_dark_nudge_or_default();
        let variations_changed = new_variations != self.font_variations
            || (new_dark_nudge - self.font_weight_dark_nudge).abs() > f32::EPSILON;
        self.font_variations = new_variations;
        self.font_weight_dark_nudge = new_dark_nudge;
        let geometry_knob_changed = knob_changes
            .iter()
            .any(|c| matches!(c, KnobChange::LineHeight(_)));
        let prepared_font_refresh = prepared_fonts.is_some();
        let backend_rebuild_needed = font_changed
            || family_changed
            || geometry_knob_changed
            || variations_changed
            || font_cfg_changed
            || prepared_font_refresh;
        self.font_px = new_font_px;
        self.refresh_all_window_metrics();
        let new_padding = (
            config.window_padding_or_default(),
            config.window_padding_top_or_default(),
        );
        let new_stats_bars = config.stats_bars_or_default();
        let new_deck_size = config.stats_deck_size_or_default();
        let deck_moved = old_stats_bars.takes_room() != new_stats_bars.takes_room()
            || (new_stats_bars.takes_room() && old_deck_size != new_deck_size);
        crate::menu::set_stats_bars_form(new_stats_bars);
        crate::menu::set_deck_size_pref(new_deck_size);
        if old_stats_bars != new_stats_bars {
            for ws in self.windows.values_mut() {
                ws.stats_lens = crate::stats_lens::LensSlot::default();
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        if !backend_rebuild_needed && (old_padding != new_padding || deck_moved) {
            let _site = deck_moved
                .then(|| crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Deck));
            let sized: Vec<(WindowId, PhysicalSize<u32>)> = self
                .windows
                .iter()
                .filter_map(|(wid, ws)| ws.win_px.map(|size| (*wid, size)))
                .collect();
            for (wid, size) in sized {
                self.on_resize(wid, size);
            }
            for ws in self.windows.values() {
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        }
        let mut rolled_back = false;
        if backend_rebuild_needed {
            self.theme = new_theme;
            self.font_family = effective_family;
            let prepared = prepared_fonts
                .take()
                .map(|fonts| (fonts.renderer, fonts.chrome));
            let rebuild_succeeded = self
                .finish_reload_render_transaction_with(reload_render_before, move |app| {
                    app.rebuild_backend_with_prepared(prepared)
                });
            rolled_back = !rebuild_succeeded;
            if rebuild_succeeded && theme_changed {
                self.publish_theme_chrome();
            }
        } else if theme_changed {
            self.font_family = effective_family;
            if shaping_changed {
                self.backend.set_text_shaping(self.text_shaping.clone());
            }
            if typography_changed {
                self.backend.set_text_blending(self.text_blending);
                self.backend.set_font_thicken(self.font_thicken);
                self.backend.set_stem_gamma(self.stem_gamma);
                let hinting = self.font_hinting.clone();
                self.backend.set_font_hinting(&hinting);
                let subpixel = self.font_subpixel.clone();
                self.backend.set_font_subpixel(&subpixel);
            }
            for &change in &knob_changes {
                self.apply_render_knob(change);
            }
        } else if shaping_changed || typography_changed || !knob_changes.is_empty() {
            self.backend.set_text_shaping(self.text_shaping.clone());
            self.backend.set_text_blending(self.text_blending);
            self.backend.set_font_thicken(self.font_thicken);
            self.backend.set_stem_gamma(self.stem_gamma);
            let hinting = self.font_hinting.clone();
            self.backend.set_font_hinting(&hinting);
            let subpixel = self.font_subpixel.clone();
            self.backend.set_font_subpixel(&subpixel);
            for &change in &knob_changes {
                self.apply_render_knob(change);
            }
            for ws in self.windows.values_mut() {
                ws.invalidate_raster_caches();
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
        } else {
            // No backend rebuild, but the engine config may have changed cells, so
            // still request a repaint (the redraw early-out skips it if nothing moved).
            if let Some(w) = self.front().and_then(|ws| ws.os_window.as_ref()) {
                w.request_redraw();
            }
        }
        if !rolled_back {
            self.install_colors(colors, install_reason);
        }
        if !backend_rebuild_needed
            && !theme_changed
            && (shaping_changed || typography_changed || !knob_changes.is_empty())
        {
            self.sync_chrome_fonts();
        }
        self.warn_font_feature_issues();
        self.finish_native_config_external_admission(&admitted_baseline);
    }

    /// Interior padding (device px per edge) at `scale` under the live config.
    pub(crate) fn cfg_pad_for_scale(&self, scale: f64) -> usize {
        crate::logical_to_device_px(self.config.window_padding_or_default(), scale)
    }

    /// Top-edge twin of [`Self::cfg_pad_for_scale`], capped at the base pad in device px (rounding could exceed it at fractional scales).
    pub(crate) fn cfg_pad_top_for_scale(&self, scale: f64) -> usize {
        crate::logical_to_device_px(self.config.window_padding_top_or_default(), scale)
            .min(self.cfg_pad_for_scale(scale))
    }

    /// Synthetic tab-band head in device px for a window at `scale` with cell height `cell_h`.
    pub(crate) fn synthetic_strip_head_px(&self, scale: f64, cell_h: usize) -> usize {
        let target = self.config.tab_band_height_or_default().target_logical_px();
        let foot = if target > 0.0 {
            crate::tab_bar::STRIP_FOOT_LOGICAL_PX
        } else {
            0.0
        };
        synthetic_band_head_px(
            target + foot,
            self.cfg_pad_top_for_scale(scale),
            self.tab_strip_rows,
            cell_h,
            scale,
        )
    }

    /// Re-resolve every window's [`MetricsView`](crate::MetricsView) from the live font size.
    pub(crate) fn refresh_all_window_metrics(&mut self) {
        let forced = resolve_force_scale();
        let size = self.font_size;
        let (pad_l, pad_top_l) = (
            self.config.window_padding_or_default(),
            self.config.window_padding_top_or_default(),
        );
        let mut attached = Vec::new();
        for (wid, ws) in self.windows.iter_mut() {
            let (pad, pad_top) = if let Some(w) = ws.os_window.as_ref() {
                attached.push((*wid, ws.head_pts, ws.scale, w.inner_size().height));
                let pad = crate::logical_to_device_px(pad_l, ws.scale);
                (
                    pad,
                    crate::logical_to_device_px(pad_top_l, ws.scale).min(pad),
                )
            } else {
                (ws.metrics.pad, ws.metrics.pad_top)
            };
            let font_px = crate::font_px_for_scale(size, forced.unwrap_or(ws.scale));
            ws.metrics = crate::MetricsView::applied(font_px, pad, pad_top, ws.metrics.head);
        }
        for (wid, head_pts, scale, frame_h) in attached {
            let head = self.deck_head_px(wid, head_pts, scale, frame_h);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.metrics.head = head;
            }
        }
        let wids: Vec<crate::WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            self.sync_cell_pixel_size(wid);
        }
    }

    /// Warn once per config load about `font_features` that cannot take effect.
    pub(crate) fn warn_font_feature_issues(&self) {
        let Some(specs) = self.config.font_features.as_deref() else {
            return;
        };
        let rejected: Vec<&str> = specs
            .iter()
            .flat_map(|s| s.split_whitespace())
            .filter(|t| aterm_types::text_shaping::FontFeature::parse_token(t).is_none())
            .collect();
        if !rejected.is_empty() {
            crate::logging::stderr_line!(
                "aterm-gui: config font_features: ignored unparseable token(s) {rejected:?} \
                 (use a 1–4 char tag; optional +/- prefix or tag=value)"
            );
        }
        let unsupported = self.backend.unsupported_user_feature_tags();
        if !unsupported.is_empty() {
            let tags: Vec<String> = unsupported
                .iter()
                .map(|t| String::from_utf8_lossy(t).trim_end().to_string())
                .collect();
            crate::logging::stderr_line!(
                "aterm-gui: config font_features: the active font does not provide {tags:?}; \
                 those have no effect (choose a font that carries these OpenType features)"
            );
        }
    }
}

#[cfg(test)]
mod reload_render_transaction_tests {
    use super::{Config, FontConfig, ReloadRenderSnapshot, RenderKnobs};

    #[test]
    fn failed_backend_rebuild_rolls_back_complete_render_slice_only() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let old_config = app.config.clone();
        let old_default_font_size = app.default_font_size;
        let old_font_size = app.font_size;
        let old_font_px = app.font_px;
        let old_theme = app.theme;
        let old_family = app.font_family.clone();
        let old_shaping = app.text_shaping.clone();
        let old_blending = app.text_blending;
        let old_thicken = app.font_thicken;
        let old_stem_gamma = app.stem_gamma;
        let old_variations = app.font_variations.clone();
        let old_dark_nudge = app.font_weight_dark_nudge;
        let old_knobs = app.render_knobs;
        let old_font_config = app.font_config.clone();
        let old_typography = app.typography.clone();
        let old_size_scale = app.font_size_scale_milli;
        let old_metrics = app.windows[&wid].metrics;
        let previous = ReloadRenderSnapshot::capture(&app);

        let candidate: Config = aterm_toml::from_str(
            r##"
font_px = 23.0
theme = "Dracula"
foreground = "#f0f1f2"
background = "#101112"
cursor_color = "#aabbcc"
selection_color = "#334455"
palette = ["#010203", "#040506"]
font_family = "Menlo"
font_family_bold = "Menlo Bold"
font_family_italic = "Menlo Italic"
font_family_bold_italic = "Menlo Bold Italic"
font_synthetic_style = false
fallback_fonts = ["Menlo"]
symbol_font = "Menlo"
emoji_font = "Menlo"
ligatures = false
merged_ligatures = true
font_features = ["ss01"]
text_blending = "linear"
font_thicken = true
font_variation = ["wght=550"]
font_weight = 600
font_weight_dark_nudge = 40.0
stem_gamma = 0.7
line_height = 1.4
font_size_scale = 1.1
adjust_baseline = 3
adjust_underline_position = 2
adjust_underline_thickness = 1
underline_skip_descenders = false
minimum_contrast = 7.0
selection_foreground = "#eeeeee"
selection_inactive = true
cursor_break_ligatures = true
background_opacity = 0.8
background_material = "hud"
window_padding = 31.0
window_padding_top = 7.0
copy_on_select = true
"##,
        )
        .expect("candidate config");
        app.config = candidate.clone();
        app.default_font_size = 23.0;
        app.font_size = 23.0;
        app.font_px = 23.0;
        app.theme = candidate.theme_for(app.os_appearance);
        app.font_family = Some("Menlo".to_string());
        app.text_shaping = candidate.text_shaping();
        app.text_blending = candidate.text_blending_or_default();
        app.font_thicken = candidate.font_thicken_or_default();
        app.stem_gamma = candidate.stem_gamma_or_default();
        let (variations, _) = candidate.font_variation_requests();
        app.font_variations = variations;
        app.font_weight_dark_nudge = candidate.font_weight_dark_nudge_or_default();
        app.render_knobs = RenderKnobs::from_config(
            &candidate,
            &candidate.resolved_colors(app.os_appearance, &app.config_assets.themes, None),
        );
        app.font_config = FontConfig::from_config(&candidate).0;
        app.typography = crate::resolved_typography::ResolvedTypography {
            id: "Dracula".to_string(),
            applied: aterm_types::scheme::parse_theme_str(
                "fg = #ffffff\nbg = #000000\nline_height = 1.5\n",
            )
            .expect("a typography theme")
            .typography,
            ..crate::resolved_typography::ResolvedTypography::none(app.os_appearance)
        };
        app.font_size_scale_milli = candidate.font_size_scale_milli_or_default();
        assert_ne!(
            app.typography, old_typography,
            "candidate typography is live"
        );
        app.refresh_all_window_metrics();
        assert_ne!(app.windows[&wid].metrics, old_metrics, "candidate is live");

        assert!(
            !app.finish_reload_render_transaction_with(previous, |candidate_app| {
                assert_eq!(candidate_app.font_px, 23.0, "failure sees candidate");
                assert_eq!(candidate_app.render_knobs.line_height, 1.4);
                false
            })
        );

        let mut expected = old_config.clone();
        expected.copy_on_select = Some(true);
        assert!(
            app.config == expected,
            "config authority rolls back the render slice only"
        );
        assert_eq!(app.default_font_size, old_default_font_size);
        assert_eq!(app.font_size, old_font_size);
        assert_eq!(app.font_px, old_font_px);
        assert_eq!(app.theme, old_theme);
        assert_eq!(app.font_family, old_family);
        assert_eq!(app.text_shaping, old_shaping);
        assert_eq!(app.text_blending, old_blending);
        assert_eq!(app.font_thicken, old_thicken);
        assert_eq!(app.stem_gamma, old_stem_gamma);
        assert_eq!(app.font_variations, old_variations);
        assert_eq!(app.font_weight_dark_nudge, old_dark_nudge);
        assert_eq!(app.render_knobs, old_knobs);
        assert_eq!(app.font_config, old_font_config);
        assert_eq!(app.typography, old_typography);
        assert_eq!(app.font_size_scale_milli, old_size_scale);
        assert_eq!(app.windows[&wid].metrics, old_metrics);
    }

    #[test]
    fn a_same_cell_box_face_rebuild_repaints_the_cpu_window() {
        use aterm_render::DamageOutcome;

        fn present_cpu(
            app: &mut crate::App,
            input: &aterm_render::RenderInput,
        ) -> (Vec<u32>, DamageOutcome) {
            let crate::App {
                backend, windows, ..
            } = app;
            let crate::Backend::Cpu(renderer) = backend.ready_mut() else {
                panic!("headless test backend is CPU")
            };
            let ws = windows
                .get_mut(&crate::WindowId(0))
                .expect("headless fixture window");
            let _ = renderer.render_input_cached(&mut ws.cpu_cache, input);
            (
                ws.cpu_cache.presented_pixels().to_vec(),
                ws.cpu_cache.last_damage(),
            )
        }

        let mut app = crate::App::headless_for_test();
        let mut term = aterm_core::terminal::Terminal::new(3, 8);
        term.process(b"\x1b[?25lABC");
        let input = term.cell_frame(3, 8);
        let _ = present_cpu(&mut app, &input);
        assert_eq!(
            present_cpu(&mut app, &input).1,
            DamageOutcome::GateHit,
            "negative control: an identical frame really is cached"
        );
        let cell = app.backend.cell_size();

        let mut prepared = aterm_render::Renderer::from_system(app.font_px, app.theme)
            .expect("system font for the prepared generation");
        let _ = prepared.set_font_variations(&[(u32::from_be_bytes(*b"wght"), 800.0)], 0.0);
        let _ = prepared.seal_admitted_font_sources();
        let chrome = crate::tray_raster::prepare_chrome_fonts(None, None, None);
        assert!(app.rebuild_backend_with_prepared(Some((prepared, chrome))));
        assert_eq!(
            app.backend.cell_size(),
            cell,
            "the case needs an unchanged cell box"
        );

        let (pixels, damage) = present_cpu(&mut app, &input);
        assert_eq!(damage, DamageOutcome::Full, "the face rebuild repaints");
        let crate::Backend::Cpu(renderer) = app.backend.ready_mut() else {
            panic!("headless test backend is CPU")
        };
        assert!(
            pixels == renderer.render_input(&input).pixels,
            "the window shows what the new face renders"
        );
    }
}

#[cfg(test)]
mod config_policy_tests {

    use super::Config;

    fn cfg(source: &str) -> Config {
        aterm_toml::from_str(source).expect("valid config")
    }

    #[test]
    fn a_config_that_still_sets_smart_title_keys_loads_with_every_other_setting() {
        const RETIRED: &[(&str, &str)] = &[
            ("descriptive_titles", "true"),
            ("title_summary_provider", "\"ollama\""),
            ("title_summary_model", "\"qwen3.5:4b-q4_K_M\""),
            (
                "title_summary_endpoint",
                "\"http://127.0.0.1:11434/api/chat\"",
            ),
            ("title_summary_token_file", "\"/tmp/aterm-summary-token\""),
            ("title_summary_timeout_seconds", "42"),
            ("title_summary_proxy_mode", "\"direct\""),
            ("title_summary_ca_file", "\"/tmp/private-model-ca.pem\""),
            ("title_summary_interval_seconds", "15"),
            ("title_summary_context_lines", "24"),
            ("title_summary_include_output", "false"),
            ("title_summary_allow_remote", "true"),
            ("tab_title_format", "\"description\""),
            ("window_title_format", "\"description-title\""),
        ];
        let others = concat!(
            "theme = \"Nord\"\n",
            "font_px = 17.0\n",
            "scrollback_lines = 4321\n",
            "tab_status_badge = false\n",
            "[harness]\n",
            "limit_reset = true\n",
        );
        let retired: String = RETIRED
            .iter()
            .map(|(key, value)| format!("{key} = {value}\n"))
            .collect();
        let source = format!("{retired}{others}");

        let loaded = Config::parse(&source).expect("retired keys never fail the file");
        let baseline = Config::parse(others).expect("the other settings parse alone");
        assert!(
            loaded == baseline,
            "every other setting loads exactly as it would without the retired keys"
        );
        assert!(
            loaded != Config::stand_in(&source),
            "the file is not replaced by the stand-in defaults"
        );
        assert_eq!(loaded.theme.as_deref(), Some("Nord"));
        assert_eq!(loaded.font_px, Some(17.0));
        assert_eq!(loaded.scrollback_lines, Some(4321));
        assert_eq!(loaded.tab_status_badge, Some(false));

        let warnings = crate::native_config_language::key_warnings(&source);
        assert!(warnings.ignored.is_empty(), "{:?}", warnings.ignored);
        assert_eq!(
            warnings.retired.len(),
            RETIRED.len(),
            "{:?}",
            warnings.retired
        );
        for ((line, (key, _)), number) in warnings.retired.iter().zip(RETIRED).zip(1..) {
            assert_eq!(
                line,
                &format!(
                    "line {number}: {key} does nothing: Smart Titles was removed; safe to delete"
                )
            );
        }
        assert!(
            super::unaccepted_value_notices(&source, &[]).is_empty(),
            "a retired key's value is never reported as a value its reader rejects"
        );
    }

    #[test]
    fn a_config_that_still_sets_the_band_or_the_chime_loads_and_is_told_once() {
        let others = "theme = \"Nord\"\n[presence]\nrim = false\n";
        let source =
            "choice_sound = false\ntheme = \"Nord\"\n[presence]\nband = false\nrim = false\n";
        let loaded = Config::parse(source).expect("retired keys never fail the file");
        let baseline = Config::parse(others).expect("the other settings parse alone");
        assert!(
            loaded == baseline,
            "every other setting loads as without them"
        );
        assert!(
            !loaded.presence_rim_enabled(),
            "the needs switch beside them stands"
        );
        let warnings = crate::native_config_language::key_warnings(source);
        assert!(warnings.ignored.is_empty(), "{:?}", warnings.ignored);
        assert_eq!(warnings.retired.len(), 2, "{:?}", warnings.retired);
        assert!(
            super::unaccepted_value_notices(source, &[]).is_empty(),
            "a retired key's value is never a value its reader rejects"
        );
    }

    /// 0.135.0 shipped `[sys] stats_deck_style` (hidden, for the rejected merged deck): a file
    /// that still sets it loads as without it and is told once that the key does nothing.
    #[test]
    fn a_config_that_still_sets_the_deck_style_loads_and_is_told_once() {
        let others = "[sys]\nstats_bars = \"deck\"\nstats_deck_size = \"full\"\n";
        let source = "[sys]\nstats_bars = \"deck\"\nstats_deck_style = \"merged\"\nstats_deck_size = \"full\"\n";
        let loaded = Config::parse(source).expect("a retired key never fails the file");
        let baseline = Config::parse(others).expect("the other settings parse alone");
        assert!(
            loaded == baseline,
            "every other setting loads as without it"
        );
        assert_eq!(
            loaded.stats_deck_size_or_default(),
            crate::stats_plan::DeckSizePref::Full,
            "the size setting beside it stands"
        );
        let warnings = crate::native_config_language::key_warnings(source);
        assert!(warnings.ignored.is_empty(), "{:?}", warnings.ignored);
        assert_eq!(
            warnings.retired,
            [
                "line 3: sys.stats_deck_style does nothing: Merged top deck was removed; safe to delete"
            ],
        );
        assert!(
            super::unaccepted_value_notices(source, &[]).is_empty(),
            "a retired key's value is never a value its reader rejects"
        );
    }

    #[test]
    fn tab_status_policy_clamps_both_ends() {
        assert_eq!(
            cfg("tab_status_quiet_after_ms = 0").tab_status_quiet_after_ms_or_default(),
            500
        );
        assert_eq!(
            cfg("tab_status_quiet_after_ms = 999999").tab_status_quiet_after_ms_or_default(),
            120_000
        );
        assert_eq!(
            cfg("tab_status_dwell_ms = 0").tab_status_dwell_ms_or_default(),
            0
        );
        assert_eq!(
            cfg("tab_status_dwell_ms = 999999").tab_status_dwell_ms_or_default(),
            10_000
        );
    }

    #[test]
    fn the_observation_interval_tracks_dwell_between_a_floor_and_the_budget() {
        let ms = |toml: &str| cfg(toml).tab_status_observe_interval().as_millis();
        assert_eq!(
            Config::default().tab_status_observe_interval().as_millis(),
            250
        );
        assert_eq!(ms("tab_status_dwell_ms = 0"), 50, "floored, never zero");
        assert_eq!(ms("tab_status_dwell_ms = 100"), 100, "tracks a short dwell");
        assert_eq!(
            ms("tab_status_dwell_ms = 9000"),
            250,
            "capped at the budget"
        );
        for dwell in [0_u64, 1, 100, 750, 5_000, 10_000, 999_999] {
            let config = cfg(&format!("tab_status_dwell_ms = {dwell}"));
            let interval = config.tab_status_observe_interval().as_millis();
            let resolved = u128::from(config.tab_status_dwell_ms_or_default());
            assert!(
                interval <= resolved.max(50),
                "interval {interval} outruns dwell {resolved}"
            );
        }
    }
}

#[cfg(test)]
mod cfg_engine_tests {
    use super::{
        Config, KittySpriteAsset, MAX_KITTY_SPRITE_FILE_BYTES, MAX_USER_THEME_FILE_BYTES,
        ThemeCatalog, ThemeCatalogLimits, ThemeEntryStatus, ThemeScanCaps,
    };
    #[cfg(unix)]
    use super::{ThemeCatalogWatchError, read_bounded_theme_file};
    use aterm_core::config::BiDiMode;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn shell_override_resolver_keeps_launch_precedence_for_new_sessions() {
        assert_eq!(
            super::resolve_shell_override_with(None, Some("/bin/zsh")),
            Some("/bin/zsh".to_string())
        );
        assert_eq!(
            super::resolve_shell_override_with(
                Some("/opt/homebrew/bin/fish".to_string()),
                Some("/bin/zsh")
            ),
            Some("/opt/homebrew/bin/fish".to_string())
        );
        assert_eq!(
            super::resolve_shell_override_with(Some(String::new()), Some("/bin/zsh")),
            Some("/bin/zsh".to_string()),
            "an empty environment value does not mask the config"
        );
    }

    fn kitty_fixture(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "aterm-nyan-asset-{}-{}-{name}.png",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn config_with_kitty(path: &std::path::Path) -> Config {
        cfg(&format!(
            "theme = \"Nord\"\ncursor_nyan_sprite = {:?}\n",
            path.to_string_lossy()
        ))
    }

    fn theme_fixture_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "aterm-theme-catalog-{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_theme(path: &std::path::Path, foreground: &str, background: &str) {
        std::fs::write(
            path,
            format!("foreground = {foreground}\nbackground = {background}\n"),
        )
        .unwrap();
    }

    const SMALL_FILE_CAP: ThemeScanCaps = ThemeScanCaps {
        files: 4,
        ..ThemeScanCaps::PRODUCTION
    };

    const _: () = assert!(
        ThemeScanCaps::PRODUCTION.files < ThemeScanCaps::PRODUCTION.candidates
            && ThemeScanCaps::PRODUCTION.candidates <= ThemeScanCaps::PRODUCTION.entries
    );

    fn discover_capped(dir: &std::path::Path, caps: ThemeScanCaps) -> ThemeCatalog {
        ThemeCatalog::discover_in_capped(dir, false, caps).unwrap_or_default()
    }

    #[test]
    fn theme_catalog_is_sorted_bounded_safe_and_parse_validated() {
        let dir = theme_fixture_dir("bounded");
        let cap = SMALL_FILE_CAP.files;
        for index in (0..(cap + 2)).rev() {
            write_theme(
                &dir.join(format!("Theme-{index:03}.conf")),
                "#ddeeff",
                "#102030",
            );
        }
        let catalog = discover_capped(&dir, SMALL_FILE_CAP);
        let names = catalog.ready_names().collect::<Vec<_>>();
        assert_eq!(names.len(), cap);
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(names[0], "Theme-000");
        assert_eq!(names[cap - 1], format!("Theme-{:03}", cap - 1));
        assert!(catalog.truncated());
        assert_ne!(catalog.fingerprint(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_catalog_meta_shows_name_and_keys_on_the_stem() {
        use aterm_types::Appearance;
        use aterm_types::scheme::{AppearanceSource, ThemeWarning};
        let dir = theme_fixture_dir("meta");
        std::fs::write(
            dir.join("studio.conf"),
            "name = Studio Night\ndescription = deep navy\nauthor = Jo\n\
             foreground = #c0caf5\nbackground = #1a1b26\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("paper.conf"),
            "foreground = #202020\nbackground = #fafafa\ncolor7 = #fafafa\n",
        )
        .unwrap();
        let catalog = ThemeCatalog::discover_in(&dir);
        assert_eq!(
            catalog.ready_names().collect::<Vec<_>>(),
            ["paper", "studio"]
        );

        let studio = catalog.meta("STUDIO").expect("the stem is the id");
        assert_eq!(studio.display_name, "Studio Night");
        assert_eq!(studio.description.as_deref(), Some("deep navy"));
        assert_eq!(studio.author.as_deref(), Some("Jo"));
        assert_eq!(studio.appearance, Appearance::Dark);
        assert_eq!(studio.appearance_source, AppearanceSource::Measured);
        assert!(studio.warnings.is_empty(), "{:?}", studio.warnings);
        assert!(catalog.resolve("studio").is_ok());
        assert!(catalog.resolve("Studio Night").is_err());
        assert_eq!(catalog.meta("Studio Night"), None);

        let paper = catalog.meta("paper").expect("paper loaded");
        assert_eq!(paper.display_name, "paper");
        assert_eq!(paper.appearance, Appearance::Light);
        assert_eq!(
            catalog.resolve("paper").unwrap().appearance,
            Appearance::Light
        );
        assert_eq!(
            paper.warnings,
            vec![ThemeWarning::AnsiMatchesBackground { slot: 7 }]
        );
        assert_eq!(
            catalog
                .ready_themes()
                .map(|(id, meta)| (id, meta.display_name.as_str()))
                .collect::<Vec<_>>(),
            [("paper", "paper"), ("studio", "Studio Night")]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_catalog_builtin_meta_matches_the_classifier() {
        use super::measured_appearance;
        let catalog = ThemeCatalog::empty();
        for theme in aterm_types::scheme::builtin_theme_list() {
            let scheme = aterm_types::scheme::builtin(theme.name).expect("listed");
            assert_eq!(
                measured_appearance(scheme.background),
                theme.appearance,
                "{}",
                theme.name
            );
            let meta = catalog.meta(theme.name).expect("a built-in has meta");
            assert_eq!(meta.display_name, theme.name);
            assert_eq!(meta.pair.as_deref(), theme.pair);
            assert_eq!(meta.description.as_deref(), Some(theme.description));
        }
        assert_eq!(catalog.meta("no such theme"), None);
    }

    #[test]
    fn theme_catalog_lists_a_file_shadowing_a_builtin() {
        for (file, row, builtin) in [
            ("Nord", "Nord", "Nord"),
            ("nord", "nord", "Nord"),
            ("default", "default", "Default"),
        ] {
            let dir = theme_fixture_dir("shadow");
            write_theme(&dir.join(format!("{file}.conf")), "#010203", "#fefdfc");
            write_theme(&dir.join("Work.conf"), "#ddeeff", "#102030");
            let catalog = ThemeCatalog::discover_in(&dir);
            let listing = catalog.listing().collect::<Vec<_>>();
            assert!(
                listing.contains(&(row, ThemeEntryStatus::Shadowed(builtin))),
                "{file}: {listing:?}"
            );
            assert!(listing.contains(&("Work", ThemeEntryStatus::Ready)));
            assert_eq!(catalog.ready_names().collect::<Vec<_>>(), ["Work"]);
            assert_eq!(
                catalog.resolve(file).unwrap(),
                aterm_types::scheme::builtin(builtin).unwrap(),
                "the built-in still wins for {file}"
            );
            assert!(
                ThemeEntryStatus::Shadowed(builtin)
                    .reason()
                    .is_some_and(|reason| reason.contains(builtin))
            );
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn theme_catalog_keeps_a_row_per_case_variant_shadowing_a_builtin() {
        let dir = theme_fixture_dir("shadow-twins");
        write_theme(&dir.join("Nord.conf"), "#010203", "#fefdfc");
        write_theme(&dir.join("nord.conf"), "#030201", "#fcfdfe");
        let files = std::fs::read_dir(&dir).unwrap().count();
        let catalog = ThemeCatalog::discover_in(&dir);
        let listing = catalog.listing().collect::<Vec<_>>();
        assert_eq!(listing.len(), files, "{listing:?}");
        assert!(
            listing
                .iter()
                .all(|(_, status)| *status == ThemeEntryStatus::Shadowed("Nord")),
            "{listing:?}"
        );
        assert_eq!(
            catalog.resolve("Nord").unwrap(),
            aterm_types::scheme::builtin("Nord").unwrap()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_catalog_caps_files_after_the_name_filters() {
        let dir = theme_fixture_dir("cap-after-filter");
        let cap = SMALL_FILE_CAP.files;
        write_theme(&dir.join("Nord.conf"), "#010203", "#fefdfc");
        write_theme(&dir.join("Dracula.conf"), "#010203", "#fefdfc");
        for index in 0..cap {
            write_theme(
                &dir.join(format!("Theme-{index:03}.conf")),
                "#ddeeff",
                "#102030",
            );
        }
        let catalog = discover_capped(&dir, SMALL_FILE_CAP);
        assert_eq!(catalog.ready_names().count(), cap);
        assert!(!catalog.truncated());
        assert_eq!(
            catalog.limits(),
            ThemeCatalogLimits {
                seen: cap + 2,
                kept: cap,
            }
        );
        assert_eq!(
            catalog
                .listing()
                .filter(|(_, status)| matches!(status, ThemeEntryStatus::Shadowed(_)))
                .count(),
            2
        );

        write_theme(&dir.join("Theme-999.conf"), "#ddeeff", "#102030");
        let over = discover_capped(&dir, SMALL_FILE_CAP);
        assert_eq!(over.ready_names().count(), cap);
        assert!(over.truncated());
        assert!(over.resolve("Theme-999").is_err());
        assert_eq!(over.limits().kept, cap);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_catalog_past_the_entry_cap_keeps_a_prefix() {
        let caps = ThemeScanCaps {
            entries: 8,
            ..ThemeScanCaps::PRODUCTION
        };
        let dir = theme_fixture_dir("entry-cap");
        for index in 0..=caps.entries {
            std::fs::write(
                dir.join(format!("Theme-{index:04}.conf")),
                "foreground = #ddeeff\nbackground = #102030\n",
            )
            .unwrap();
        }
        let catalog = discover_capped(&dir, caps);
        assert!(catalog.truncated());
        let ready = catalog.ready_names().collect::<Vec<_>>();
        assert_eq!(ready.len(), caps.entries);
        assert!(catalog.resolve(ready[0]).is_ok());
        assert_eq!(catalog.limits().seen, caps.entries);
        let live = ThemeCatalog::discover_in_capped(&dir, true, caps)
            .expect("a truncated scan is not a failure");
        assert_eq!(live.ready_names().count(), caps.entries);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn theme_catalog_lists_a_non_utf8_file_name() {
        use std::os::unix::ffi::OsStrExt;
        let dir = theme_fixture_dir("non-utf8");
        let path = dir.join(std::ffi::OsStr::from_bytes(b"Caf\xff.conf"));
        write_theme(&path, "#ddeeff", "#102030");
        let catalog = ThemeCatalog::discover_in(&dir);
        let listing = catalog.listing().collect::<Vec<_>>();
        assert_eq!(listing, [("Caf\u{FFFD}", ThemeEntryStatus::Unreadable)]);
        assert!(catalog.ready_names().next().is_none());
        assert!(catalog.resolve("Caf\u{FFFD}").is_err());
        assert_eq!(catalog.limits().kept, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_catalog_hides_unsafe_malformed_oversized_and_symlink_entries() {
        let dir = theme_fixture_dir("invalid");
        write_theme(&dir.join("Work.conf"), "#123456", "#abcdef");
        write_theme(&dir.join("dark:Trap.conf"), "#ffffff", "#000000");
        write_theme(&dir.join(" Bad.conf"), "#ffffff", "#000000");
        std::fs::write(dir.join("Broken.conf"), "foreground = #ffffff\n").unwrap();
        let oversized = std::fs::File::create(dir.join("Huge.conf")).unwrap();
        oversized
            .set_len((MAX_USER_THEME_FILE_BYTES + 1) as u64)
            .unwrap();
        drop(oversized);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("Work.conf"), dir.join("Alias.conf")).unwrap();
            let fifo = dir.join("Pipe.conf");
            let fifo_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
            // SAFETY: `fifo_path` is a valid NUL-terminated path and the return value is checked before the no-follow/nonblocking open is tested.
            assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
        }

        let catalog = ThemeCatalog::discover_in(&dir);
        assert_eq!(catalog.ready_names().collect::<Vec<_>>(), ["Work"]);
        for invalid in ["dark:Trap", " Bad", "Broken", "Huge"] {
            assert!(
                catalog.resolve(invalid).is_err(),
                "{invalid} must fail closed"
            );
        }
        #[cfg(unix)]
        {
            assert!(catalog.resolve("Alias").is_err());
            let alias = catalog.listing().find(|(name, _)| *name == "Alias");
            assert!(
                matches!(alias, Some((_, ThemeEntryStatus::Invalid(why))) if why.contains("symlink")),
                "{alias:?}"
            );
            assert!(
                ThemeCatalog::try_discover_in(&dir).is_ok(),
                "a linked theme never blocks a live reload"
            );
            assert!(
                read_bounded_theme_file(&dir.join("Alias.conf")).is_err(),
                "the handle-level open must reject a symlink even if it is swapped in after scanning"
            );
            assert!(
                read_bounded_theme_file(&dir.join("Pipe.conf")).is_err(),
                "the nonblocking handle-level open must reject a FIFO without waiting for a writer"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn live_theme_file_swap_retains_valid_catalog_until_recovery() {
        use std::os::unix::fs::symlink;

        let dir = theme_fixture_dir("live-file-swap");
        let work = dir.join("Work.conf");
        write_theme(&work, "#123456", "#abcdef");
        let active = ThemeCatalog::try_discover_in(&dir).expect("initial valid catalog");
        assert!(active.resolve("Work").is_ok());

        let rejected = ThemeCatalog::try_discover_in_after_scan(&dir, || {
            std::fs::remove_file(&work).unwrap();
            symlink(dir.join("missing.conf"), &work).unwrap();
        });
        assert_eq!(rejected, Err(ThemeCatalogWatchError::FileUnavailable));
        assert!(
            active.resolve("Work").is_ok(),
            "a rejected live generation leaves the prior active theme intact"
        );

        std::fs::remove_file(&work).unwrap();
        write_theme(&work, "#fedcba", "#101820");
        let recovered = ThemeCatalog::try_discover_in(&dir).expect("recovered catalog");
        assert!(recovered.resolve("Work").is_ok());
        assert_ne!(recovered, active);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_catalog_has_one_case_insensitive_identity_and_rejects_collisions() {
        let dir = theme_fixture_dir("case-identity");
        write_theme(&dir.join("Work.conf"), "#123456", "#abcdef");
        let catalog = ThemeCatalog::discover_in(&dir);
        assert_eq!(catalog.ready_names().collect::<Vec<_>>(), ["Work"]);
        assert_eq!(catalog.resolve("work"), catalog.resolve("WORK"));

        let collided = ThemeCatalog::from_schemes([
            (
                "Work".to_string(),
                aterm_types::scheme::builtin("Dracula").unwrap(),
            ),
            (
                "work".to_string(),
                aterm_types::scheme::builtin("GitHub Light").unwrap(),
            ),
        ]);
        assert!(collided.ready_names().next().is_none());
        let upper = collided.resolve("Work").unwrap_err();
        let lower = collided.resolve("work").unwrap_err();
        assert_eq!(upper, lower);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn one_theme_catalog_drives_identical_preview_renderer_and_engine_colors() {
        let dir = theme_fixture_dir("projection");
        write_theme(&dir.join("Work.conf"), "#123456", "#abcdef");
        let catalog = ThemeCatalog::discover_in(&dir);
        let config = cfg("theme = \"work\"");
        let scheme = catalog.resolve("Work").unwrap();
        assert_eq!(catalog.resolve("work").unwrap(), scheme);
        let renderer = config.theme_for_with_assets(aterm_types::Appearance::Dark, &catalog);
        let engine =
            config.applied_terminal_config_for_with_assets(aterm_types::Appearance::Dark, &catalog);
        assert_eq!(renderer.fg, scheme.to_theme_parts().fg);
        assert_eq!(renderer.bg, scheme.to_theme_parts().bg);
        assert_eq!(engine.default_foreground, scheme.foreground);
        assert_eq!(engine.default_background, scheme.background);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn kitty_asset_resolves_once_to_shared_rgba_with_stable_identity() {
        let path = kitty_fixture("ready");
        let rgba = [
            0xff, 0x10, 0x20, 0xff, 0x20, 0xff, 0x30, 0x80, 0x10, 0x20, 0xff, 0x40, 0xff, 0xff,
            0xff, 0xff,
        ];
        let png = crate::app_introspect::encode_rgba8_png(&rgba, 2, 2).unwrap();
        std::fs::write(&path, png).unwrap();
        let config = config_with_kitty(&path);
        let first = config.resolve_asset_catalog();
        let second = config.resolve_asset_catalog();
        let (
            KittySpriteAsset::Ready {
                source_id,
                w,
                h,
                rgba: resolved,
                fp,
            },
            KittySpriteAsset::Ready { fp: second_fp, .. },
        ) = (&first.kitty_sprite, &second.kitty_sprite)
        else {
            panic!("valid PNG must resolve Ready");
        };
        assert_eq!(source_id.as_ref(), path.to_string_lossy());
        assert_eq!((*w, *h), (2, 2));
        assert_eq!(resolved.as_ref(), rgba);
        assert_ne!(*fp, 0);
        assert_eq!(fp, second_fp, "same source bytes have stable identity");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn kitty_asset_hostile_sources_fail_closed() {
        let invalid = |path: &std::path::Path, case: &str| {
            assert!(
                matches!(
                    config_with_kitty(path).resolve_asset_catalog().kitty_sprite,
                    KittySpriteAsset::Invalid { .. }
                ),
                "{case}"
            );
        };
        invalid(&kitty_fixture("missing"), "missing");

        let oversized = kitty_fixture("encoded-limit");
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len((MAX_KITTY_SPRITE_FILE_BYTES as u64) + 1)
            .unwrap();
        invalid(&oversized, "encoded bytes past the cap");

        let wide = kitty_fixture("dimension-limit");
        let width = super::MAX_KITTY_SPRITE_DIMENSION + 1;
        let png = crate::app_introspect::encode_rgba8_png(&vec![0x7f; width * 4], width as u32, 1)
            .unwrap();
        std::fs::write(&wide, png).unwrap();
        invalid(&wide, "dimension past the cap");
        let fixtures = vec![oversized, wide];

        #[cfg(unix)]
        let fixtures = {
            use std::os::unix::ffi::OsStrExt as _;
            let fifo = kitty_fixture("writerless-fifo");
            let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
            // SAFETY: `fifo_c` is a live NUL-terminated pathname and mkfifo retains no pointer. `kitty_fixture` gives this test a unique final component.
            assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
            invalid(&fifo, "writerless FIFO");
            let target = kitty_fixture("symlink-target");
            let linked = kitty_fixture("symlink-final");
            std::fs::write(&target, b"not consulted").expect("write symlink target");
            std::os::unix::fs::symlink(&target, &linked).expect("create final symlink");
            invalid(&linked, "final-component symlink");
            let mut fixtures = fixtures;
            fixtures.extend([fifo, linked, target]);
            fixtures
        };
        for path in fixtures {
            let _ = std::fs::remove_file(path);
        }
    }

    fn write_toy_pack(label: &str, words: &[&str], burst: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aterm-toy-pack-test-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create toy-pack fixture directory");
        let path = dir.join("pack.toml");
        let words = words
            .iter()
            .map(|word| super::toml_basic_string(word))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!(
            "schema = 1\n\
             [pack]\n\
             id = \"community.test.{label}\"\n\
             name = \"{label}\"\n\
             version = 1\n\
             authors = [\"aterm tests\"]\n\
             license = \"Apache-2.0\"\n\
             [[toy]]\n\
             id = \"recipe\"\n\
             lang = \"en\"\n\
             words = [{words}]\n\
             burst = {{ kind = \"{burst}\" }}\n"
        );
        std::fs::write(&path, source).expect("write toy-pack fixture");
        path
    }

    fn config_with_toy_packs(paths: &[std::path::PathBuf], tail: &str) -> Config {
        let paths = paths
            .iter()
            .map(|path| super::toml_basic_string(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(", ");
        cfg(&format!("[sparkle_words]\ntoy_packs = [{paths}]\n{tail}"))
    }

    #[test]
    fn w6_font_config_warns_and_skips_unresolvable() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-render/assets/DejaVuSansMono.ttf"
        );
        let toml = format!(
            "font_family_bold = \"definitely-not-a-real-font-xyzzy\"\n\
             font_synthetic_style = false\n\
             fallback_fonts = [{}, \"also-not-a-real-font-xyzzy\"]\n",
            super::toml_basic_string(fixture)
        );
        let (fc, warns) = super::FontConfig::from_config(&cfg(&toml));
        assert_eq!(fc.styled_paths[0], None, "bogus bold family skipped");
        assert!(!fc.synthetic_style);
        assert_eq!(fc.fallback_fonts, vec![fixture.to_string()]);
        assert_eq!(
            warns.len(),
            2,
            "one warning per unresolvable entry: {warns:?}"
        );
        assert!(warns[0].contains("font_family_bold"));
        assert!(warns[1].contains("fallback_fonts"));
    }

    #[test]
    fn w6_rejected_oversized_face_preserves_last_working_font() {
        let root = std::env::temp_dir().join(format!(
            "aterm-font-config-oversized-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("oversized.ttf");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(aterm_render::font_file::MAX_FONT_FILE_BYTES as u64 + 1)
            .unwrap();
        let config = cfg(&format!(
            "font_family_bold = {}\n",
            super::toml_basic_string(&path.to_string_lossy())
        ));
        let (mut next, warnings) = super::FontConfig::from_config(&config);
        let mut working = super::FontConfig::default();
        working.styled_paths[0] = Some(aterm_render::FaceRef::new("/admitted/working-bold.ttf", 0));
        next.preserve_rejected_from(&config, &working);

        assert_eq!(next.styled_paths[0], working.styled_paths[0]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("font_family_bold"), "{warnings:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn w6_writerless_fifo_is_diagnosed_without_entering_live_font_config() {
        use std::os::unix::ffi::OsStrExt as _;

        let root =
            std::env::temp_dir().join(format!("aterm-font-config-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("face.fifo");
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the CString is live for the call and the private fixture path does not alias another test's FIFO.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let config = cfg(&format!(
            "font_family_italic = {}\n",
            super::toml_basic_string(&path.to_string_lossy())
        ));
        let (resolved, warnings) = super::FontConfig::from_config(&config);
        assert!(resolved.styled_paths[1].is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("font_family_italic"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_ignored_font_px_is_a_load_notice() {
        let with = |font_px: Option<f32>| Config {
            font_px,
            ..Config::default()
        };
        let validate_flags = |config: &Config| {
            crate::diagnostics::config_semantic_warnings(config)
                .iter()
                .any(|w| w.key == "font_px")
        };
        for fine in [
            None,
            Some(14.0),
            Some(crate::FONT_PX_MIN),
            Some(crate::FONT_PX_MAX),
        ] {
            assert_eq!(super::font_px_load_notice(&with(fine)), None, "{fine:?}");
            assert!(!validate_flags(&with(fine)), "{fine:?}");
        }
        for bad in [0.0, 500.0, -5.0, f32::NAN, f32::INFINITY] {
            assert!(
                super::font_px_load_notice(&with(Some(bad))).is_some(),
                "{bad}"
            );
            assert!(validate_flags(&with(Some(bad))), "{bad}");
            assert_eq!(
                super::resolve_font_px_with(None, Some(bad)),
                crate::FONT_PX,
                "{bad}: the banner's claim is the resolver's behaviour"
            );
        }
    }

    #[test]
    fn restart_notices_flags_columns_and_lines_but_nothing_else() {
        use super::restart_notices;
        let a = cfg("columns = 100\nlines = 40");
        assert!(restart_notices(&a, &a).is_empty());
        let hot = cfg("columns = 100\nlines = 40\nfont_px = 18");
        assert!(restart_notices(&a, &hot).is_empty());
        let cols = cfg("columns = 120\nlines = 40");
        assert_eq!(restart_notices(&a, &cols).len(), 1);
        let rows = cfg("columns = 100\nlines = 50");
        assert_eq!(restart_notices(&a, &rows).len(), 1);
        let both = cfg("columns = 120\nlines = 50");
        assert_eq!(restart_notices(&a, &both).len(), 1);
        let cleared = cfg("lines = 40");
        assert_eq!(restart_notices(&a, &cleared).len(), 1);
    }

    #[test]
    fn engine_keys_resolve_onto_terminal_config() {
        use aterm_core::config::TerminalConfig;
        use aterm_core::terminal::CursorStyle;
        type Check = fn(&TerminalConfig) -> bool;
        let cases: [(&str, Check); 10] = [
            ("cursor_style = \" BAR \"", |t| {
                t.cursor_style == CursorStyle::BlinkingBar
            }),
            ("cursor_style = \"Beam\"\ncursor_blink = false", |t| {
                t.cursor_style == CursorStyle::SteadyBar
            }),
            ("bidi = \"disabled\"", |t| t.bidi.mode == BiDiMode::Disabled),
            ("bidi = \" Explicit \"", |t| {
                t.bidi.mode == BiDiMode::Explicit
            }),
            ("bidi = \" ON \"", |t| t.bidi.mode == BiDiMode::Implicit),
            ("ambiguous_width = \" wide \"", |t| t.ambiguous_width_double),
            ("ambiguous_width = \" DOUBLE \"", |t| {
                t.ambiguous_width_double
            }),
            ("ambiguous_width = \" NARROW \"", |t| {
                !t.ambiguous_width_double
            }),
            ("bold_is_bright = false", |t| !t.bold_is_bright),
            ("faint_opacity = 0.25", |t| t.faint_opacity == 0.25),
        ];
        for (source, check) in cases {
            let tc = cfg(source).applied_terminal_config();
            assert!(check(&tc), "{source}");
        }
        assert_eq!(
            cfg("font_px = 14.0\nline_height = 1.2").applied_terminal_config(),
            Config::default().applied_terminal_config()
        );
    }

    #[test]
    fn font_features_parse_onto_primary_face() {
        let s = cfg("font_features = [\"ss01\", \"-calt\", \"+zero\"]").text_shaping();
        assert_eq!(s.font_features.len(), 1, "one primary-face feature set");
        let set = &s.font_features[0];
        assert_eq!(set.font_id, 0);
        let tags: Vec<(&[u8], u32)> = set.features.iter().map(|f| (&f.tag[..], f.value)).collect();
        assert_eq!(
            tags,
            vec![(&b"ss01"[..], 1), (&b"calt"[..], 0), (&b"zero"[..], 1)]
        );
        let grouped = cfg("font_features = [\"+ss01 +zero\"]").text_shaping();
        assert_eq!(grouped.font_features[0].features.len(), 2);
        let empty = cfg("font_features = [\"\", \"+\", \"-\"]").text_shaping();
        assert!(empty.font_features.is_empty(), "{:?}", empty.font_features);
    }

    #[test]
    fn security_flags_are_opt_in_and_fail_closed() {
        let flags = |source: &str| {
            let tc = cfg(source).applied_terminal_config();
            [
                tc.allow_osc52_query,
                tc.allow_window_ops,
                tc.allow_notifications,
                tc.allow_palette_reconfigure,
            ]
        };
        assert_eq!(
            flags("scrollback_lines = 5000"),
            [false, false, false, true],
            "palette sets are allowed by default"
        );
        assert_eq!(
            flags(
                "allow_osc52_query = true\nallow_window_ops = true\n\
                 allow_notifications = true\nallow_palette_reconfigure = true"
            ),
            [true; 4]
        );
    }

    #[test]
    fn palette_sets_refused_only_when_the_owner_says_false() {
        assert!(
            !cfg("allow_palette_reconfigure = false")
                .applied_terminal_config()
                .allow_palette_reconfigure
        );
        assert!(
            cfg("allow_palette_reconfigure = true")
                .applied_terminal_config()
                .allow_palette_reconfigure
        );
        assert!(!aterm_core::config::TerminalConfig::default().allow_palette_reconfigure);
    }

    #[test]
    fn sparkle_absent_table_decorates_the_alt_screen_and_master_off_disables() {
        let deco = Config::default()
            .sparkle_deco_config()
            .expect("an absent table resolves the defaults");
        assert!(!deco.suppress_in_alt_screen);
        assert!(
            cfg("[sparkle_words]\nsuppress_in_alt_screen = true")
                .sparkle_deco_config()
                .unwrap()
                .suppress_in_alt_screen
        );
        assert!(
            cfg("[sparkle_words]\nenabled = false")
                .sparkle_deco_config()
                .is_none()
        );
    }

    #[test]
    fn sparkle_style_tokens_resolve_trimmed_any_case_with_fallback() {
        use crate::word_decorations::{FelineStyle, ProfanityStyle};
        for (style, expected) in [
            (" Nova ", ProfanityStyle::Nova),
            (" SPARKLE ", ProfanityStyle::Sparkle),
            ("rainbow", ProfanityStyle::Rainbow),
            ("comet", ProfanityStyle::Rainbow),
        ] {
            let deco = cfg(&format!("[sparkle_words.profanity]\nstyle = \"{style}\""))
                .sparkle_deco_config()
                .unwrap();
            assert_eq!(deco.profanity_style, expected, "{style:?}");
        }
        for (style, expected) in [(" Paw ", FelineStyle::Paw), ("lion", FelineStyle::Cat)] {
            let deco = cfg(&format!("[sparkle_words.feline]\nstyle = \"{style}\""))
                .sparkle_deco_config()
                .unwrap();
            assert_eq!(deco.feline_style, expected, "{style:?}");
        }
        let deco = cfg("[sparkle_words.profanity]\nsupernova_chance = 250")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.supernova_chance, 100);
    }

    #[test]
    fn sparkle_custom_specs_resolve_and_survive_ink_off() {
        let toml = concat!(
            "[sparkle_words.ink]\nenabled = false\n",
            "[[sparkle_words.custom]]\nwords = [\"Ultrathink\", \"猫神\"]\n",
            "ink = { colorway = \"rainbow\" }\n",
        );
        let c = cfg(toml);
        let deco = c.sparkle_deco_config().expect("resolves");
        assert!(deco.spec_table.has_custom(), "custom specs registered");
        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("ultrathink"))
                .is_some(),
            "keyed by the FOLDED surface hash"
        );
        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("猫神"))
                .is_some(),
            "CJK surfaces keyed RAW"
        );
        assert!(
            !deco.ink_enabled && deco.emphasis,
            "emphasis scans with ink off when custom specs exist (v3 §6 gate)"
        );
        let deco = cfg("[sparkle_words.ink]\nenabled = false")
            .sparkle_deco_config()
            .unwrap();
        assert!(!deco.emphasis, "no customs: the ink AND-gate stands");
        let over = c.sparkle_override_toml().expect("custom words synthesize");
        assert!(over.contains("forms = [\"Ultrathink\"]"), "{over}");
        assert!(over.contains("cjk = true"), "{over}");
        assert!(over.contains("forms = [\"猫神\"]"), "{over}");
    }

    #[test]
    fn sparkle_words_product_toggle_suppresses_all_non_feline_effects() {
        let c = cfg(concat!(
            "[sparkle_words.profanity]\nenabled = false\n",
            "[sparkle_words.emphasis]\nenabled = true\nextra_words = [\"wow\"]\n",
            "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\n",
            "burst = { kind = \"starburst\", chance = 100 }\n",
        ));
        let deco = c
            .sparkle_deco_config()
            .expect("keyword kitties remain independently enabled");
        assert!(!deco.profanity, "built-in sparkle words are off");
        assert!(!deco.emphasis, "emphasis cannot bypass the product switch");
        assert!(
            !deco.canine,
            "every non-feline class shares the product switch"
        );
        assert!(deco.feline, "keyword kitties remain independently on");
        assert!(
            !deco.spec_table.has_custom(),
            "custom and Toy Pack effects cannot bypass the product switch"
        );

        assert!(
            cfg("[sparkle_words.profanity]\nenabled = false\n\
                 [sparkle_words.feline]\nenabled = false")
            .sparkle_deco_config()
            .is_none(),
            "turning both top-level toys off produces no decoration engine"
        );
    }

    #[test]
    fn toy_packs_load_skip_invalid_and_apply_documented_precedence() {
        use aterm_effects::spec::BurstKind;

        let first = write_toy_pack("first", &["packshared", "firsttoy"], "sparkle");
        let invalid_dir =
            std::env::temp_dir().join(format!("aterm-toy-pack-invalid-{}", std::process::id()));
        std::fs::create_dir_all(&invalid_dir).expect("create invalid fixture directory");
        let invalid = invalid_dir.join("pack.toml");
        std::fs::write(&invalid, "schema = 99\n").expect("write invalid fixture");
        let later = write_toy_pack("later", &["packshared", "latertoy"], "glow");
        let paths = [first.clone(), invalid.clone(), later.clone()];

        let imported = config_with_toy_packs(&paths, "");
        let (deco, override_toml) = imported.sparkle_runtime_parts().expect("sparkle resolves");
        assert!(
            deco.profanity && deco.feline,
            "an invalid pack disables nothing"
        );
        let shared = deco
            .spec_table
            .override_for(aterm_lexicon::form_hash("packshared"))
            .and_then(|spec| spec.burst)
            .expect("shared imported recipe");
        assert_eq!(shared.kind, BurstKind::Glow, "later valid pack wins");
        let lexicon =
            aterm_lexicon::Lexicon::with_languages_and_override(&["en"], override_toml.as_deref())
                .expect("compiled pack lexicon parses");
        assert_eq!(
            lexicon
                .scan(
                    "firsttoy packshared latertoy",
                    &aterm_lexicon::ScanOptions::default()
                )
                .len(),
            3,
            "both valid packs reach the production scanner"
        );

        let inline = config_with_toy_packs(
            &paths,
            "[[sparkle_words.custom]]\n\
             words = [\"packshared\"]\n\
             burst = { kind = \"nova\", chance = 100 }\n",
        );
        let (deco, _) = inline.sparkle_runtime_parts().expect("inline resolves");
        let shared = deco
            .spec_table
            .override_for(aterm_lexicon::form_hash("packshared"))
            .and_then(|spec| spec.burst)
            .expect("inline shared recipe");
        assert_eq!(
            shared.kind,
            BurstKind::Nova,
            "inline custom is the final, most-local override"
        );

        for path in [first, invalid, later] {
            let _ = std::fs::remove_dir_all(path.parent().expect("fixture parent"));
        }
    }

    #[test]
    fn render_recompute_uses_only_the_prepared_sparkle_generation() {
        let pack = write_toy_pack("render-no-host-io", &["preparedword"], "glow");
        let config = config_with_toy_packs(std::slice::from_ref(&pack), "");
        let prepared = config.prepare_sparkle_runtime();

        std::fs::remove_file(&pack).expect("remove admitted Toy Pack source");
        super::reset_sparkle_host_prepare_count();
        let mut app = crate::App::headless_for_test();
        app.config = config;
        app.prepared_sparkle = prepared;
        app.sparkle_dirty = true;
        super::reset_sparkle_host_prepare_count();

        app.recompute_sparkle();
        assert_eq!(
            super::sparkle_host_prepare_count(),
            0,
            "render-time recompute must not open/compile any configured feed"
        );
        assert!(
            app.sparkle.as_ref().is_some_and(|resolved| resolved
                .cfg
                .spec_table
                .override_for(aterm_lexicon::form_hash("preparedword"))
                .is_some()),
            "the immutable prepared Toy Pack remains active after its source disappears"
        );
        let _ = std::fs::remove_dir_all(pack.parent().expect("fixture parent"));
    }

    #[test]
    fn prepared_config_apply_does_not_refingerprint_path_feeds() {
        let dir =
            std::env::temp_dir().join(format!("aterm-prepared-apply-no-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "# comment-only generation\n").unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        let config = Config::default();
        let assets = config.resolve_asset_catalog();
        let feeds = config.path_feed_fingerprints();
        let sparkle = config.prepare_sparkle_runtime();
        let mut app = crate::App::headless_for_test();
        super::reset_path_feed_fingerprint_count();

        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                config,
                values: std::collections::BTreeMap::new(),
                disk_faults: std::sync::Arc::default(),
                assets,
                path_feed_fps: feeds,
                sparkle,
                fonts: None,
                warnings: Vec::new(),
                font_refusals: Vec::new(),
                reason: crate::app_colorscheme::ThemeChange::ConfigReload,
            },
        );
        assert_eq!(
            super::path_feed_fingerprint_count(),
            0,
            "the event-loop reducer must consume the worker's immutable fingerprint"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn apply_config_text(app: &mut crate::App, dir: &std::path::Path, text: &str) {
        let path = dir.join("aterm.toml");
        std::fs::write(&path, text).expect("write config");
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .expect("observe");
        let config: Config = aterm_toml::from_str(text).expect("valid toml");
        let assets = config.resolve_asset_catalog();
        let path_feed_fps = config.path_feed_fingerprints();
        let sparkle = config.prepare_sparkle_runtime();
        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                config,
                values: std::collections::BTreeMap::new(),
                disk_faults: std::sync::Arc::default(),
                assets,
                path_feed_fps,
                sparkle,
                fonts: None,
                warnings: Vec::new(),
                font_refusals: Vec::new(),
                reason: crate::app_colorscheme::ThemeChange::ConfigReload,
            },
        );
    }

    #[test]
    fn a_misspelled_key_added_by_a_live_edit_still_warns_through_the_dedupe() {
        let dir = std::env::temp_dir().join(format!("aterm-typo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut app = crate::App::headless_for_test();

        apply_config_text(&mut app, &dir, "windw_padding = 20\n");
        let ignored = app
            .messages
            .live_by_key(crate::message_reporters::ConfigFamily::IgnoredKeys.key())
            .expect("a misspelled key must warn even when the parse dedupes");
        assert!(
            ignored
                .msg
                .detail
                .iter()
                .any(|l| l.contains("windw_padding")),
            "the row names the key that was ignored: {:?}",
            ignored.msg
        );

        let mut chord = crate::message_reporters::ConfigWarnings::default();
        chord.push(
            crate::message_reporters::ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        for msg in chord.into_messages() {
            app.post_message(msg);
        }

        apply_config_text(&mut app, &dir, "# fixed\n");
        assert!(
            app.messages
                .live_by_key(crate::message_reporters::ConfigFamily::IgnoredKeys.key())
                .is_none(),
            "removing the bad key clears its row"
        );
        assert!(
            app.messages
                .live_by_key(crate::message_reporters::ConfigFamily::Keybindings.key())
                .is_some(),
            "a comment-only save leaves the chord's still-true row standing"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_theme_that_does_not_load_is_on_the_band_until_its_file_arrives() {
        const NAME: &str = "Aterm Test Theme 7F1C";
        let dir = std::env::temp_dir().join(format!("aterm-theme-miss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut app = crate::App::headless_for_test();
        apply_config_text(&mut app, &dir, &format!("theme = {NAME:?}\n"));
        let key = crate::message_reporters::ConfigFamily::UnacceptedValues.key();
        let row = app
            .messages
            .live_by_key(key)
            .expect("a theme that does not load is on the band");
        assert!(
            row.msg.detail.iter().any(|line| line.contains(NAME)),
            "{:?}",
            row.msg
        );

        app.reload_theme_catalog(ThemeCatalog::from_schemes([(
            NAME.to_string(),
            aterm_types::ColorScheme::default(),
        )]));
        assert!(
            app.messages.live_by_key(key).is_none(),
            "the theme file arriving takes the row down"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_external_lexicon_does_not_drop_valid_toy_pack_layer() {
        let pack = write_toy_pack("lexicon-isolation", &["packstillworks"], "glow");
        let external = pack
            .parent()
            .expect("fixture parent")
            .join("broken-lexicon.toml");
        std::fs::write(&external, "[[entry]\nclass = \"emphasis\"")
            .expect("write malformed external lexicon");
        let tail = format!(
            "lexicon = {}\n",
            super::toml_basic_string(&external.to_string_lossy())
        );
        let config = config_with_toy_packs(std::slice::from_ref(&pack), &tail);
        let (deco, override_toml) = config.sparkle_runtime_parts().expect("sparkle resolves");

        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("packstillworks"))
                .is_some(),
            "the compiled Toy Pack spec survives the rejected user layer"
        );
        let lexicon =
            aterm_lexicon::Lexicon::with_languages_and_override(&["en"], override_toml.as_deref())
                .expect("generated layers remain independently valid");
        assert_eq!(
            lexicon
                .scan("packstillworks", &aterm_lexicon::ScanOptions::default())
                .len(),
            1,
            "the Toy Pack trigger still reaches the production scanner"
        );

        std::fs::remove_dir_all(pack.parent().expect("fixture parent"))
            .expect("remove isolation fixtures");
    }

    #[test]
    fn external_lexicon_cap_is_exact_and_enforced_by_the_runtime_loader() {
        let path = kitty_fixture("lexicon-cap");
        let prefix = "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"boundedword\"]\n#";
        let mut exact = prefix.to_string();
        exact.push_str(
            &"x".repeat(aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES - prefix.len()),
        );
        std::fs::write(&path, &exact).expect("write exact-limit lexicon");
        let config = cfg(&format!(
            "[sparkle_words]\nlexicon = {}\n",
            super::toml_basic_string(&path.to_string_lossy())
        ));
        assert!(
            config
                .sparkle_override_toml()
                .is_some_and(|source| source.contains("boundedword")),
            "a regular lexicon exactly at the cap is admitted"
        );

        exact.push('x');
        std::fs::write(&path, exact).expect("write over-limit lexicon");
        assert!(
            config.sparkle_override_toml().is_none(),
            "the same lexicon one byte over the cap is skipped"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn toy_pack_path_count_is_bounded() {
        let active = write_toy_pack("active", &["activepackword"], "glow");
        let overflow = write_toy_pack("overflow", &["overflowpackword"], "nova");
        let mut paths = vec![active.clone(); super::MAX_ACTIVE_TOY_PACKS];
        paths.push(overflow.clone());
        let config = config_with_toy_packs(&paths, "");
        let (deco, override_toml) = config.sparkle_runtime_parts().expect("sparkle resolves");
        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("activepackword"))
                .is_some(),
            "accepted paths load"
        );
        assert!(
            deco.spec_table
                .override_for(aterm_lexicon::form_hash("overflowpackword"))
                .is_none(),
            "paths beyond the hard cap are inactive"
        );
        assert!(
            !override_toml
                .as_deref()
                .unwrap_or_default()
                .contains("overflowpackword"),
            "capped paths do not leak into the scanner document"
        );
        for path in [active, overflow] {
            let _ = std::fs::remove_dir_all(path.parent().expect("fixture parent"));
        }
    }

    #[test]
    fn sparkle_ink_round_trip_and_clamps() {
        let deco = cfg(
            "[sparkle_words.ink]\nenabled = true\nstrength = 0.4\nsweep_ms = 1000\nloop = true",
        )
        .sparkle_deco_config()
        .expect("ink table keeps the feature on");
        assert!(deco.ink_enabled);
        assert_eq!(deco.ink_strength, 0.4);
        assert_eq!(deco.ink_sweep_ms, 1000);
        assert!(deco.ink_loop);
        let deco = cfg("[sparkle_words.ink]\nstrength = 3.0\nsweep_ms = 50")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.ink_strength, 1.0, "strength clamps to 0.0..=1.0");
        assert_eq!(deco.ink_sweep_ms, 350, "sweep_ms clamps to 350..=6000");
        let deco = cfg("[sparkle_words.ink]\nsweep_ms = 99999")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.ink_sweep_ms, 6000);
        let deco = cfg("[sparkle_words.ink]\nloop = true\nsweep_ms = 400")
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(
            deco.ink_sweep_ms, 600,
            "looping shimmer floors sweep at 600"
        );
        let deco = cfg("[sparkle_words.emphasis]\nenabled = false")
            .sparkle_deco_config()
            .unwrap();
        assert!(!deco.emphasis && deco.ink_enabled);
    }

    #[test]
    fn prepared_emphasis_capability_matches_scannable_unoverridden_runtime_words() {
        let capability = |source: &str| {
            cfg(source)
                .prepare_sparkle_runtime()
                .consumer_capabilities()
                .emphasis_class_default
        };
        assert!(capability(
            "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\n"
        ));
        for source in [
            "[sparkle_words.emphasis]\nextra_words = []\n",
            "[sparkle_words.emphasis]\nextra_words = [\"   \"]\n",
            "[sparkle_words.emphasis]\nextra_words = [\"two words\"]\n",
            "[sparkle_words.emphasis]\nextra_words = [\"kitty\"]\n",
            "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\nignore_words = [\"MEGATHINK\"]\n",
            concat!(
                "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\n",
                "[[sparkle_words.custom]]\nwords = [\"megathink\"]\n",
                "graphic = { collection = \"cats\" }\n",
            ),
        ] {
            assert!(
                !capability(source),
                "no class-default emphasis match should survive: {source}"
            );
        }

        let dir =
            std::env::temp_dir().join(format!("aterm-emphasis-capability-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lexicon = dir.join("lexicon.toml");
        std::fs::write(
            &lexicon,
            concat!(
                "[[entry]]\nclass = \"emphasis\"\nlang = \"en\"\n",
                "mode = \"forms\"\nforms = [\"fromfile\"]\n",
                "[[entry]]\nclass = \"emphasis\"\nlang = \"zh\"\n",
                "mode = \"forms\"\ncjk = true\nforms = [\"超\"]\n",
            ),
        )
        .unwrap();
        let path = super::toml_basic_string(&lexicon.to_string_lossy());
        assert!(capability(&format!("[sparkle_words]\nlexicon = {path}\n")));
        assert!(
            !capability(&format!(
                "[sparkle_words]\nlexicon = {path}\n\
                 [[sparkle_words.custom]]\nwords = [\"fromfile\"]\n\
                 graphic = {{ collection = \"cats\" }}\n\
                 [sparkle_words.emphasis]\nignore_words = [\"超\"]\n"
            )),
            "custom ownership plus ignore removes every external default consumer"
        );

        std::fs::write(
            &lexicon,
            concat!(
                "[[entry]]\nclass = \"emphasis\"\nlang = \"zh\"\n",
                "mode = \"forms\"\ncjk = true\nforms = [\"超\"]\n",
            ),
        )
        .unwrap();
        assert!(!capability(&format!("[sparkle_words]\nlexicon = {path}\n")));
        assert!(capability(&format!(
            "[sparkle_words]\nlexicon = {path}\n\
             [sparkle_words.feline]\ncjk_single_char = true\n"
        )));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod w5_knob_tests {
    use super::{Config, KnobChange, RenderKnobs, ThemeCatalog};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    fn knobs_of(c: &Config) -> RenderKnobs {
        RenderKnobs::from_config(
            c,
            &c.resolved_colors(
                aterm_types::Appearance::Dark,
                &ThemeCatalog::default(),
                None,
            ),
        )
    }

    #[test]
    fn new_keys_parse_and_clamp() {
        let read = |key: &str, c: &Config| -> f64 {
            match key {
                "stem_gamma" => f64::from(c.stem_gamma_or_default()),
                "trail_sound_volume" => f64::from(c.trail_sound_volume()),
                "line_height" => f64::from(c.line_height_or_default()),
                "minimum_contrast" => f64::from(c.minimum_contrast_or_default()),
                "adjust_baseline" => f64::from(c.adjust_baseline_or_default()),
                "adjust_underline_position" => f64::from(c.adjust_underline_position_or_default()),
                "adjust_underline_thickness" => {
                    f64::from(c.adjust_underline_thickness_or_default())
                }
                "faint_opacity" => f64::from(c.faint_opacity_or_default()),
                "background_opacity" => f64::from(c.background_opacity_or_default()),
                other => panic!("no reader for {other}"),
            }
        };
        for (key, value, want) in [
            ("stem_gamma", "99.0", 3.0_f32),
            ("trail_sound_volume", "2.0", 1.0),
            ("trail_sound_volume", "nan", 0.0),
            ("line_height", "9.0", 2.0),
            ("line_height", "0.1", 0.8),
            ("line_height", "nan", super::DEFAULT_LINE_HEIGHT),
            ("minimum_contrast", "99.0", 21.0),
            ("adjust_baseline", "400", 32.0),
            ("adjust_underline_position", "400", 32.0),
            ("adjust_underline_thickness", "-400", -32.0),
            ("faint_opacity", "7.0", 1.0),
            ("background_opacity", "-3.0", 0.0),
        ] {
            assert_eq!(
                read(key, &cfg(&format!("{key} = {value}"))),
                f64::from(want),
                "{key} = {value}"
            );
        }
        assert_eq!(
            cfg("background_material = \"HUD\"").background_material_or_default(),
            super::BackgroundMaterial::Hud,
            "case-insensitive"
        );
        assert_eq!(
            cfg("background_material = \"under_window\"").background_material_or_default(),
            super::BackgroundMaterial::UnderWindow,
            "alias"
        );
        assert_eq!(
            cfg("background_material = \"bogus\"").background_material_or_default(),
            super::BackgroundMaterial::None,
            "unknown → none"
        );
        use aterm_render::TextBlending;
        for (source, expected) in [
            ("text_blending = \"linear\"", TextBlending::Linear),
            (
                "text_blending = \"Linear_Corrected\"",
                TextBlending::LinearCorrected,
            ),
            ("text_blending = \"gamma\"", TextBlending::LinearCorrected),
        ] {
            assert_eq!(cfg(source).text_blending_or_default(), expected, "{source}");
        }
        assert!(
            !Config::default().trail_sound_bed_or_default(),
            "ambient bed defaults off so typing notes carry no continuous drone"
        );
        assert!(cfg("trail_sound_bed = true").trail_sound_bed_or_default());
        assert!(!cfg("trail_sound_bed = false").trail_sound_bed_or_default());
    }

    #[test]
    fn ligature_keys_map_to_shaping_modes() {
        use aterm_types::text_shaping::LigatureMode;
        assert_eq!(
            cfg("cursor_break_ligatures = true")
                .text_shaping()
                .ligature_mode,
            LigatureMode::CursorDisabled
        );
        assert_eq!(
            cfg("cursor_break_ligatures = false")
                .text_shaping()
                .ligature_mode,
            LigatureMode::Enabled
        );
        assert_eq!(
            cfg("ligatures = false\ncursor_break_ligatures = true")
                .text_shaping()
                .ligature_mode,
            LigatureMode::Disabled,
            "ligatures=false wins"
        );
        let merged = cfg("merged_ligatures = true\nligatures = false").text_shaping();
        assert!(merged.admit_collapsed);
        assert_eq!(merged.ligature_mode, LigatureMode::Disabled);
    }

    #[test]
    fn new_keys_round_trip_through_prefs_writer() {
        use crate::prefs::{
            EDIT_ADJUST_BASELINE, EDIT_BOLD_IS_BRIGHT, EDIT_CURSOR_BREAK_LIGATURES,
            EDIT_FAINT_OPACITY, EDIT_LINE_HEIGHT, EDIT_MINIMUM_CONTRAST, EDIT_SELECTION_FOREGROUND,
            EDIT_SELECTION_INACTIVE, apply_prefs_edits,
        };
        let out = apply_prefs_edits(
            "",
            &[
                (EDIT_LINE_HEIGHT, Some("1.35".to_string())),
                (EDIT_MINIMUM_CONTRAST, Some("4.5".to_string())),
                (EDIT_SELECTION_FOREGROUND, Some("#aabbcc".to_string())),
                (EDIT_SELECTION_INACTIVE, Some("true".to_string())),
                (EDIT_CURSOR_BREAK_LIGATURES, Some("true".to_string())),
                (EDIT_BOLD_IS_BRIGHT, Some("false".to_string())),
                (EDIT_FAINT_OPACITY, Some("0.4".to_string())),
                (EDIT_ADJUST_BASELINE, Some("-2".to_string())),
            ],
        )
        .expect("writes typed values");
        let c: Config = aterm_toml::from_str(&out).expect("round-trips through serde");
        assert_eq!(c.line_height_or_default(), 1.35);
        assert_eq!(c.minimum_contrast_or_default(), 4.5);
        assert_eq!(knobs_of(&c).selection_fg, Some(0x00AA_BBCC));
        assert!(c.selection_inactive_or_default());
        assert_eq!(c.cursor_break_ligatures, Some(true));
        assert_eq!(c.bold_is_bright, Some(false));
        assert_eq!(c.faint_opacity_or_default(), 0.4);
        assert_eq!(c.adjust_baseline_or_default(), -2);
    }

    #[test]
    fn each_knob_key_diffs_to_exactly_one_change() {
        let base = RenderKnobs::default();
        assert!(base.diff(&base).is_empty(), "no change → no calls");

        let cases: [(&str, KnobChange); 9] = [
            ("line_height = 1.5", KnobChange::LineHeight(1.5)),
            ("minimum_contrast = 4.5", KnobChange::MinimumContrast(4.5)),
            (
                "selection_foreground = \"#010203\"",
                KnobChange::SelectionFg(Some(0x0001_0203)),
            ),
            (
                "selection_inactive = true",
                KnobChange::SelectionInactive(true),
            ),
            ("adjust_baseline = 2", KnobChange::AdjustBaseline(2)),
            (
                "adjust_underline_position = 2",
                KnobChange::AdjustUnderline(2, 0),
            ),
            (
                "adjust_underline_thickness = 1",
                KnobChange::AdjustUnderline(0, 1),
            ),
            (
                "underline_skip_descenders = false",
                KnobChange::UnderlineSkipDescenders(false),
            ),
            (
                "background_material = \"hud\"",
                KnobChange::BackgroundMaterial(super::BackgroundMaterial::Hud),
            ),
        ];
        for (toml, expected) in cases {
            let knobs = knobs_of(&cfg(toml));
            let changes = base.diff(&knobs);
            assert_eq!(
                changes,
                vec![expected],
                "{toml:?} must diff to exactly one renderer call"
            );
        }
        let all = knobs_of(&cfg(
            "line_height = 1.5\nminimum_contrast = 4.5\nadjust_baseline = 2",
        ));
        assert_eq!(all.diff(&base).len(), 3, "one change per reverted key");
    }

    #[test]
    fn translucency_couples_opacity_to_the_contrast_floor() {
        let base = RenderKnobs::default();
        let glass = knobs_of(&cfg("background_opacity = 0.6"));
        let changes = base.diff(&glass);
        assert!(
            changes.contains(&KnobChange::BackgroundOpacity(0.6)),
            "opacity change must fire: {changes:?}"
        );
        assert!(
            changes.contains(&KnobChange::MinimumContrast(4.5)),
            "glass must auto-engage the WCAG-AA floor: {changes:?}"
        );
        assert_eq!(changes.len(), 2, "exactly the opacity + the auto-floor");

        let glass_hi = knobs_of(&cfg("background_opacity = 0.6\nminimum_contrast = 7.0"));
        let hi_base = knobs_of(&cfg("minimum_contrast = 7.0"));
        assert_eq!(
            hi_base.diff(&glass_hi),
            vec![KnobChange::BackgroundOpacity(0.6)],
            "an already-legible floor is not disturbed by engaging glass"
        );

        assert_eq!(
            glass.diff(&base).len(),
            2,
            "revert drops opacity + auto-floor"
        );
    }
}

#[cfg(test)]
mod vibrancy_contrast_guarantee {
    use super::Config;
    use aterm_render::vibrancy::{CONTRAST_FLOOR, effective_minimum_contrast};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn effective_minimum_contrast_is_the_renderers_law() {
        let opacities: Vec<f32> = (0..=20).map(|i| i as f32 / 20.0).collect();
        let contrasts = [1.0_f32, 2.0, 3.0, 4.5, 4.6, 7.0, 21.0];
        for &o in &opacities {
            for &c in &contrasts {
                let config = cfg(&format!("background_opacity = {o}\nminimum_contrast = {c}"));
                let base = config.minimum_contrast_or_default();
                let opacity = config.background_opacity_or_default();
                assert_eq!(
                    config.effective_minimum_contrast().to_bits(),
                    effective_minimum_contrast(base, opacity).to_bits(),
                    "opacity={o} contrast={c}"
                );
            }
        }

        let default_glass = cfg("background_opacity = 0.5");
        assert_eq!(default_glass.minimum_contrast_or_default(), 1.0);
        assert_eq!(default_glass.effective_minimum_contrast(), CONTRAST_FLOOR);
    }

    #[test]
    fn an_unset_minimum_contrast_floors_theme_pairs_on_windows_only() {
        let unset = cfg("");
        assert_eq!(unset.effective_minimum_contrast(), 1.0);
        assert_eq!(
            unset.applied_terminal_config().indexed_minimum_contrast,
            if cfg!(windows) { 4.5 } else { 1.0 }
        );
        for set in ["minimum_contrast = 1.0", "minimum_contrast = 7.0"] {
            let config = cfg(set);
            assert_eq!(
                config.applied_terminal_config().indexed_minimum_contrast,
                1.0,
                "{set}"
            );
        }
    }
}

#[cfg(test)]
mod config_token_tests {
    use super::{
        Config, RightClickGesture, TabBandHeight, TabMenuChord, WindowColorspace, WindowTheme,
    };

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn enum_keys_parse_trimmed_any_case_with_aliases_and_fall_back() {
        let theme = |t: &str| cfg(t).window_theme_or_default();
        assert_eq!(theme("window_theme = \" Dark \""), WindowTheme::Dark);
        assert_eq!(theme("window_theme = \"LIGHT\""), WindowTheme::Light);
        assert_eq!(theme("window_theme = \"midnight\""), WindowTheme::Auto);
        assert_eq!(theme(""), WindowTheme::Auto);

        let band = |t: &str| cfg(t).tab_band_height_or_default();
        let standard = "tab_band_height = \"  STANDARD \"";
        assert_eq!(band(standard), TabBandHeight::Standard);
        assert_eq!(
            band("tab_band_height = \"compact\""),
            TabBandHeight::Compact
        );
        let unknown = "tab_band_height = \"huge\"";
        assert_eq!(band(unknown), TabBandHeight::PLATFORM_DEFAULT);

        let click = |t: &str| cfg(t).right_click_or_default();
        let copy_paste = "right_click = \" Copy-Paste \"";
        assert_eq!(click(copy_paste), RightClickGesture::CopyPaste);
        assert_eq!(
            click("right_click = \"menu\""),
            RightClickGesture::PLATFORM_DEFAULT
        );
        assert_eq!(click(""), RightClickGesture::PLATFORM_DEFAULT);

        let space = |t: &str| cfg(t).window_colorspace_or_default();
        let p3 = "window_colorspace = \" DisplayP3 \"";
        assert_eq!(space(p3), WindowColorspace::DisplayP3);
        assert_eq!(
            space("window_colorspace = \"P3\""),
            WindowColorspace::DisplayP3
        );
        let rec2020 = "window_colorspace = \"rec2020\"";
        assert_eq!(space(rec2020), WindowColorspace::Srgb);
        assert_eq!(space(""), WindowColorspace::Srgb);

        for (source, chord, menu_key, shift_f10) in [
            ("", TabMenuChord::MenuKey, true, false),
            (
                "tab_menu_chord = \"nope\"",
                TabMenuChord::MenuKey,
                true,
                false,
            ),
            (
                "tab_menu_chord = \" Menu-Key \"",
                TabMenuChord::MenuKey,
                true,
                false,
            ),
            ("tab_menu_chord = \"BOTH\"", TabMenuChord::On, true, true),
            ("tab_menu_chord = \"off\"", TabMenuChord::Off, false, false),
        ] {
            let got = cfg(source).tab_menu_chord_or_default();
            assert_eq!(got, chord, "{source}");
            assert_eq!(
                (got.claims_menu_key(), got.claims_shift_f10()),
                (menu_key, shift_f10),
                "{source}"
            );
        }
    }
}

#[cfg(test)]
mod decorative_effect_default_tests {
    use super::{Config, DEFAULT_DECORATIVE_EFFECTS, default_cursor_trail_style};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn the_trail_master_is_platform_split_and_its_two_neighbours_are_not() {
        let fresh = Config::default();
        assert_eq!(
            fresh.cursor_trail_drawn(),
            DEFAULT_DECORATIVE_EFFECTS,
            "a fresh Windows config stands no resident cat on live output; \
             elsewhere the delight stays on"
        );
        assert_eq!(
            fresh.cursor_momentum_glow_or_default(),
            DEFAULT_DECORATIVE_EFFECTS
        );
        assert_eq!(
            cfg("font_px = 14.0").cursor_trail_drawn(),
            DEFAULT_DECORATIVE_EFFECTS
        );
        assert!(fresh.cursor_trail_bloom_or_default());
        assert!(fresh.sparkle_words_enabled_or_default());
        assert!(cfg("cursor_trail_style = \"comet\"").cursor_trail_drawn());
        assert!(!cfg("cursor_trail_style = \"off\"").cursor_trail_drawn());
        assert!(!cfg("cursor_trail_bloom = false").cursor_trail_bloom_or_default());
        assert!(!cfg("[sparkle_words]\nenabled = false").sparkle_words_enabled_or_default());
    }

    #[test]
    fn the_retired_cursor_master_folds_into_its_successors() {
        let mut naive_disagrees = false;
        for master in [None, Some(true), Some(false)] {
            for style in [None, Some("off"), Some("fire")] {
                for glow in [None, Some(true), Some(false)] {
                    let mut toml = String::new();
                    if let Some(m) = master {
                        toml.push_str(&format!("cursor_trail = {m}\n"));
                    }
                    if let Some(s) = style {
                        toml.push_str(&format!("cursor_trail_style = \"{s}\"\n"));
                    }
                    if let Some(g) = glow {
                        toml.push_str(&format!("cursor_momentum_glow = {g}\n"));
                    }
                    let c = cfg(&toml);
                    let on = master.unwrap_or(DEFAULT_DECORATIVE_EFFECTS);
                    let old_style = style.unwrap_or(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE);
                    let old_drawn = on && old_style != "off";
                    let old_glow = on && glow.unwrap_or(true);
                    let (want_drawn, want_glow) = if master.is_some() || DEFAULT_DECORATIVE_EFFECTS
                    {
                        (old_drawn, old_glow)
                    } else {
                        (style.is_some_and(|s| s != "off"), glow.unwrap_or(false))
                    };
                    assert_eq!(c.cursor_trail_drawn(), want_drawn, "{toml}");
                    assert_eq!(c.cursor_momentum_glow_or_default(), want_glow, "{toml}");
                    let naive_drawn = style.unwrap_or(default_cursor_trail_style()).ne("off");
                    naive_disagrees |= naive_drawn != want_drawn;
                }
            }
        }
        assert!(
            naive_disagrees,
            "the matrix catches a fold that ignores the old switch"
        );
    }
}

#[cfg(test)]
mod output_streak_cfg_tests {

    use super::{Config, DEFAULT_DECORATIVE_EFFECTS};
    use aterm_effects::output_streak::{MAX_COMETS, TAIL_MAX, TAIL_MIN};

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn output_streak_keys_resolve_clamp_and_fail_toward_less_effect() {
        assert_eq!(
            cfg("[output_streak]\ntail = 12").output_streak_enabled_or_default(),
            DEFAULT_DECORATIVE_EFFECTS
        );
        assert!(!cfg("[output_streak]\nenabled = false").output_streak_enabled_or_default());

        type Read = fn(&Config) -> f64;
        let intensity: Read = |c| f64::from(c.output_streak_intensity_or_default());
        let tail: Read = |c| f64::from(c.output_streak_tail_or_default());
        let streaks: Read = |c| f64::from(c.output_streak_max_streaks_or_default());
        let idle: Read = |c| f64::from(c.output_streak_idle_secs_or_default());
        for (key, read, value, expected) in [
            ("intensity", intensity, "4.0", 1.0),
            ("intensity", intensity, "-2.0", 0.0),
            ("intensity", intensity, "nan", 0.0),
            ("tail", tail, "4000", f64::from(TAIL_MAX)),
            ("tail", tail, "0", f64::from(TAIL_MIN)),
            ("max_streaks", streaks, "99", MAX_COMETS as f64),
            ("max_streaks", streaks, "0", 1.0),
            ("idle_secs", idle, "9000.0", 120.0),
            ("idle_secs", idle, "0.0", 2.0),
            ("idle_secs", idle, "nan", 2.0),
        ] {
            assert_eq!(
                read(&cfg(&format!("[output_streak]\n{key} = {value}"))),
                expected,
                "{key} = {value}"
            );
        }
    }

    #[test]
    fn the_streak_warning_names_only_the_silently_unreachable_pip() {
        assert!(Config::default().output_streak_warning().is_none());
        assert!(
            cfg("[output_streak]\ntail = 4000\nmax_streaks = 99\nidle_secs = 9000.0")
                .output_streak_warning()
                .is_none(),
            "a documented clamp is not a warning in this repo"
        );
        assert!(
            cfg("[output_streak]\nenabled = true\nsound = true")
                .output_streak_warning()
                .is_none()
        );
        let warning = cfg("[output_streak]\nenabled = false\nsound = true")
            .output_streak_warning()
            .expect("an unreachable pip is worth saying out loud");
        assert!(
            warning.contains("output_streak.sound"),
            "the message names the key: {warning}"
        );
    }

    #[test]
    fn the_harness_words_name_every_limit_and_only_limits() {
        let read = super::HarnessPolicy::read;
        let words = |text: &str| read(text).words();
        let automatic = words("");
        for neutral in [
            "[harness]\naccounts = \"early\"\n",
            "[harness]\nanswer_text = \"ask me\"\n",
            "[harness]\nquestion_priority = \"long-term\"\n",
            "[harness]\nlimit_reset = true\n",
            "[harness]\nlimit_reset = false\n",
            "[harness]\nusage_tap = true\n",
            "[harness]\nusage_notes = true\n",
            "[harness]\nusage_notes = false\n",
            "[harness]\nusage_credits = true\n",
            "[harness]\nusage_credits = false\n",
        ] {
            assert_eq!(words(neutral), automatic, "{neutral}");
        }
        assert_eq!(
            words("[harness]\nusage_tap = false\n"),
            "Automatic, limited by 1 setting in aterm.toml",
            "the tap ships on, so turning it off is a limit"
        );
        let safe = words("[harness]\napprove = \"safe\"\n");
        assert_ne!(safe, automatic);
        for own_roots in [
            "[harness]\napprove = \"safe\"\ntrust_roots = []\n",
            "[harness]\napprove = \"safe\"\ntrust_roots = [\"~/src/app*\"]\n",
        ] {
            assert_eq!(words(own_roots), safe, "{own_roots}");
        }
        let all = "[harness]\nheadless = false\napprove = \"none\"\ntrust_roots = []\n\
                   answer_questions = false\nanswer_text = \"ask me\"\n\
                   dismiss_surveys = false\ncontinue = false\ncontinue_text = \"go\"\n\
                   continue_per_hour = 2\nrules_file = \"/r\"\nretry_api_errors = false\n\
                   probe_api = false\nresume_limits = false\nlimit_wait = false\n\
                   accounts = \"off\"\nmodel_fallback = \"\"\n\
                   compact_on_context_wall = false\nrelaunch = false\nupgrade = false\n\
                   human_grace_s = 600\n";
        for named in [
            "[harness]\napprove = \"none\"\n",
            "[harness]\nanswer_questions = false\n",
            "[harness]\naccounts = \"wall\"\n",
            all,
        ] {
            let said = words(named);
            assert_ne!(said, automatic, "{named}");
            for key in aterm_agent::supervise::config::KEYS {
                assert!(
                    !key.contains('_') || !said.contains(key),
                    "{key} is a file's word, not the row's: {said}"
                );
            }
            assert!(!said.contains(": "), "no `key: value`: {said}");
        }
        let off = words(&format!("{all}enabled = false\n"));
        assert_eq!(
            off,
            words("[harness]\nenabled = false\n"),
            "Off outranks every limit"
        );
        assert_ne!(off, automatic);
        assert!(!super::HarnessPolicy::read("").policy.limit_reset);
        assert!(!super::HarnessPolicy::read("").policy.usage_notes);
        assert!(!super::HarnessPolicy::read("").policy.usage_credits);
        assert_eq!(
            read("[harness.claude]\nenabled = false\n").words(),
            "Automatic, limited by Supervise Claude Code sessions set to Off"
        );
        assert_eq!(
            read("[harness]\nautopilot = \"land\"\n[harness.codex]\nenabled = false\n").words(),
            "Automatic, limited by Default autopilot set to Land; Watch codex-alab sessions set \
             to Off"
        );
        assert_eq!(
            read("[harness]\nautopilot = \"off\"\nrelaunch = false\n").words(),
            "Automatic, limited by Default autopilot set to Off; 1 setting in aterm.toml"
        );
        assert_eq!(
            read("[harness]\nrelaunch = false\n").words(),
            "Automatic, limited by 1 setting in aterm.toml"
        );
        assert_eq!(read("[harness]\nrelaunch = false\n").file_limits(), 1);
        assert_eq!(read("[harness]\napprove = \"none\"\n").file_limits(), 0);
    }

    #[test]
    fn every_harness_rows_limit_is_named_by_its_row() {
        let read = super::HarnessPolicy::read;
        let mut limited = Vec::new();
        for leaf in crate::prefs::NESTED_LEAVES
            .iter()
            .filter(|leaf| leaf.key.starts_with("harness."))
            .filter(|leaf| leaf.key != crate::prefs::EDIT_HARNESS_ENABLED)
        {
            let (table, key) = leaf.key.rsplit_once('.').expect("a dotted key");
            let values: Vec<String> = match leaf.kind {
                crate::prefs::EditKind::Bool => vec!["false".to_string()],
                crate::prefs::EditKind::Enum { options } => options
                    .iter()
                    .map(|option| format!("\"{option}\""))
                    .collect(),
                crate::prefs::EditKind::Text if leaf.key == crate::prefs::EDIT_HARNESS_LAND_AT => {
                    let policy = read("[harness]\nland_at = \"18:00\"\n");
                    assert!(
                        policy.policy.against_default().limits.is_empty(),
                        "land_at limits nothing: {}",
                        policy.words()
                    );
                    assert_eq!(policy.file_limits(), 0, "land_at is no file limit");
                    Vec::new()
                }
                kind => panic!("{}: a {kind:?} row", leaf.key),
            };
            for value in values {
                let source = format!("[{table}]\n{key} = {value}\n");
                let policy = read(&source);
                if policy.policy.against_default().limits.is_empty() {
                    continue;
                }
                let words = policy.words();
                assert!(words.contains(leaf.label), "{source}: {words}");
                assert!(!words.contains("aterm.toml"), "{source}: {words}");
                assert_eq!(policy.file_limits(), 0, "{source}");
                limited.push(leaf.key);
            }
        }
        for key in [
            crate::prefs::EDIT_HARNESS_CLAUDE_ENABLED,
            crate::prefs::EDIT_HARNESS_CODEX_ENABLED,
            crate::prefs::EDIT_HARNESS_AUTOPILOT,
            crate::prefs::EDIT_HARNESS_APPROVE,
            crate::prefs::EDIT_HARNESS_ANSWER_QUESTIONS,
        ] {
            assert!(limited.contains(&key), "{key} limits: {limited:?}");
        }
        assert_eq!(read("[harness]\nrelaunch = false\n").file_limits(), 1);
    }

    #[test]
    fn a_launch_that_could_not_load_the_file_still_reads_its_harness_lines() {
        let text = "font_px = \n[harness]\ncontinue = false\napprove = \"safe\"\n";
        assert!(Config::parse(text).is_err(), "the fixture is not a config");
        let stand_in = Config::stand_in(text);
        let p = &stand_in.harness.policy;
        assert!(p.enabled && !p.continue_policy);
        assert_eq!(p.approve, aterm_agent::supervise::config::Approve::Safe);
        assert!(!stand_in.harness.notes.is_empty());
        assert!(
            stand_in.font_px.is_none(),
            "every other setting at its default"
        );
        assert_eq!(
            Config::stand_in("font_px = \n").harness.policy,
            aterm_agent::supervise::SupervisorConfig::default()
        );
    }

    #[test]
    fn the_harness_table_is_the_supervisors_policy() {
        let cfg = |text: &str| Config::parse(text).expect("valid config");
        let text = "[harness]\ncontinue = false\n";
        assert!(cfg(text) != cfg("[harness]\ncontinue = true\n"));

        let bad = "[harness]\ncontinue = \"maybe\"\nheadless = false\ncontine = true\n";
        let notices = cfg(bad).harness_notices();
        assert_eq!(notices.len(), 1, "the unknown key is the config language's");
        assert!(
            notices[0].starts_with("config harness.continue:") && notices[0].contains("maybe"),
            "{notices:?}"
        );
        let ignored = crate::native_config_language::key_warnings(bad).ignored;
        assert!(ignored.iter().any(|l| l.contains("contine")), "{ignored:?}");

        let text = "[harness]\napprove = \"everything\"\n";
        let told = cfg(text).harness_notices();
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(
            super::unaccepted_value_notices(text, &told),
            Vec::<String>::new()
        );
        assert_eq!(super::unaccepted_value_notices(text, &[]).len(), 1);

        let misplaced = cfg("[packages]\nharness.enabled = false\n");
        assert!(!misplaced.harness.policy.enabled);
        assert!(
            misplaced.harness_notices()[0].contains("`packages.harness.enabled`"),
            "{:?}",
            misplaced.harness_notices()
        );
    }
}

#[cfg(test)]
mod tab_band_height_tests {
    #[cfg(windows)]
    use super::Config;
    use super::{TabBandHeight, synthetic_band_head_px};

    #[cfg(windows)]
    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[cfg(windows)]
    fn window_cell(config: &Config) -> ((usize, usize), i32, String) {
        let face = Config::font_family_admission(config.font_family_request().as_deref())
            .expect("the family is admitted");
        let mut r = aterm_render::Renderer::from_system_with_face(
            face.as_ref(),
            super::resolve_font_px(config),
            aterm_render::Theme::default(),
        )
        .expect("a face");
        r.set_line_height(config.line_height_or_default());
        (
            r.cell_size(),
            r.baseline(),
            r.primary_source_path().unwrap_or_default().to_string(),
        )
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_defaults_draw_the_judged_9x23_cell() {
        assert_eq!(crate::FONT_PX, 17.0);
        assert_eq!(super::DEFAULT_LINE_HEIGHT, 1.1);
        assert_eq!(
            window_cell(&Config::default()),
            (
                (9, 23),
                17,
                aterm_render::bundled::IOSEVKA_SEMIBOLD_ID.to_string()
            )
        );
    }

    #[cfg(windows)]
    #[test]
    fn three_lines_restore_the_cascadia_9x21_cell() {
        let family = aterm_render::bundled::CASCADIA_FAMILY;
        if aterm_render::resolve_font_family(family, aterm_render::FaceTarget::REGULAR).is_some() {
            aterm_spec::host::could_not_run(format!(
                "{family} is installed on this host, and an installed face wins the name"
            ));
        }
        let old = cfg("font_family = \"Cascadia Mono PL\"\nfont_px = 16\nline_height = 1.0\n");
        assert_eq!(
            window_cell(&old),
            ((9, 21), 15, aterm_render::bundled::CASCADIA_ID.to_string())
        );
        let lh = cfg("font_family = \"Cascadia Mono PL\"\nfont_px = 16\n");
        assert_ne!(window_cell(&lh).0, (9, 21));
        let px = cfg("font_family = \"Cascadia Mono PL\"\nline_height = 1.0\n");
        assert_ne!(window_cell(&px).0, (9, 21));
    }

    #[test]
    fn synthetic_band_head_law() {
        let target = TabBandHeight::Standard.target_logical_px();
        let band = target as usize;
        let band_150 = (f64::from(target) * 1.5).round() as usize;
        let cap = 10 * super::SYNTHETIC_BAND_HEAD_CELL_CAP;
        for (case, (t, pad_top, rows, cell_h, scale), expected) in [
            ("no strip", (target, 2, 0, 21, 1.0), 0),
            (
                "96 dpi totals the target",
                (target, 2, 1, 21, 1.0),
                band - 2 - 21,
            ),
            (
                "150% re-derives",
                (target, 3, 1, 28, 1.5),
                band_150 - 3 - 28,
            ),
            (
                "one row absorbs the target",
                (target, 2, 1, band - 2, 1.0),
                0,
            ),
            ("two rows absorb it", (target, 2, 2, 21, 1.0), 0),
            ("oversized target caps", (4000.0, 0, 1, 10, 1.0), cap),
            ("+inf target caps", (f32::INFINITY, 0, 1, 10, 1.0), cap),
            ("NaN target", (f32::NAN, 2, 1, 21, 1.0), 0),
            ("zero target", (0.0, 2, 1, 21, 1.0), 0),
            ("negative target", (-32.0, 2, 1, 21, 1.0), 0),
        ] {
            assert_eq!(
                synthetic_band_head_px(t, pad_top, rows, cell_h, scale),
                expected,
                "{case}"
            );
        }
        let at_1x = synthetic_band_head_px(32.0, 2, 1, 21, 1.0);
        assert_ne!(at_1x, 0, "control: a finite positive target has a head");
        for scale in [f64::NAN, f64::INFINITY, 0.0, -2.0] {
            assert_eq!(
                synthetic_band_head_px(32.0, 2, 1, 21, scale),
                at_1x,
                "scale {scale}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_shipped_windows_face_reaches_the_standard_band() {
        let Some(renderer) =
            aterm_render::Renderer::from_system(crate::FONT_PX, aterm_render::Theme::default())
        else {
            aterm_spec::host::could_not_run("no system monospace font here");
        };
        let target = TabBandHeight::Standard.target_logical_px();
        let pad_logical = Config::default().window_padding_top_or_default();
        let mut renderer = renderer;
        renderer.set_line_height(Config::default().line_height_or_default());
        for scale in [1.0_f64, 1.5] {
            let px = crate::font_px_for_scale(crate::FONT_PX, scale);
            renderer.set_px(px);
            let (_, cell_h) = renderer.cell_size();
            let pad_top = crate::logical_to_device_px(pad_logical, scale);
            let head = synthetic_band_head_px(target, pad_top, 1, cell_h, scale);
            let band = head + pad_top + cell_h;
            crate::logging::stderr_line!(
                "C3 band at scale {scale}: font_px={px} cell_h={cell_h} pad_top={pad_top} \
                 before={} head={head} band={band}",
                pad_top + cell_h
            );
            if head > 0 {
                assert_eq!(
                    band,
                    crate::logical_to_device_px(target, scale),
                    "the band lands ON the WinUI target at scale {scale}"
                );
            } else {
                assert!(
                    band >= crate::logical_to_device_px(target, scale),
                    "a zero head is only correct when the strip already fills the band"
                );
            }
        }
    }
}

#[cfg(test)]
mod window_padding_tests {
    use super::Config;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn window_padding_clamps_and_rejects_non_finite() {
        type Read = fn(&Config) -> f32;
        let base: Read = Config::window_padding_or_default;
        let top: Read = Config::window_padding_top_or_default;
        for (source, read, expected) in [
            ("window_padding = 20.0", base, 20.0),
            ("window_padding = -3.0", base, 0.0),
            ("window_padding = 500.0", base, super::MAX_WINDOW_PADDING_PX),
            ("window_padding = nan", base, crate::PAD_LOGICAL_PX),
            ("window_padding_top = 6.0", top, 6.0),
            ("window_padding = 4.0\nwindow_padding_top = 9.0", top, 4.0),
            ("window_padding = 1.0", top, 1.0),
            ("window_padding_top = -2.0", top, 0.0),
            ("window_padding_top = inf", top, crate::PAD_TOP_LOGICAL_PX),
        ] {
            assert_eq!(read(&cfg(source)), expected, "{source}");
        }
    }
}

#[cfg(test)]
mod headless_boot_metrics_tests {
    #[test]
    fn asymmetric_vertical_padding_round_trips_resize_with_remainder() {
        use aterm_window::dpi::PhysicalSize;

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 0;
        app.backend.set_pad(12);
        app.backend.set_pad_top(2);
        app.backend.set_head(5);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.metrics.pad = 12;
            ws.metrics.pad_top = 2;
            ws.metrics.head = 5;
        }
        let (cw, ch) = app.win_cell_size(wid);
        let (rows, cols) = (24usize, 80usize);
        let exact = PhysicalSize::new((cols * cw + 24) as u32, (rows * ch + 5 + 2 + 12) as u32);
        assert_eq!(app.grid_dims_for(wid, exact), (24, 80));

        let remainder = ch.saturating_sub(1);
        let with_remainder = PhysicalSize::new(exact.width, exact.height + remainder as u32);
        assert_eq!(app.grid_dims_for(wid, with_remainder), (24, 80));

        let old_symmetric_rows =
            aterm_render::pad_split((exact.height as usize).saturating_sub(5), 12, ch).cells;
        assert!(
            old_symmetric_rows < rows,
            "negative control: conserving 2*pad cannot invert the shorter visible frame"
        );
    }

    #[test]
    fn headless_boot_band_survives_a_delta_config_reload() {
        let mut app = crate::App::headless_for_test();
        app.backend.set_pad(12);
        app.backend.set_head(60);
        app.seed_headless_boot_metrics();
        let armed = app.backend.frame_size(24, 80);
        app.refresh_all_window_metrics();
        let ws = app.windows.get(&crate::WindowId(0)).expect("window 0");
        assert_eq!(ws.metrics.head, 60, "the record keeps the sealed band");
        assert_eq!(ws.metrics.pad, 12, "the record keeps the sealed pad");
        assert_eq!(
            ws.metrics.pad_top, 12,
            "the record keeps the sealed symmetric top origin"
        );
        app.apply_window_scale(crate::WindowId(0));
        assert_eq!(app.backend.head(), 60, "the band survives the reload");
        assert_eq!(app.backend.pad(), 12, "the boot pad survives the reload");
        assert_eq!(
            app.backend.pad_top(),
            12,
            "the top origin survives the reload"
        );
        assert_eq!(
            app.backend.frame_size(24, 80),
            armed,
            "the first present's frame still matches the armed tap geometry"
        );
    }
}

#[cfg(test)]
mod split_theme_tests {
    use super::{Config, ThemeCatalog, theme_misses, theme_selector_notice};
    use aterm_types::Appearance;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn theme_name_resolves_per_appearance() {
        for (source, dark, light) in [
            (
                r#"theme = "Tokyo Night""#,
                Some("Tokyo Night"),
                Some("Tokyo Night"),
            ),
            (
                r#"theme = "dark:Dracula,light:GitHub Light""#,
                Some("Dracula"),
                Some("GitHub Light"),
            ),
            (
                r#"theme = " DARK : Solarized Dark , Light : Solarized Light ""#,
                Some("Solarized Dark"),
                Some("Solarized Light"),
            ),
        ] {
            let c = cfg(source);
            assert_eq!(
                c.resolve_theme_name(Appearance::Dark).as_deref(),
                dark,
                "{source}"
            );
            assert_eq!(
                c.resolve_theme_name(Appearance::Light).as_deref(),
                light,
                "{source}"
            );
        }
        let split = cfg(r#"theme = "dark:Dracula,light:GitHub Light""#);
        assert_eq!(
            split.theme_for(Appearance::Dark).bg,
            cfg(r#"theme = "Dracula""#).theme_for(Appearance::Dark).bg
        );
        assert_eq!(split.theme_for(Appearance::Light).bg, 0x00FF_FFFF);
    }

    #[test]
    fn split_omitted_side_draws_the_named_side() {
        let c = cfg(r#"theme = "light:GitHub Light""#);
        for appearance in [Appearance::Dark, Appearance::Light] {
            assert_eq!(
                c.resolve_theme_name(appearance).as_deref(),
                Some("GitHub Light")
            );
        }
        assert_eq!(c.theme_for(Appearance::Dark).bg, 0x00FF_FFFF);
        assert!(theme_misses(&c, &ThemeCatalog::empty()).is_empty());
    }

    #[test]
    fn split_omitted_side_draws_the_builtin_pair() {
        let c = cfg(r#"theme = "dark:Gruvbox Dark""#);
        assert_eq!(
            c.resolve_theme_name(Appearance::Light).as_deref(),
            Some("Gruvbox Light")
        );
        assert_eq!(
            c.theme_for(Appearance::Light),
            cfg(r#"theme = "Gruvbox Light""#).theme_for(Appearance::Light)
        );
        assert_ne!(
            c.theme_for(Appearance::Light),
            c.theme_for(Appearance::Dark)
        );
        let user = cfg(r#"theme = "dark:Studio""#);
        assert_eq!(
            user.resolve_theme_name(Appearance::Light).as_deref(),
            Some("Studio")
        );
        let notice = theme_selector_notice(Some("dark:Gruvbox Dark,lite:x")).expect("an issue");
        assert!(
            notice.ends_with(r#"light mode uses "Gruvbox Light""#),
            "{notice}"
        );
    }

    #[test]
    fn theme_misses_include_selector_issues() {
        let themes = ThemeCatalog::empty();
        let misses = |theme: &str| {
            theme_misses(
                &Config {
                    theme: Some(theme.to_string()),
                    ..Default::default()
                },
                &themes,
            )
        };
        let selector = |theme: &str| theme_selector_notice(Some(theme));
        let c = cfg(r#"theme = "dark:Nord,dark:Dracula""#);
        assert_eq!(
            c.resolve_theme_name(Appearance::Dark).as_deref(),
            Some("Nord")
        );
        assert_eq!(
            misses("dark:Nord,dark:Dracula"),
            vec![selector("dark:Nord,dark:Dracula").unwrap()]
        );
        assert_eq!(misses("dark:Nord,lite:Dracula").len(), 1);
        assert_eq!(misses("dark:Nord,light:Nope,light:Dracula").len(), 2);
        for clean in ["Nord", "dark:Nord,light:GitHub Light", "dark:Nord"] {
            assert_eq!(selector(clean), None, "{clean:?}");
            assert!(misses(clean).is_empty(), "{clean:?}");
        }
        assert!(theme_selector_notice(None).is_none());
    }

    #[test]
    fn applied_terminal_config_tracks_split() {
        let c = cfg(r#"theme = "dark:Dracula,light:GitHub Light""#);
        let rgb = |packed: u32| {
            aterm_types::Rgb::new(
                ((packed >> 16) & 0xff) as u8,
                ((packed >> 8) & 0xff) as u8,
                (packed & 0xff) as u8,
            )
        };
        assert_ne!(
            c.applied_terminal_config_for(Appearance::Dark)
                .default_background,
            c.applied_terminal_config_for(Appearance::Light)
                .default_background
        );
        for side in [Appearance::Dark, Appearance::Light] {
            let engine = c.applied_terminal_config_for(side);
            let theme = c.theme_for(side);
            assert_eq!(engine.default_background, rgb(theme.bg), "{side:?}");
            assert_eq!(
                engine.cursor_color,
                Some(rgb(theme.cursor)),
                "{side:?}: the OSC 112 reset baseline"
            );
            assert_eq!(
                engine.selection_background,
                Some(rgb(theme.selection)),
                "{side:?}: the OSC 117 reset baseline"
            );
        }
    }

    #[test]
    fn applied_terminal_config_preserves_selected_text_override() {
        let c = cfg("selection_foreground = \"#123456\"");
        assert_eq!(
            c.applied_terminal_config().selection_foreground,
            Some(aterm_types::Rgb::new(0x12, 0x34, 0x56))
        );
    }
}

#[cfg(test)]
mod reload_dedupe_tests {
    use super::Config;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn path_feed_fingerprints_track_referenced_file_content() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lex = dir.join("lexicon.toml");
        std::fs::write(&lex, "one").unwrap();
        let config = cfg(&format!(
            "[sparkle_words]\nlexicon = {}\n",
            super::toml_basic_string(&lex.to_string_lossy())
        ));
        let base = config.path_feed_fingerprints();
        assert_eq!(
            base,
            config.path_feed_fingerprints(),
            "untouched files fingerprint stably"
        );
        std::fs::write(&lex, "two").unwrap();
        let lexed = config.path_feed_fingerprints();
        assert_ne!(lexed.deco, base.deco, "lexicon content feeds deco");
        std::fs::remove_file(&lex).unwrap();
        assert_ne!(
            config.path_feed_fingerprints().deco,
            lexed.deco,
            "a vanished lexicon file reads as changed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_feed_generation_binds_consumer_and_fingerprint_to_one_read() {
        use std::hash::Hasher as _;

        let dir =
            std::env::temp_dir().join(format!("aterm-feed-generation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lexicon = dir.join("lexicon.toml");
        let v1 = "# generation-v1\n";
        let v2 = "# generation-v2\n";
        std::fs::write(&lexicon, v1).unwrap();
        let lexicon_path = lexicon.to_string_lossy().into_owned();
        let config = cfg(&format!("[sparkle_words]\nlexicon = {lexicon_path:?}\n"));
        let model = aterm_models_misc::path_feed_snapshot_model();
        let mut model_state = model.init_state();
        let v1_fingerprint = config.path_feed_fingerprints().deco;

        let mut reads = 0;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let (_, admitted) = super::read_and_fingerprint_path_feed_with_reader(
            &lexicon_path,
            aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES,
            &mut hasher,
            |admitted_path, max_bytes| {
                reads += 1;
                let admitted =
                    aterm_effects::file_feed::read_bounded_regular_utf8(admitted_path, max_bytes);
                std::fs::write(&lexicon, v2).unwrap();
                admitted
            },
        );
        let admitted_fingerprint = hasher.finish();
        for action in ["Read", "LiveMutate", "Publish"] {
            assert!(
                model.fire(action, &mut model_state),
                "{action}: {model_state:?}"
            );
        }
        assert!(model.check_invariant("PublishedPairComesFromAdmittedRead", &model_state));
        assert_eq!(model_state["prepared"], 1);
        assert_eq!(model_state["fingerprint"], 1);
        assert_eq!(model_state["live"], 2);
        assert_eq!(reads, 1, "the configured path is opened exactly once");
        assert_eq!(
            admitted.as_deref(),
            Ok(v1),
            "the consumer holds the admitted v1"
        );
        assert_eq!(
            admitted_fingerprint, v1_fingerprint,
            "Tier-1 projection: prepared=1 and fingerprint=1 derive from the admitted v1 String"
        );
        assert_ne!(
            admitted_fingerprint,
            config.path_feed_fingerprints().deco,
            "Tier-1 negative control: the v1 consumer never carries the live v2 fingerprint"
        );

        let replacement = config.prepare_path_feed_generation();
        assert_eq!(
            replacement.fingerprints,
            config.path_feed_fingerprints(),
            "the next generation admits and identifies the replacement bytes"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn disabled_sparkle_performs_no_path_feed_reads() {
        let dir = std::env::temp_dir().join(format!("aterm-disabled-feed-{}", std::process::id()));
        let lexicon = dir.join("lexicon.toml");
        let toy = dir.join("toy.toml");
        let disabled = cfg(&format!(
            "[sparkle_words]\nenabled = false\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
            lexicon.to_string_lossy(),
            toy.to_string_lossy(),
        ));

        super::reset_path_feed_read_count();
        let prepared = disabled.prepare_path_feed_generation();
        assert_eq!(super::path_feed_read_count(), 0);
        assert_eq!(prepared.sparkle.consumer_capabilities(), Default::default());

        let enabled = cfg(&format!(
            "[sparkle_words]\nenabled = true\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
            lexicon.to_string_lossy(),
            toy.to_string_lossy(),
        ));
        super::reset_path_feed_read_count();
        let _ = enabled.prepare_path_feed_generation();
        assert_eq!(
            super::path_feed_read_count(),
            2,
            "negative control: enabling the same two configured feeds admits each once"
        );
    }

    #[test]
    fn the_launch_generation_lands_the_same_kitty_vocabulary_after_the_join() {
        let config = cfg("[sparkle_words]\nlanguages = [\"en\", \"es\"]\n");
        let reload = config.prepare_path_feed_generation();
        let held = super::TRICKS_COMPILE_GATE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (handed_tx, handed) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = handed_tx.send(config.prepare_launch_path_feed_generation());
        });
        let launch = handed
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the launch generation is handed over while its vocabulary compiles");
        let mut scratch = String::new();
        assert_eq!(
            launch.sparkle.tricks().classify("sit", &mut scratch),
            None,
            "handed over before its vocabulary landed: a press reads the empty one"
        );
        drop(held);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the launch generation's kitty vocabulary to land",
            || launch.sparkle.tricks.0.get().map(|_| ()),
        );
        assert!(
            reload
                .sparkle
                .tricks()
                .classify("sit", &mut scratch)
                .is_some(),
            "negative control: a compiled vocabulary knows `sit`"
        );
        for word in ["sit", "kitty", "please", "siéntate", "sientate", "compile"] {
            assert_eq!(
                launch.sparkle.tricks().classify(word, &mut scratch),
                reload.sparkle.tricks().classify(word, &mut scratch),
                "{word}"
            );
        }
        assert_eq!(
            launch.sparkle.tricks().surface_count(),
            reload.sparkle.tricks().surface_count()
        );
    }

    #[test]
    fn an_unlanded_kitty_vocabulary_classifies_nothing() {
        let unlanded = super::TrickVocabulary(std::sync::Arc::default());
        let mut scratch = String::new();
        assert_eq!(unlanded.get().classify("sit", &mut scratch), None);
        assert_eq!(unlanded.get().surface_count(), 0);
        assert!(
            super::TrickVocabulary::compiled(vec!["en".to_string()])
                .get()
                .classify("sit", &mut scratch)
                .is_some(),
            "negative control: the compiled vocabulary knows `sit`"
        );
    }

    #[test]
    fn path_feed_fingerprints_enforce_the_loader_specific_caps() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-caps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lexicon = dir.join("lexicon.toml");
        let toy = dir.join("toy.toml");
        let config = cfg(&format!(
            "[sparkle_words]\nlexicon = {:?}\ntoy_packs = [{:?}]\n",
            lexicon.to_string_lossy(),
            toy.to_string_lossy(),
        ));
        let missing = config.path_feed_fingerprints();

        std::fs::write(
            &lexicon,
            vec![b'x'; aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES + 1],
        )
        .unwrap();
        std::fs::write(
            &toy,
            vec![b'x'; aterm_effects::spec::MAX_TOY_PACK_BYTES + 1],
        )
        .unwrap();
        assert_eq!(
            config.path_feed_fingerprints(),
            missing,
            "oversized inputs are rejected exactly like the loaders, not hashed past their caps"
        );

        std::fs::write(&lexicon, "# admitted lexicon\n").unwrap();
        std::fs::write(&toy, "# admitted toy manifest bytes\n").unwrap();
        let admitted = config.path_feed_fingerprints();
        assert_ne!(admitted.deco, missing.deco);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn path_feed_fingerprint_and_loaders_refuse_a_writerless_fifo() {
        use std::os::unix::ffi::OsStrExt as _;

        let path = std::env::temp_dir().join(format!("aterm-feed-fifo-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `path_c` is a live NUL-terminated pathname and mkfifo retains no pointer. The path is unique to this test process.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        let config = cfg(&format!(
            "[sparkle_words]\nlexicon = {0:?}\ntoy_packs = [{0:?}]\n",
            path.to_string_lossy(),
        ));
        let fifo_fingerprint = config.path_feed_fingerprints();
        let _ = config.sparkle_runtime_parts();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            config.path_feed_fingerprints(),
            fifo_fingerprint,
            "a FIFO and a missing file are both rejected admissions"
        );
    }

    #[test]
    fn byte_equal_reload_still_rereads_touched_path_feed_files() {
        let dir = std::env::temp_dir().join(format!("aterm-feed-touch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lex = dir.join("lexicon.toml");
        std::fs::write(&lex, "# lexicon v1\n").unwrap();

        let mut app = crate::App::headless_for_test();
        app.config = cfg(&format!(
            "[sparkle_words]\nlexicon = {}\n",
            super::toml_basic_string(&lex.to_string_lossy())
        ));
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.path_feed_fps = app.config.path_feed_fingerprints();
        app.recompute_sparkle();
        assert!(!app.sparkle_dirty, "recompute clears the dirty latch");

        let (deco_cfg, lexicon) = {
            let r = app.sparkle.as_ref().expect("sparkle resolves on defaults");
            (r.cfg.clone(), r.lexicon.clone())
        };
        let wid = crate::WindowId(0);
        let seed_scan = |app: &mut crate::App| {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.fx.decos_mut().rescan_from_cells(
                &[],
                &[],
                0,
                0,
                &lexicon,
                &deco_cfg,
                7,
                std::time::Instant::now(),
            );
            assert!(
                !ws.fx.decos().needs_rescan(7),
                "probe seeded: scanned at epoch 7"
            );
        };
        seed_scan(&mut app);

        let fresh = app.config.path_feed_fingerprints();
        app.refresh_path_feeds(fresh);
        assert!(!app.sparkle_dirty, "nothing changed ⇒ no recompute re-arm");
        assert!(
            !app.windows[&wid].fx.decos().needs_rescan(7),
            "…and no word-deco reset (the settings-commit + watcher dedupe win)"
        );

        std::fs::write(&lex, "# lexicon v2\n").unwrap();
        let fresh = app.config.path_feed_fingerprints();
        app.refresh_path_feeds(fresh);
        assert!(
            app.sparkle_dirty,
            "lexicon content change re-arms the recompute"
        );
        assert!(
            app.windows[&wid].fx.decos().needs_rescan(7),
            "changed lexicon hard-resets the per-window decorations"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deco_feed_gate_sees_lexicon_content_behind_an_unrelated_edit() {
        let old = cfg("font_px = 12.0\n");
        let new = cfg("font_px = 14.0\n");
        assert!(
            !super::deco_feed_changed(&old, &new, 7, 7),
            "an unrelated edit with unchanged feeds must not wipe decorations"
        );
        assert!(
            super::deco_feed_changed(&old, &new, 8, 7),
            "a lexicon-file edit behind an unrelated config edit must reset"
        );
        let sparkled = cfg("font_px = 12.0\n[sparkle_words]\nenabled = false\n");
        assert!(
            super::deco_feed_changed(&old, &sparkled, 7, 7),
            "a [sparkle_words] table edit resets as before"
        );
    }
}

#[cfg(test)]
mod whole_cell_min_size_tests {

    #[test]
    fn min_size_sits_on_the_whole_cell_frame_lattice() {
        let app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let min = app.whole_cell_min_size(wid);
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let base_h =
            app.win_head(wid) + app.win_pad_top(wid) + pad + usize::from(app.tab_strip_rows) * ch;

        assert_eq!(
            (min.width as usize - 2 * pad) % cw,
            0,
            "min width must be an exact whole-cell frame"
        );
        assert_eq!(
            (min.height as usize - base_h) % ch,
            0,
            "min height must be an exact whole-cell frame"
        );
        assert!(min.width >= 164, "min width keeps the 164 logical floor");
        assert!(min.height >= 98, "min height keeps the 98 logical floor");
        assert!((min.width as usize) < 164 + cw + 2 * pad + cw);
        assert!((min.height as usize) < 98 + ch + base_h + ch);
    }
}

#[cfg(test)]
mod theme_live_tests {
    #[test]
    fn apply_theme_live_recolours_backend_and_drops_caches() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);

        let mut term = aterm_core::terminal::Terminal::new(2, 2);
        term.process(b"\x1b[?25l");
        let input = term.cell_frame(2, 2);
        let render = |backend: &mut crate::Backend| match backend {
            crate::Backend::Cpu(r) => r.render_input(&input),
            crate::Backend::Gpu(_) => unreachable!("headless test backend is CPU"),
        };
        let before = render(app.backend.ready_mut());

        app.windows.get_mut(&wid).unwrap().last_strip_fp = Some((
            0xFEED,
            80,
            false,
            None,
            false,
            None,
            crate::tab_bar::VersionSlotCols::NONE,
            false,
            crate::tab_bar::CaptionSlot::NONE,
            None,
            None,
        ));

        let new_theme =
            aterm_render::Theme::from(&aterm_types::scheme::builtin("Dracula").unwrap());
        assert_ne!(
            app.theme.bg, new_theme.bg,
            "test needs a real colour change"
        );

        app.apply_theme_live(new_theme);

        assert_eq!(app.theme.bg, new_theme.bg);
        assert_eq!(app.theme.fg, new_theme.fg);
        assert!(app.windows[&wid].last_strip_fp.is_none(), "strip cache");
        assert!(app.windows[&wid].last_present.is_none(), "RepaintKey gate");
        let got = render(app.backend.ready_mut());
        assert_ne!(got.pixels, before.pixels, "the idle grid recoloured");
        let mut fresh = aterm_render::Renderer::from_system(crate::FONT_PX, new_theme)
            .expect("system font for test renderer");
        fresh.set_line_height(super::Config::default().line_height_or_default());
        assert_eq!(got.pixels, fresh.render_input(&input).pixels);
    }
}

#[cfg(test)]
mod matrix_rain_cfg_tests {

    use super::{Config, PRIVACY_WARMUP_HOLD_MS_MAX, PrivacyWarmup};
    use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainConfig, RainHue};

    const BG: u32 = 0x0011_1318;
    const FG: u32 = 0x00D0_D0D0;
    const CURSOR: u32 = 0x0050_FA7B;

    fn rain(toml: &str) -> Option<RainConfig> {
        let cfg = aterm_toml::from_str::<Config>(toml).expect("valid toml");
        cfg.matrix_rain_enabled()
            .then(|| cfg.matrix_rain_params(BG, FG, CURSOR))
    }

    #[test]
    fn rain_is_off_by_default_and_params_ignore_the_enabled_bit() {
        assert!(!Config::default().matrix_rain_enabled());
        assert!(rain("[matrix_rain]\nfps = 42").is_none());
        assert!(rain("[matrix_rain]\nenabled = false\nfps = 42").is_none());
        let params = |toml: &str| {
            aterm_toml::from_str::<Config>(toml)
                .unwrap()
                .matrix_rain_params(BG, FG, CURSOR)
        };
        let disabled = params("[matrix_rain]\nenabled = false\nfps = 24");
        assert_eq!(disabled, params("[matrix_rain]\nenabled = true\nfps = 24"));
        assert_eq!(disabled.fps, 24, "knobs still resolve while disabled");
    }

    #[test]
    fn packages_retired_switch_note_and_retired_keys_load() {
        let note = |text: &str| {
            aterm_toml::from_str::<Config>(text)
                .unwrap()
                .packages_retired_switch_note()
        };
        assert_eq!(
            note("[packages]\nauto_update = false"),
            Some(atpkg::config::auto_update_note(false, None))
        );
        assert_eq!(
            note("[packages]\nenabled = true\nauto_update = false"),
            Some(atpkg::config::auto_update_note(false, Some(true)))
        );
        for text in [
            "[packages]\nenabled = false\nauto_update = false",
            "[packages]\nauto_update = true",
            "[packages]\nenabled = false",
            "",
        ] {
            assert_eq!(note(text), None, "{text:?}");
        }
        let retired = "[packages]\nchannel = 7\ninclude = \"x\"\n[packages.links]\nay = \"~/ay\"\n";
        assert!(aterm_toml::from_str::<Config>(retired).is_ok());
    }

    #[test]
    fn packages_defaults_agree_across_the_two_config_readers() {
        for text in [
            "",
            "[packages]\n",
            "[packages]\nenabled = false\n",
            "[packages]\nauto_update = false\n",
            "[packages]\nenabled = true\nauto_update = false\n",
            "[packages]\nauto_update = true\n",
            "[packages]\nauto_install = false\n",
            "[packages]\nseed_install = false\n",
            "[packages]\nseed_install = false\nauto_install = true\n",
            "[packages]\nseed_install = true\nauto_install = false\n",
        ] {
            let gui = aterm_toml::from_str::<Config>(text).unwrap();
            let pkg = atpkg::config::parse_packages(text);
            assert_eq!(gui.packages_enabled(), pkg.enabled(), "enabled: {text:?}");
            assert_eq!(
                gui.packages_auto_install(),
                pkg.auto_install(),
                "auto_install: {text:?}"
            );
        }
        for text in [
            "",
            "[reroute]\nannounce = true\n",
            "[reroute]\nannounce = false\n",
            "[reroute]\nstrict = true\n",
            "[reroute]\nannounce = false\nstrict = false\n",
        ] {
            let gui = aterm_toml::from_str::<Config>(text).unwrap();
            assert_eq!(
                gui.reroute
                    .as_ref()
                    .and_then(|r| r.announce)
                    .unwrap_or(true),
                atpkg::config::parse_reroute(text).announce(),
                "reroute.announce: {text:?}"
            );
            assert_eq!(
                gui.reroute.as_ref().and_then(|r| r.strict).unwrap_or(false),
                atpkg::config::parse_reroute(text).strict(),
                "reroute.strict: {text:?}"
            );
        }
    }

    #[test]
    fn update_enabled_agrees_with_the_updaters_own_reader() {
        for text in [
            "",
            "[update]\n",
            "[update]\nenabled = true\n",
            "[update]\nenabled = false\n",
        ] {
            let gui = aterm_toml::from_str::<Config>(text).unwrap();
            assert_eq!(
                gui.update.as_ref().and_then(|u| u.enabled).unwrap_or(true),
                aterm_update_core::settings::parse_update_enabled(text),
                "{text:?}"
            );
        }
    }

    #[test]
    fn machine_defaults_agree_across_the_two_config_readers() {
        let gui = Config::default();
        let pkg = atpkg::config::MachineConfig::default();
        assert_eq!(gui.machine_universal_control(), pkg.universal_control());
        assert_eq!(gui.machine_rosetta(), pkg.rosetta());
        for text in [
            "[machine]\nuniversal_control = \"leave\"\n",
            "[machine]\nuniversal_control = \"off\"\n",
            "[machine]\nuniversal_control = \"\"\n",
            "[machine]\nrosetta = false\n",
            "[machine]\nrosetta = true\n",
            "[machine]\nuniversal_control = \"leave\"\nrosetta = false\n",
            "[machine]\n",
            "",
        ] {
            let gui = aterm_toml::from_str::<Config>(text).unwrap();
            let pkg = atpkg::config::parse_machine(text);
            assert_eq!(
                gui.machine_universal_control(),
                pkg.universal_control(),
                "{text:?}"
            );
            assert_eq!(gui.machine_rosetta(), pkg.rosetta(), "{text:?}");
        }
        let bogus =
            aterm_toml::from_str::<Config>("[machine]\nuniversal_control = \"bogus\"\n").unwrap();
        assert_eq!(
            bogus.machine_universal_control(),
            atpkg::config::UniversalControlPolicy::Leave,
            "an unknown spelling is the inert reading, never a write on the machine"
        );
    }

    #[test]
    fn privacy_absent_section_offers_warmup_and_observes_as_sentinel() {
        use crate::consent_observer::ObserverMode;
        let c = Config::default();
        assert!(c.privacy_probe_gate().permits());
        assert_eq!(c.privacy_warmup(), PrivacyWarmup::Automatic);
        let mode = |text: &str| {
            aterm_toml::from_str::<Config>(text)
                .expect("parses")
                .privacy_observer_mode()
        };
        assert_eq!(mode(""), ObserverMode::Sentinel, "no [privacy] table");
        assert_eq!(mode("[privacy]\n"), ObserverMode::Sentinel, "key absent");
        assert_eq!(mode("[privacy]\nobserver = false\n"), ObserverMode::Off);
    }

    #[test]
    fn the_default_warmup_omits_app_data_but_an_explicit_list_may_include_it() {
        let absent = Config::default();
        let defaults = absent.privacy_warmup_folders();
        assert_eq!(defaults, vec!["documents", "desktop", "downloads"]);
        let explicit = aterm_toml::from_str::<Config>(
            "[privacy]\nwarmup_folders = [\"documents\", \"app-data\"]\n",
        )
        .expect("explicit list parses");
        assert_eq!(
            explicit.privacy_warmup_folders(),
            vec!["documents".to_string(), "app-data".to_string()]
        );
    }

    const ABS_TMP: &str = if cfg!(windows) { "C:/tmp" } else { "/tmp" };

    #[test]
    fn privacy_explicit_values_win_over_the_defaults() {
        let c = aterm_toml::from_str::<Config>(&format!(
            concat!(
                "[privacy]\n",
                "enabled = false\n",
                "check = false\n",
                "notice = false\n",
                "report_attribution = false\n",
                "warmup = \"never\"\n",
                "observer = true\n",
                "protected_roots = [\"{}/aterm-privacy-explicit\"]\n",
                "auto_accept = true\n",
            ),
            ABS_TMP
        ))
        .expect("explicit table parses");
        assert!(
            !c.privacy_probe_gate().permits(),
            "either bit off ⇒ the gate refuses before any syscall"
        );
        assert_eq!(c.privacy_warmup(), PrivacyWarmup::Never);
        assert_eq!(
            c.privacy_observer_mode(),
            crate::consent_observer::ObserverMode::Full,
            "the full stream is opt-in from config, and config alone"
        );
        assert_eq!(
            c.privacy_protected_roots(),
            vec![std::path::PathBuf::from(format!(
                "{ABS_TMP}/aterm-privacy-explicit"
            ))],
            "an absolute override replaces the containment set verbatim"
        );
        assert_eq!(
            c.privacy.as_ref().and_then(|p| p.auto_accept),
            Some(true),
            "the reserved key round-trips so --validate-config can refuse it"
        );

        let over = aterm_toml::from_str::<Config>("[privacy]\nwarmup_hold_ms = 3600000\n")
            .expect("parses");
        assert_eq!(
            over.privacy_warmup_hold_ms(),
            PRIVACY_WARMUP_HOLD_MS_MAX,
            "an hour-long hold is clamped, not honored"
        );
        let typo = aterm_toml::from_str::<Config>("[privacy]\nwarmup = \"first-launch\"\n")
            .expect("an unknown spelling must NOT fail the whole file");
        assert_eq!(
            typo.privacy_warmup(),
            PrivacyWarmup::default(),
            "unknown spellings fall back; the validator names them"
        );
        assert_eq!(
            PrivacyWarmup::parse("Automatic"),
            Some(PrivacyWarmup::Automatic)
        );
        assert!(PrivacyWarmup::OnRequest.offered() && PrivacyWarmup::Automatic.offered());
        assert!(!PrivacyWarmup::Never.offered());
    }

    #[cfg(unix)]
    #[test]
    fn privacy_empty_protected_roots_resolve_to_the_containment_private_set() {
        let expected: Vec<std::path::PathBuf> = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|home| {
                aterm_containment::sbpl::PRIVATE_SUBDIRS
                    .iter()
                    .map(|sub| home.join(sub))
                    .collect()
            })
            .unwrap_or_default();
        let explicit_empty =
            aterm_toml::from_str::<Config>("[privacy]\nprotected_roots = []\n").expect("parses");
        assert_eq!(explicit_empty.privacy_protected_roots(), expected);
        assert_eq!(
            Config::default().privacy_protected_roots(),
            expected,
            "an absent key resolves the same way an empty one does"
        );
        assert!(
            !expected.is_empty(),
            "this test is only meaningful with a $HOME; the resolver returns \
             empty for 'cannot answer', never for 'nothing is protected'"
        );
    }

    #[test]
    fn privacy_protected_root_overrides_expand_tilde_and_drop_relatives() {
        let c = aterm_toml::from_str::<Config>(&format!(
            "[privacy]\nprotected_roots = [\"~/src\", \"relative/path\", \"{ABS_TMP}/absolute\"]\n"
        ))
        .expect("parses");
        let roots = c.privacy_protected_roots();
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut expected = Vec::new();
        if let Some(home) = home {
            expected.push(home.join("src"));
        }
        expected.push(std::path::PathBuf::from(format!("{ABS_TMP}/absolute")));
        assert_eq!(
            roots, expected,
            "the relative entry is dropped, not guessed"
        );
    }

    #[test]
    fn clamps_hold_at_both_edges() {
        let lo = rain(concat!(
            "[matrix_rain]\nenabled = true\n",
            "fps = 1\ndensity = 0\nspeed = 0\ntrail = 0\n",
            "alpha = 0\nhead_alpha = 0\nmutation_ms = 1\nidle_secs = 0\n"
        ))
        .unwrap();
        assert!(
            lo.fps > 1
                && lo.density > 0
                && lo.speed > 0
                && lo.trail > 0
                && lo.mutation_ms > 1
                && lo.idle_secs > 0
        );
        assert_eq!(lo.alpha_override, Some(RAIN_ALPHA_FLOOR));
        let hi = rain(concat!(
            "[matrix_rain]\nenabled = true\n",
            "fps = 240\nalpha = 255\nhead_alpha = 255\n"
        ))
        .unwrap();
        assert!(hi.fps <= 60);
        assert_eq!(hi.alpha_override, Some(RAIN_ALPHA_CAP), "READABLE ceiling");
        assert_eq!(hi.head_alpha_override, Some(RAIN_ALPHA_CAP));
    }

    #[test]
    fn head_alpha_floors_at_body_alpha() {
        let c = rain("[matrix_rain]\nenabled = true\nalpha = 120\nhead_alpha = 20").unwrap();
        assert_eq!(c.alpha_override, Some(120));
        assert_eq!(c.head_alpha_override, Some(120));
        let c = rain("[matrix_rain]\nenabled = true\nhead_alpha = 5").unwrap();
        assert_eq!(c.alpha_override, None);
        assert_eq!(c.head_alpha_override, Some(RAIN_ALPHA_FLOOR));
    }

    #[test]
    fn hue_parses_and_bad_hex_fails_closed() {
        let hue = |h: &str| {
            rain(&format!("[matrix_rain]\nenabled = true\nhue = \"{h}\""))
                .expect("hue never aborts the resolve")
                .hue
        };
        assert_eq!(hue("matrix"), RainHue::Matrix);
        assert_eq!(hue("theme"), RainHue::Theme);
        assert_eq!(hue("THEME"), RainHue::Theme, "case-insensitive");
        assert_eq!(hue("#28D75F"), RainHue::Custom(0x0028_D75F));
        assert_eq!(
            hue("#a0b1c2"),
            RainHue::Custom(0x00A0_B1C2),
            "lowercase hex"
        );
        for bad in ["#28D75", "#28D75F0", "#GGGGGG", "chartreuse", "", "#"] {
            assert_eq!(hue(bad), RainHue::Matrix, "{bad:?} must fail closed");
        }
    }
}

#[cfg(test)]
mod matrix_rain_app_tests {

    use super::Config;
    use crate::keybinding::Action;

    fn cfg(toml: &str) -> Config {
        aterm_toml::from_str(toml).expect("valid toml")
    }

    #[test]
    fn toggle_dispatch_flips_front_session_override() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.recompute_matrix_rain();
        assert!(!app.session_rain_enabled(0), "default config: off");
        assert!(
            app.windows.values().all(|ws| ws.fx.rain().is_none()),
            "no engine is constructed while rain is off"
        );

        app.dispatch_action(wid, Action::ToggleMatrixRain);
        assert_eq!(app.pool.rain_override(0), Some(true));
        assert!(app.session_rain_enabled(0), "override forces rain on");
        assert!(app.rain_dirty, "toggle marks the resolve stale");
        app.recompute_matrix_rain();
        assert!(
            app.rain.is_some(),
            "params available for the forced-on field"
        );

        app.config = cfg("[matrix_rain]\nenabled = false");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert!(
            app.session_rain_enabled(0),
            "session override survives (and outranks) a config reload"
        );

        let rc = crate::rain_config_for_window(app.rain.unwrap(), wid);
        app.windows.get_mut(&wid).unwrap().fx.build_rain(rc);
        app.dispatch_action(wid, Action::ToggleMatrixRain);
        assert_eq!(app.pool.rain_override(0), Some(false));
        assert!(!app.session_rain_enabled(0));
        assert!(
            app.windows[&wid].fx.rain().is_some(),
            "the engine survives the off flip and drains via the suspend path"
        );

        app.config = cfg("[matrix_rain]\nenabled = true");
        app.rain_dirty = true;
        app.recompute_matrix_rain();
        assert!(
            !app.session_rain_enabled(0),
            "session off-override outranks an enabled config"
        );
        assert_eq!(app.pool.rain_override(999), None, "unknown session: none");
    }

    #[test]
    fn rain_control_verb_reads_and_writes_front_session() {
        let mut app = crate::App::headless_for_test();
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Status).as_deref(),
            Ok(
                "config_enabled=false session_override=none effective=false \
                 engine=none active=false scope=window focused=true animating=true"
            ),
            "status is a pure read"
        );
        assert_eq!(
            app.rain_control(crate::RainCtlOp::On).as_deref(),
            Ok("config_enabled=false session_override=on effective=true \
                 engine=none active=false scope=window focused=true animating=true"),
        );
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Toggle).as_deref(),
            Ok("config_enabled=false session_override=off effective=false \
                 engine=none active=false scope=window focused=true animating=true"),
        );
        app.config = cfg("[matrix_rain]\nenabled = true");
        app.rain_dirty = true;
        assert_eq!(
            app.rain_control(crate::RainCtlOp::Status).as_deref(),
            Ok("config_enabled=true session_override=off effective=false \
                 engine=none active=false scope=window focused=true animating=true"),
            "the off override keeps winning over the enabled config"
        );
    }

    #[test]
    fn fx_control_verb_arms_once_and_reports() {
        let mut app = crate::App::headless_for_test();
        app.config = cfg("cursor_trail = true\ncursor_trail_style = \"rainbow kitty pet\"");
        let status = app
            .fx_control(crate::FxCtlOp::Status)
            .expect("status reads");
        assert!(
            status.starts_with(
                "armed=none sig=00000000 bars=0 live=false bar=none drive=0.00 cooldown_ms=0 style="
            ),
            "{status}"
        );
        assert!(
            status.ends_with("style=rainbow-kitty"),
            "positive fixture must enable the real style"
        );
        let sig = aterm_effects::kitty_sing::song_signature('C');
        let arm = app.fx_control(crate::FxCtlOp::Celebrate {
            sig,
            bars: 2,
            on: aterm_effects::kitty_sing::CelebrateOn::Green,
        });
        let line = arm.expect("the first arm is admitted");
        assert!(
            line.starts_with(&format!(
                "armed=green sig={sig:08x} bars=2 live=false bar=none drive=0.00 cooldown_ms="
            )),
            "{line}"
        );
        let again = app.fx_control(crate::FxCtlOp::Celebrate {
            sig,
            bars: 1,
            on: aterm_effects::kitty_sing::CelebrateOn::Enter,
        });
        assert!(again.is_err(), "one arm at a time");
        let status = app
            .fx_control(crate::FxCtlOp::Status)
            .expect("status reads");
        assert!(
            status.starts_with("armed=green"),
            "status is a pure read: {status}"
        );
    }

    #[test]
    fn fx_control_refuses_inactive_visual_policies_without_spending_an_arm() {
        use aterm_effects::kitty_sing::CelebrateOn;
        for (config, on) in [
            "cursor_trail = false\ncursor_trail_style = \"rainbow kitty pet\"",
            "cursor_trail = true\ncursor_trail_style = \"laser\"",
            "cursor_trail = true\ncursor_trail_style = \"rainbow kitty pet\"\nserious_mode = true",
        ]
        .into_iter()
        .flat_map(|c| [CelebrateOn::Green, CelebrateOn::Done].map(|on| (c, on)))
        {
            let mut app = crate::App::headless_for_test();
            app.config = cfg(config);
            app.apply_serious_mode(app.config.serious_mode_or_default());
            let arm = app.fx_control(crate::FxCtlOp::Celebrate {
                sig: aterm_effects::kitty_sing::song_signature('C'),
                bars: 2,
                on,
            });
            assert!(arm.is_err(), "{config} {on:?}: {arm:?}");
            let status = app
                .fx_control(crate::FxCtlOp::Status)
                .expect("status still reads");
            assert!(
                status.starts_with(
                    "armed=none sig=00000000 bars=0 live=false bar=none drive=0.00 cooldown_ms=0 "
                ),
                "refusal spends no arm or cooldown: {status}"
            );
        }
    }

    #[test]
    fn zero_seed_derives_stable_per_window() {
        let base = crate::matrix_rain::RainConfig {
            enabled: true,
            ..Default::default()
        };
        let a = crate::rain_config_for_window(base, crate::WindowId(0));
        let b = crate::rain_config_for_window(base, crate::WindowId(0));
        let c = crate::rain_config_for_window(base, crate::WindowId(1));
        assert_ne!(a.seed, 0, "the sentinel resolves to a real seed");
        assert_eq!(a.seed, b.seed, "stable for the window's whole life");
        assert_ne!(a.seed, c.seed, "distinct fields per window");
        let pinned = crate::matrix_rain::RainConfig { seed: 7, ..base };
        assert_eq!(
            crate::rain_config_for_window(pinned, crate::WindowId(3)).seed,
            7,
            "a configured seed is reproducible verbatim"
        );
    }
}

#[cfg(test)]
mod agents_auto_prime_tests {
    use super::agents_auto_prime_from_text;

    #[test]
    fn the_knob_reads_without_a_full_config_load() {
        assert!(agents_auto_prime_from_text(""), "empty file: default");
        assert!(
            agents_auto_prime_from_text("tab_status = false"),
            "unrelated key: default"
        );
        assert!(!agents_auto_prime_from_text("agents_auto_prime = false"));
        assert!(agents_auto_prime_from_text("agents_auto_prime = true"));
        assert!(agents_auto_prime_from_text("agents_auto_prime = "));
        assert!(agents_auto_prime_from_text("agents_auto_prime = \"no\""));
    }
}

#[cfg(test)]
mod update_check_settings_tests {
    #[test]
    fn persisted_check_settings_bind_source_and_manual_policy_in_one_parse() {
        let defaults = super::update_check_settings_from_text("").unwrap();
        assert!(defaults.auto_apply);
        assert_eq!(defaults.source, aterm_update::Source::resolve(None, None));
        let manual = super::update_check_settings_from_text(
            "[update]\nowner = \"private-org\"\nrepo = \"fork\"\nauto_apply = false\n",
        )
        .unwrap();
        assert!(!manual.auto_apply);
        assert_eq!(
            manual.source,
            aterm_update::Source::resolve(Some("private-org"), Some("fork"))
        );
        assert!(
            super::update_check_settings_from_text("[update]\nauto_apply = invalid").is_none(),
            "malformed existing policy must not fall back to automatic"
        );
    }
}

#[cfg(test)]
mod trail_style_resolution_tests {
    use super::*;
    use aterm_effects::cursor_glow::{GlowConfig, GlowStyle};

    fn glow(raw: &str, enabled: bool) -> GlowConfig {
        resolve_cursor_glow(
            CursorGlowInputs {
                enabled,
                color: None,
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: 0.7,
                radius: 0.6,
                ring: true,
            },
            resolve_trail_presentation(raw),
            0x00FF_FFFF,
            true,
            0x00C8_D3F5,
            0x001A_1B26,
            0.5,
        )
    }

    fn warning_for(raw: &str) -> Option<String> {
        Config {
            cursor_trail_style: Some(raw.to_string()),
            ..Config::default()
        }
        .cursor_trail_style_warning()
    }

    #[test]
    fn classic_receives_the_shipped_knobs() {
        let mut app = crate::App::headless_for_test();
        app.config.cursor_trail_style = Some("classic".to_string());
        let cfg = app.glow_config();
        assert_eq!(
            (
                cfg.style,
                cfg.enabled,
                cfg.duration,
                cfg.length,
                cfg.intensity,
                cfg.radius,
                cfg.ring,
                cfg.beam,
                cfg.classic_mono,
                cfg.head_dx,
            ),
            (
                GlowStyle::Classic,
                true,
                std::time::Duration::from_millis(260),
                24,
                aterm_effects::cursor_fx::CLASSIC_DEFAULT_INTENSITY,
                0.6,
                true,
                true,
                false,
                0.5,
            ),
            "the window hands Classic other knobs than the dark wake is pinned on"
        );
        assert_eq!(
            aterm_effects::cursor_fx::CLASSIC_DEFAULT_INTENSITY,
            0.7,
            "Classic's unset brightness is the v0.28 release's"
        );
        app.config.cursor_trail_length = Some(18);
        assert_eq!(
            app.glow_config().length,
            18,
            "control: a set length is read"
        );
        app.config.cursor_trail_intensity = Some(1.0);
        assert_eq!(
            app.glow_config().intensity,
            1.0,
            "control: a set brightness is read"
        );
        app.config.cursor_trail_intensity = None;
        app.config.cursor_trail_color = Some("#d03a8c".to_string());
        assert_eq!(
            app.glow_config().intensity,
            1.0,
            "control: Classic's one-colour face (a trail colour) draws at full brightness"
        );
        app.config.cursor_trail_color = None;
        app.config.cursor_trail_style = Some("lumen".to_string());
        assert_eq!(
            app.glow_config().intensity,
            1.0,
            "control: another style's unset brightness is full"
        );
    }

    #[test]
    fn native_ribbon_resolution_keeps_tall_default_and_explicit_underline() {
        for (style, expected_tall) in [
            ("rainbow kitty", true),
            (crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE, true),
            ("rainbow kitty flying", true),
            ("rainbow kitty tall", true),
            ("rainbow kitty underline", false),
        ] {
            assert_eq!(
                glow(style, true).ribbon_tall,
                expected_tall,
                "native ribbon resolver returned the wrong geometry for {style:?}"
            );
        }
    }

    #[test]
    fn immutable_catalog_caches_the_complete_trail_presentation() {
        let config = Config {
            cursor_trail_style: Some("rainbow kitty underline".to_string()),
            ..Config::default()
        };
        let catalog = config.resolve_trail_style_cache();
        let cached = catalog.presentation_for(config.cursor_trail_style_raw());
        assert_eq!(cached.style.style, Some(GlowStyle::RainbowKitty));
        assert!(!cached.ribbon_tall);
        assert_eq!(
            cached.pet_species,
            Some(aterm_effects::kitty_pet::PetSpecies::Cat)
        );

        let fallback = catalog.presentation_for("rainbow dog pet");
        assert!(fallback.ribbon_tall);
        assert_eq!(
            fallback.pet_species,
            Some(aterm_effects::kitty_pet::PetSpecies::Dog)
        );
        assert_eq!(
            catalog
                .presentation_for(config.cursor_trail_style_raw())
                .pet_species,
            cached.pet_species
        );
    }

    fn engine_is_v2_for(raw: &str) -> bool {
        use aterm_effects::cursor_glow::{CursorGlow, Geom};
        let cfg = glow(raw, true);
        let mut engine = CursorGlow::default();
        let mut out = Vec::new();
        engine.tick(
            Some((2, 4)),
            std::time::Instant::now(),
            &cfg,
            Geom {
                cw: 8,
                ch: 16,
                rows: 24,
                cols: 80,
                origin_x: 0,
                origin_y: 0,
                win_w: 640,
                win_h: 384,
                head: 0,
            },
            &mut out,
        );
        engine.v2_status().is_some()
    }

    #[test]
    fn every_rainbow_kitty_spelling_is_v2() {
        use aterm_effects::trail_sound::SoundVoice;
        let default = crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE;
        assert_eq!(
            ResolvedTrailPresentation::default(),
            resolve_trail_presentation(default),
            "`Default` must be the resolver's answer for the default spelling"
        );
        let auto = Config::default();
        let marimba = Config {
            trail_sound_style: Some("marimba".to_string()),
            ..Config::default()
        };
        let spellings = crate::prefs::CURSOR_TRAIL_STYLES.iter().copied().chain(
            crate::prefs::CURSOR_TRAIL_STYLE_ALIASES
                .iter()
                .map(|&(alias, _)| alias),
        );
        for raw in spellings {
            let p = resolve_trail_presentation(raw);
            assert!(!p.style.unknown, "{raw:?} is a documented spelling");
            let is_kitty = p.style.style == Some(GlowStyle::RainbowKitty);
            assert_eq!(
                engine_is_v2_for(raw),
                is_kitty,
                "{raw:?}: the engine owns the frame as v2 iff the spelling is the rainbow kitty"
            );
            assert_eq!(
                auto.trail_sound_voice_for(p),
                if is_kitty {
                    SoundVoice::RainbowKittyV2
                } else {
                    SoundVoice::Style
                },
                "{raw:?}: `auto` is the music box iff the spelling is the rainbow kitty"
            );
            assert_eq!(
                marimba.trail_sound_voice_for(p),
                SoundVoice::Marimba,
                "{raw:?}: an explicit instrument is the user's"
            );
        }
        let fallback = resolve_trail_presentation(default);
        for v1 in [
            "rainbow kitty v1",
            "RAINBOW KITTY V1 pet",
            "v2",
            "",
            "phasr",
            "rainbow kity tall",
        ] {
            let p = resolve_trail_presentation(v1);
            assert!(p.style.unknown, "{v1:?} names no style and says so");
            assert_eq!(
                ResolvedTrailStyle {
                    unknown: false,
                    ..p.style
                },
                fallback.style,
                "{v1:?} falls back to the default"
            );
            assert!(
                engine_is_v2_for(v1),
                "{v1:?} draws the default, which is v2"
            );
            assert_eq!(auto.trail_sound_voice_for(p), SoundVoice::RainbowKittyV2);
            assert_eq!(effective_trail_style_token(v1), default, "{v1:?}");
            assert_eq!(
                glow(v1, true).ribbon_tall,
                glow(default, true).ribbon_tall,
                "{v1:?}"
            );
            assert!(warning_for(v1).is_some(), "{v1:?} must warn");
        }
        let config = Config {
            cursor_trail_style: Some("rainbow kitty v1".to_string()),
            ..Config::default()
        };
        let cached = config
            .resolve_trail_style_cache()
            .presentation_for(config.cursor_trail_style_raw());
        assert!(cached.style.unknown);
        assert_eq!(cached.style.style, Some(GlowStyle::RainbowKitty));
        assert_eq!(
            config.trail_sound_voice_for(cached),
            SoundVoice::RainbowKittyV2
        );
    }

    #[test]
    fn off_and_the_master_switch_still_disable_the_trail() {
        for spelling in ["off", "OFF", "  off  "] {
            assert_eq!(resolve_trail_style(spelling).style, None);
            assert!(!resolve_trail_style(spelling).unknown);
            assert!(!glow(spelling, true).enabled, "{spelling:?}");
            assert!(
                warning_for(spelling).is_none(),
                "{spelling:?} is not a typo"
            );
        }
        assert!(
            !glow(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE, false).enabled,
            "`cursor_trail = false` owns the whole effect"
        );
        for &s in crate::prefs::CURSOR_TRAIL_STYLES {
            assert_eq!(
                resolve_trail_style(s).style.is_none(),
                s == "off",
                "style {s:?}"
            );
        }
        for &(alias, _) in crate::prefs::CURSOR_TRAIL_STYLE_ALIASES {
            assert!(
                resolve_trail_style(alias).style.is_some(),
                "alias {alias:?} must not disable the trail"
            );
        }
    }

    #[test]
    fn the_bloom_radius_is_per_style_with_the_knob_as_the_override() {
        fn radius(style: Option<&str>, knob: Option<f32>) -> f32 {
            let config = Config {
                cursor_trail_style: style.map(str::to_string),
                cursor_trail_bloom_radius: knob,
                ..Config::default()
            };
            let catalog = config.resolve_trail_style_cache();
            config.cursor_trail_bloom_radius_or_default(
                catalog.presentation_for(config.cursor_trail_style_raw()),
            )
        }
        let kitty = radius(Some("rainbow kitty"), None);
        assert!(kitty < radius(Some("laser"), None));
        assert_eq!(radius(Some("rainbow kitty pet"), None), kitty);
        assert_eq!(
            radius(None, None),
            radius(Some(default_cursor_trail_style()), None)
        );
        assert_eq!(radius(Some("rainbow kitty"), Some(3.0)), 3.0);
        assert_eq!(radius(Some("laser"), Some(3.0)), 3.0);
    }
}
