// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! User preferences, TOML configuration loading/saving, and live settings updates.
use crate::app_config::{
    Config, DEFAULT_TAB_STATUS_DWELL_MS, DEFAULT_TAB_STATUS_QUIET_AFTER_MS,
    MAX_TAB_STATUS_DWELL_MS, MAX_TAB_STATUS_QUIET_AFTER_MS, MIN_TAB_STATUS_DWELL_MS,
    MIN_TAB_STATUS_QUIET_AFTER_MS,
};
use aterm_effects::cursor_glow::{GlowStyle, spellings};
use aterm_effects::trail_sound::SoundVoice;
use aterm_phase::Program;

/// The TOML keys the settings model edits, each typed on write by [`apply_prefs_edits`].
pub(crate) const EDIT_FONT_PX: &str = "font_px";
pub(crate) const EDIT_FONT_FAMILY: &str = "font_family";
/// The bundled display-face selector (`display_font`).
pub(crate) const EDIT_DISPLAY_FONT: &str = "display_font";

pub(crate) const EDIT_FONT_FAMILY_BOLD: &str = "font_family_bold";
pub(crate) const EDIT_FONT_FAMILY_ITALIC: &str = "font_family_italic";
pub(crate) const EDIT_FONT_FAMILY_BOLD_ITALIC: &str = "font_family_bold_italic";
pub(crate) const EDIT_FONT_SYNTHETIC_STYLE: &str = "font_synthetic_style";
pub(crate) const EDIT_FALLBACK_FONTS: &str = "fallback_fonts";
pub(crate) const EDIT_SYMBOL_FONT: &str = "symbol_font";
pub(crate) const EDIT_EMOJI_FONT: &str = "emoji_font";
pub(crate) const EDIT_LIGATURES: &str = "ligatures";
pub(crate) const EDIT_THEME: &str = "theme";
pub(crate) const EDIT_CURSOR_STYLE: &str = "cursor_style";
pub(crate) const EDIT_CURSOR_BLINK: &str = "cursor_blink";
pub(crate) const EDIT_CURSOR_MOMENTUM_GLOW: &str = "cursor_momentum_glow";
/// The retired cursor-effects master (`Config::cursor_trail`).
pub(crate) const RETIRED_CURSOR_TRAIL: &str = "cursor_trail";
/// The two settings [`RETIRED_CURSOR_TRAIL`] reads as.
pub(crate) const RETIRED_CURSOR_TRAIL_SUCCESSORS: [&str; 3] = [
    EDIT_CURSOR_TRAIL_STYLE,
    EDIT_CURSOR_MOMENTUM_GLOW,
    EDIT_CURSOR_PET,
];
pub(crate) const EDIT_CURSOR_TRAIL_STYLE: &str = "cursor_trail_style";
/// The style `off` replaced.
pub(crate) const EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF: &str = "cursor_trail_style_before_off";
/// The pet beside the cursor (`Config::cursor_pet`), whatever the cursor style is.
pub(crate) const EDIT_CURSOR_PET: &str = "cursor_pet";
pub(crate) const EDIT_TRAIL_SOUNDS: &str = "trail_sounds";
/// Tone-melody toggle (`Config::tone_melody`, default ON).
pub(crate) const EDIT_TONE_MELODY: &str = "tone_melody";
/// Ambient-bed toggle (`Config::trail_sound_bed`, default OFF).
pub(crate) const EDIT_TRAIL_SOUND_BED: &str = "trail_sound_bed";
/// Typing-sound picker (`Config::trail_sound_style`, default `auto`).
pub(crate) const EDIT_TRAIL_SOUND_STYLE: &str = "trail_sound_style";
/// The held-key sing-along riff's own switch (`Config::trail_sound_riff`, default ON).
pub(crate) const EDIT_TRAIL_SOUND_RIFF: &str = "trail_sound_riff";
/// Audible terminal bell (`Config::bell_sound`, default OFF).
pub(crate) const EDIT_BELL_SOUND: &str = "bell_sound";
/// The curse bonk's two gates.
pub(crate) const EDIT_SPARKLE_BONK: &str = "sparkle_words.profanity.bonk";
pub(crate) const EDIT_SPARKLE_BONK_DETONATION: &str = "sparkle_words.profanity.bonk_detonation";
/// Prism wake's pip (`[output_streak] sound`, default ON).
pub(crate) const EDIT_OUTPUT_STREAK_SOUND: &str = "output_streak.sound";
/// Prism wake's switch (`[output_streak] enabled`).
pub(crate) const EDIT_OUTPUT_STREAK_ENABLED: &str = "output_streak.enabled";

/// Every config key that changes what aterm sounds like, and nothing else.
pub(crate) const SOUND_MENU_KEYS: &[&str] = &[
    EDIT_TRAIL_SOUNDS,
    EDIT_TRAIL_SOUND_VOLUME,
    EDIT_TRAIL_SOUND_STYLE,
    EDIT_TONE_MELODY,
    EDIT_TRAIL_SOUND_BED,
    EDIT_TRAIL_SOUND_RIFF,
    EDIT_BELL_SOUND,
    EDIT_SPARKLE_BONK,
    EDIT_SPARKLE_BONK_DETONATION,
    EDIT_OUTPUT_STREAK_SOUND,
];
/// The tuning a cursor style can read.
pub(crate) const CURSOR_STYLE_TUNING_KEYS: &[&str] = &[
    EDIT_CURSOR_TRAIL_INTENSITY,
    EDIT_CURSOR_TRAIL_MS,
    EDIT_CURSOR_TRAIL_LENGTH,
    EDIT_CURSOR_TRAIL_RADIUS,
    EDIT_CURSOR_TRAIL_RING,
    EDIT_CURSOR_TRAIL_COLOR,
    EDIT_CURSOR_TRAIL_ACCENT,
    EDIT_CURSOR_TRAIL_BLOOM,
    EDIT_CURSOR_TRAIL_BLOOM_STRENGTH,
    EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
    EDIT_CURSOR_FIRE_SHIMMER,
    EDIT_HDR_GLOW,
    EDIT_CURSOR_GLOW_SDR_BOOST,
];
pub(crate) const EDIT_CURSOR_TRAIL_COLOR: &str = "cursor_trail_color";
pub(crate) const EDIT_CURSOR_TRAIL_ACCENT: &str = "cursor_trail_accent";
/// `cursor_trail_color`'s one word beside a colour.
pub(crate) const TRAIL_COLOR_CURSOR: &str = aterm_effects::cursor_fx::TrailColor::CURSOR_WORD;

/// [`TRAIL_COLOR_CURSOR`] as a Settings row names it.
pub(crate) const TRAIL_COLOR_CURSOR_LABEL: &str = "Cursor color";

/// What the trail colour key `key` (the colour or its accent) draws while unset.
pub(crate) fn trail_color_default_words(
    key: &str,
    style_raw: &str,
    color: aterm_effects::cursor_fx::TrailColor,
    cursor_color_pinned: bool,
) -> String {
    use aterm_effects::cursor_fx::AccentBase;
    let style = crate::app_config::resolve_trail_style(style_raw).style;
    if key == EDIT_CURSOR_TRAIL_ACCENT {
        let base = style.map_or(AccentBase::Cursor, |style| {
            aterm_effects::cursor_fx::unset_accent_base(style, color, cursor_color_pinned)
        });
        return match base {
            AccentBase::Cursor => "Brighter cursor color".to_string(),
            AccentBase::Own(_, name) => format!("Brighter {}", name.to_lowercase()),
            AccentBase::Trail(_) => "Brighter trail color".to_string(),
        };
    }
    if style == Some(aterm_effects::cursor_glow::GlowStyle::Classic)
        && aterm_effects::cursor_glow::GlowStyle::style_names_classic_mono(style_raw.trim())
    {
        return TRAIL_COLOR_CURSOR_LABEL.to_string();
    }
    style
        .and_then(|style| {
            aterm_effects::cursor_fx::trail_color_default_name(style, cursor_color_pinned)
        })
        .map_or_else(|| TRAIL_COLOR_CURSOR_LABEL.to_string(), str::to_string)
}

/// Trail colour note for the cursor style aterm.toml spells `style_raw`.
pub(crate) fn trail_color_note(style_raw: &str, block: bool, cursor_color_pinned: bool) -> String {
    use aterm_effects::cursor_fx::{glow_color_follows_live_cursor, own_trail_color};
    use aterm_effects::cursor_glow::GlowStyle;
    let resolved = crate::app_config::resolve_trail_style(style_raw);
    let Some(style) = resolved.style else {
        return "Cursor style is Off, so nothing draws these colors.".to_string();
    };
    let name = resolved
        .canonical
        .and_then(spellings::label)
        .unwrap_or(DEFAULT_CURSOR_TRAIL_STYLE);
    if !style.draws_trail_color() {
        return if block {
            format!(
                "{name} draws its own colors on a block cursor and does not use these; on a bar \
                 cursor, a color tints its rod."
            )
        } else {
            let rod = match aterm_effects::cursor_fx::unset_trail_color(style, cursor_color_pinned)
            {
                aterm_effects::cursor_fx::UnsetTrailColor::Own(_, own) => {
                    format!("keeps its own {}", own.to_lowercase())
                }
                aterm_effects::cursor_fx::UnsetTrailColor::Shifting => {
                    "shifts through colors".to_string()
                }
                aterm_effects::cursor_fx::UnsetTrailColor::Cursor => {
                    "follows the cursor color".to_string()
                }
            };
            format!(
                "Blank: {name}\u{2019}s rod {rod} on a bar cursor; any color, \
                 {TRAIL_COLOR_CURSOR_LABEL} among them, tints it."
            )
        };
    }
    if style == GlowStyle::Classic {
        return if GlowStyle::style_names_classic_mono(style_raw.trim()) {
            format!(
                "Blank: Classic draws in the cursor color, as its older spelling in aterm.toml \
                 says; any color, {TRAIL_COLOR_CURSOR_LABEL} among them, draws it in that one \
                 color."
            )
        } else {
            format!(
                "Blank: Classic shifts through colors; any color, {TRAIL_COLOR_CURSOR_LABEL} \
                 among them, draws it in that one color."
            )
        };
    }
    match own_trail_color(style, cursor_color_pinned) {
        Some((_, own)) if glow_color_follows_live_cursor(style, true) => format!(
            "Blank: {name} keeps its {} until you set a cursor color, then follows it; \
             {TRAIL_COLOR_CURSOR_LABEL} makes it follow the cursor color now.",
            own.to_lowercase()
        ),
        Some((_, own)) => format!(
            "Blank: {name} keeps its {}; {TRAIL_COLOR_CURSOR_LABEL} makes it follow the cursor \
             color.",
            own.to_lowercase()
        ),
        None => {
            format!("Blank: {name} follows the cursor color, as {TRAIL_COLOR_CURSOR_LABEL} does.")
        }
    }
}

/// Whether `raw` is a value colour key `key` takes.
pub(crate) fn color_value_is_valid(key: &str, raw: &str) -> bool {
    let raw = raw.trim();
    aterm_types::scheme::parse_hex(raw).is_some()
        || (key == EDIT_CURSOR_TRAIL_COLOR && names_trail_color_cursor(raw))
}

/// Whether `raw` asks `cursor_trail_color` for the cursor's own colour: the file's word
/// ([`TRAIL_COLOR_CURSOR`]) or the words its row shows for it ([`TRAIL_COLOR_CURSOR_LABEL`]),
/// so what a person reads in the field is what they can type back.
pub(crate) fn names_trail_color_cursor(raw: &str) -> bool {
    let raw = raw.trim();
    raw.eq_ignore_ascii_case(TRAIL_COLOR_CURSOR)
        || raw.eq_ignore_ascii_case(TRAIL_COLOR_CURSOR_LABEL)
}

/// What a `cursor_trail_color` value says, read as a web page's setting is ([`aterm_effects::cursor_fx::TrailColor::parse`]).
pub(crate) fn trail_color_setting(raw: Option<&str>) -> aterm_effects::cursor_fx::TrailColor {
    use aterm_effects::cursor_fx::TrailColor;
    raw.and_then(TrailColor::parse)
        .unwrap_or(TrailColor::Automatic)
}
/// The rainbow kitty's user sprite.
pub(crate) const EDIT_CURSOR_NYAN_SPRITE: &str = "cursor_nyan_sprite";
pub(crate) const EDIT_SCROLLBACK: &str = "scrollback_lines";
pub(crate) const EDIT_COPY_ON_SELECT: &str = "copy_on_select";
pub(crate) const EDIT_CURSOR_TRAIL_MS: &str = "cursor_trail_ms";
pub(crate) const EDIT_CURSOR_TRAIL_LENGTH: &str = "cursor_trail_length";
pub(crate) const EDIT_CURSOR_TRAIL_INTENSITY: &str = "cursor_trail_intensity";
pub(crate) const EDIT_CURSOR_TRAIL_RADIUS: &str = "cursor_trail_radius";
pub(crate) const EDIT_CURSOR_TRAIL_RING: &str = "cursor_trail_ring";
pub(crate) const EDIT_CURSOR_TRAIL_BLOOM: &str = "cursor_trail_bloom";
pub(crate) const EDIT_CURSOR_TRAIL_BLOOM_STRENGTH: &str = "cursor_trail_bloom_strength";
pub(crate) const EDIT_CURSOR_TRAIL_BLOOM_RADIUS: &str = "cursor_trail_bloom_radius";
pub(crate) const EDIT_CURSOR_FIRE_SHIMMER: &str = "cursor_fire_shimmer";
pub(crate) const EDIT_HDR_GLOW: &str = "hdr_glow";
pub(crate) const EDIT_CURSOR_GLOW_SDR_BOOST: &str = "cursor_glow_sdr_boost";
pub(crate) const EDIT_FOREGROUND: &str = "foreground";
pub(crate) const EDIT_BACKGROUND: &str = "background";
pub(crate) const EDIT_CURSOR_COLOR: &str = "cursor_color";
pub(crate) const EDIT_SELECTION_COLOR: &str = "selection_color";
pub(crate) const EDIT_SELECTION_FOREGROUND: &str = "selection_foreground";
pub(crate) const EDIT_LINE_HEIGHT: &str = "line_height";
/// `font_size_scale`: multiplies the resolved font size without fixing it.
pub(crate) const EDIT_FONT_SIZE_SCALE: &str = "font_size_scale";
pub(crate) const EDIT_ADJUST_BASELINE: &str = "adjust_baseline";
pub(crate) const EDIT_MINIMUM_CONTRAST: &str = "minimum_contrast";
pub(crate) const EDIT_SELECTION_INACTIVE: &str = "selection_inactive";
/// Whether a split inks a hairline along the edges of the pane that takes keystrokes, a Bool, default ON.
pub(crate) const EDIT_SPLIT_FOCUS_MARK: &str = "split_focus_mark";
pub(crate) const EDIT_CURSOR_BREAK_LIGATURES: &str = "cursor_break_ligatures";
/// Admit Cascadia N:1 merged ligatures, a Bool, default OFF.
pub(crate) const EDIT_MERGED_LIGATURES: &str = "merged_ligatures";
pub(crate) const EDIT_BOLD_IS_BRIGHT: &str = "bold_is_bright";
/// Where slots 16-255 come from (`Config::palette_generate`).
pub(crate) const EDIT_PALETTE_GENERATE: &str = "palette_generate";
pub(crate) const EDIT_FAINT_OPACITY: &str = "faint_opacity";
pub(crate) const EDIT_ADJUST_UNDERLINE_POSITION: &str = "adjust_underline_position";
pub(crate) const EDIT_ADJUST_UNDERLINE_THICKNESS: &str = "adjust_underline_thickness";
pub(crate) const EDIT_UNDERLINE_SKIP_DESCENDERS: &str = "underline_skip_descenders";
pub(crate) const EDIT_TEXT_BLENDING: &str = "text_blending";
pub(crate) const EDIT_FONT_THICKEN: &str = "font_thicken";
pub(crate) const EDIT_STEM_GAMMA: &str = "stem_gamma";
pub(crate) const EDIT_FONT_VARIATION: &str = "font_variation";
pub(crate) const EDIT_FONT_WEIGHT: &str = "font_weight";
pub(crate) const EDIT_MOTION: &str = "motion";
/// Process-wide serious-mode overlay.
pub(crate) const EDIT_SERIOUS_MODE: &str = "serious_mode";
/// Load-adaptive effect shedding, a Bool, default ON.
pub(crate) const EDIT_LOAD_ADAPTIVE_MOTION: &str = "load_adaptive_motion";

/// The hex-colour keys (theme colours plus the cursor-trail overrides), edited as `RRGGBB`/`#RRGGBB` strings ([`EditKind::Color`]).
pub(crate) const COLOR_KEYS: &[&str] = &[
    EDIT_FOREGROUND,
    EDIT_BACKGROUND,
    EDIT_CURSOR_COLOR,
    EDIT_SELECTION_COLOR,
    EDIT_SELECTION_FOREGROUND,
    EDIT_CURSOR_TRAIL_COLOR,
    EDIT_CURSOR_TRAIL_ACCENT,
];

pub(crate) const EDIT_OPTION_AS_META: &str = "option_as_meta";
pub(crate) const EDIT_CONFIRM_MULTILINE_PASTE: &str = "confirm_multiline_paste";
pub(crate) const EDIT_COLUMNS: &str = "columns";
pub(crate) const EDIT_LINES: &str = "lines";
pub(crate) const EDIT_TAB_STRIP_ROWS: &str = "tab_strip_rows";
/// The selected-tab color override (`active_tab_color`, `#RRGGBB`).
pub(crate) const EDIT_ACTIVE_TAB_COLOR: &str = "active_tab_color";
/// The chrome accent, cleared = the theme's, else derived from its cursor or ANSI blue/cyan.
pub(crate) const EDIT_ACCENT_COLOR: &str = "accent_color";
/// Split dividers and chrome seams, cleared = the theme's, else the background stepped toward the foreground.
pub(crate) const EDIT_BORDER_COLOR: &str = "border_color";
/// Find's highlight on the other matches, cleared = the theme's, else a tint of the background toward the accent.
pub(crate) const EDIT_SEARCH_MATCH_COLOR: &str = "search_match_color";

/// The `display_font` Enum options.
pub(crate) const DISPLAY_FONT_OPTIONS: &[&str] = &DISPLAY_FONT_IDS;
const DISPLAY_FONT_IDS: [&str; aterm_render::DISPLAY_FACES.len() + 1] = {
    let mut out = [""; aterm_render::DISPLAY_FACES.len() + 1];
    out[0] = "off";
    let mut i = 0;
    while i < aterm_render::DISPLAY_FACES.len() {
        out[i + 1] = aterm_render::DISPLAY_FACES[i].id;
        i += 1;
    }
    out
};

/// Whether one `display_font` id names a current registry face.
#[must_use]
pub(crate) fn display_font_id_is_accepted(id: &str) -> bool {
    aterm_render::display_face_canonical_id(id).is_some()
}
/// Tab Subject & Status.
pub(crate) const EDIT_TAB_STATUS: &str = "tab_status";
pub(crate) const EDIT_TAB_STATUS_QUIET_AFTER_MS: &str = "tab_status_quiet_after_ms";
pub(crate) const EDIT_TAB_STATUS_DWELL_MS: &str = "tab_status_dwell_ms";
pub(crate) const EDIT_TAB_STATUS_BADGE: &str = "tab_status_badge";
/// The tab connection mark's opt-out (Session Connections).
pub(crate) const EDIT_TAB_CONNECTION_BADGE: &str = "tab_connection_badge";
pub(crate) const EDIT_SEARCH_HISTORY_LINES: &str = "search_history_lines";
pub(crate) const EDIT_ALLOW_OSC52_QUERY: &str = "allow_osc52_query";
/// macOS Secure Keyboard Entry (`Config::secure_keyboard_entry`, default OFF).
pub(crate) const EDIT_SECURE_KEYBOARD_ENTRY: &str = "secure_keyboard_entry";
pub(crate) const EDIT_ALLOW_WINDOW_OPS: &str = "allow_window_ops";
pub(crate) const EDIT_ALLOW_NOTIFICATIONS: &str = "allow_notifications";
pub(crate) const EDIT_ALLOW_PALETTE_RECONFIGURE: &str = "allow_palette_reconfigure";
pub(crate) const EDIT_ALLOW_KITTY_FILE_TRANSFER: &str = "allow_kitty_file_transfer";
/// The subtle top-right build/version badge switch (`Config::show_build_badge`).
pub(crate) const EDIT_SHOW_BUILD_BADGE: &str = "show_build_badge";

/// The PHOSPHOR matrix-rain master switch, `[matrix_rain] enabled`.
pub(crate) const EDIT_MATRIX_RAIN_ENABLED: &str = "matrix_rain.enabled";

/// The `[packages]` toolchain-manager switches, as dotted keys (the nested writer [`EDIT_MATRIX_RAIN_ENABLED`] uses).
pub(crate) const EDIT_PACKAGES_ENABLED: &str = "packages.enabled";
/// `[packages] auto_install`.
pub(crate) const EDIT_PACKAGES_AUTO_INSTALL: &str = "packages.auto_install";
/// The retired `[packages]` spellings and the key that replaced each.
pub(crate) const RETIRED_PACKAGES_SPELLINGS: &[(&str, &str)] = &[
    (RETIRED_PACKAGES_AUTO_UPDATE, EDIT_PACKAGES_ENABLED),
    (RETIRED_PACKAGES_SEED_INSTALL, EDIT_PACKAGES_AUTO_INSTALL),
];
/// Every retired key Settings ▸ Modified names as retired, with the setting it folded into.
pub(crate) const RETIRED_SPELLINGS: &[(&str, &str)] = &[
    (RETIRED_PACKAGES_AUTO_UPDATE, EDIT_PACKAGES_ENABLED),
    (RETIRED_PACKAGES_SEED_INSTALL, EDIT_PACKAGES_AUTO_INSTALL),
    (RETIRED_CURSOR_TRAIL, EDIT_CURSOR_TRAIL_STYLE),
];
/// The retired `[packages] auto_update`.
pub(crate) const RETIRED_PACKAGES_AUTO_UPDATE: &str = "packages.auto_update";
/// The retired `[packages] seed_install`.
pub(crate) const RETIRED_PACKAGES_SEED_INSTALL: &str = "packages.seed_install";
/// `[update] enabled`.
pub(crate) const EDIT_UPDATE_ENABLED: &str = "update.enabled";
/// The name of [`EDIT_UPDATE_ENABLED`]'s switch.
pub(crate) const UPDATE_CHECK_LABEL: &str = aterm_update_core::settings::CHECK_LABEL;
/// The name of `[update] auto_apply`'s switch, as [`UPDATE_CHECK_LABEL`] is the check switch's.
pub(crate) const UPDATE_INSTALL_LABEL: &str = aterm_update_core::settings::INSTALL_LABEL;
/// `[reroute] announce`.
pub(crate) const EDIT_REROUTE_ANNOUNCE: &str = "reroute.announce";
/// The aterm harness's durable master switch.
pub(crate) const EDIT_HARNESS_ENABLED: &str = "harness.enabled";
/// `[harness] autopilot`.
pub(crate) const EDIT_HARNESS_AUTOPILOT: &str = "harness.autopilot";
/// `[harness] land_at`.
pub(crate) const EDIT_HARNESS_LAND_AT: &str = "harness.land_at";
/// [`EDIT_HARNESS_AUTOPILOT`]'s options.
pub(crate) const HARNESS_AUTOPILOT_WORDS: [&str; 3] = {
    use aterm_agent::supervise::Autopilot;
    let [a, b, c] = Autopilot::ALL;
    [a.word(), b.word(), c.word()]
};
/// `[harness] approve`.
pub(crate) const EDIT_HARNESS_APPROVE: &str = "harness.approve";
/// `[harness] answer_questions`.
pub(crate) const EDIT_HARNESS_ANSWER_QUESTIONS: &str = "harness.answer_questions";
/// `[harness] question_priority`.
pub(crate) const EDIT_HARNESS_QUESTION_PRIORITY: &str = "harness.question_priority";
/// `[harness.claude] enabled`.
pub(crate) const EDIT_HARNESS_CLAUDE_ENABLED: &str = "harness.claude.enabled";
/// Compatibility policy key allowing codex-alab's native Autopilot.
pub(crate) const EDIT_HARNESS_CODEX_ENABLED: &str = "harness.codex.enabled";
/// `[harness.kimi] enabled`.
pub(crate) const EDIT_HARNESS_KIMI_ENABLED: &str = "harness.kimi.enabled";

/// The label of Codex's own switch ([`EDIT_HARNESS_CODEX_ENABLED`]).
pub(crate) const CODEX_SWITCH_LABEL: &str = "Enable codex-alab Autopilot";

/// The label of the switch of the agent named `product` (`Program::product`).
#[must_use]
pub(crate) fn agent_switch_label(product: &str) -> String {
    use aterm_phase::Program;
    match Program::ALL.into_iter().find(|p| p.product() == product) {
        Some(Program::Codex) => CODEX_SWITCH_LABEL.to_string(),
        Some(
            Program::Claude
            | Program::Agy
            | Program::Muse
            | Program::Grok
            | Program::Kimi
            | Program::Generic,
        )
        | None => format!("Supervise {product} sessions"),
    }
}
/// `[harness.agy] enabled` — [`EDIT_HARNESS_CLAUDE_ENABLED`] for agy.
pub(crate) const EDIT_HARNESS_AGY_ENABLED: &str = "harness.agy.enabled";
/// `[harness.muse] enabled` — [`EDIT_HARNESS_CLAUDE_ENABLED`] for Muse Code.
pub(crate) const EDIT_HARNESS_MUSE_ENABLED: &str = "harness.muse.enabled";

/// The agent a `harness.<agent>.enabled` row switches, read from its key.
pub(crate) fn agent_switch(key: &str) -> Option<Program> {
    let word = key.strip_prefix("harness.")?.strip_suffix(".enabled")?;
    aterm_phase::program_of(word).filter(|p| p.harness_table() && p.name() == word)
}

/// The names a row is searched by as its own, beside its label and key (`native_config_language::query_match_rank`).
pub(crate) fn own_names_of(key: &str) -> [Option<&'static str>; 2] {
    if let Some(agent) = agent_switch(key) {
        [Some(agent.name()), Some(folded_product(agent.product()))]
    } else if let Some(cli) = ai_use_switch(key) {
        [Some(cli.id), Some(folded_product(cli.product))]
    } else {
        [None, None]
    }
}

fn folded_product(product: &'static str) -> &'static str {
    static FOLDED: std::sync::OnceLock<Vec<(&'static str, String)>> = std::sync::OnceLock::new();
    FOLDED
        .get_or_init(|| {
            Program::ALL
                .into_iter()
                .map(Program::product)
                .chain(
                    aterm_agent::ai::connector::ROWS
                        .iter()
                        .map(|cli| cli.product),
                )
                .map(|product| (product, product.to_ascii_lowercase()))
                .collect()
        })
        .iter()
        .find(|(name, _)| *name == product)
        .map_or(product, |(_, folded)| folded.as_str())
}

/// `[ai] enabled`.
pub(crate) const EDIT_AI_ENABLED: &str = "ai.enabled";
/// `[ai] preferred`.
pub(crate) const EDIT_AI_PREFERRED: &str = "ai.preferred";
/// `[ai] reviews`.
pub(crate) const EDIT_AI_REVIEWS: &str = "ai.reviews";
/// `[ai] review_permissions`.
pub(crate) const EDIT_AI_REVIEW_PERMISSIONS: &str = "ai.review_permissions";
/// `[ai] machine`.
pub(crate) const EDIT_AI_MACHINE: &str = "ai.machine";
/// `[ai] fleet`.
pub(crate) const EDIT_AI_FLEET: &str = "ai.fleet";
/// `[ai] fleet_key`.
pub(crate) const EDIT_AI_FLEET_KEY: &str = "ai.fleet_key";

/// The AI CLI an `ai.<id>.use` row switches, read from its key.
pub(crate) fn ai_use_switch(key: &str) -> Option<&'static aterm_agent::ai::connector::Connector> {
    let id = key.strip_prefix("ai.")?.strip_suffix(".use")?;
    aterm_agent::ai::connector::connector(id)
}

/// `[machine] universal_control`.
pub(crate) const EDIT_MACHINE_UNIVERSAL_CONTROL: &str = "machine.universal_control";

/// The permission toggles.
pub(crate) const SECURITY_BOOL_KEYS: &[&str] = &[
    EDIT_ALLOW_OSC52_QUERY,
    EDIT_ALLOW_WINDOW_OPS,
    EDIT_ALLOW_NOTIFICATIONS,
    EDIT_ALLOW_PALETTE_RECONFIGURE,
    EDIT_ALLOW_KITTY_FILE_TRANSFER,
    EDIT_SECURE_KEYBOARD_ENTRY,
];

pub(crate) const EDIT_WINDOW_THEME: &str = "window_theme";
pub(crate) const EDIT_BIDI: &str = "bidi";
pub(crate) const EDIT_AMBIGUOUS_WIDTH: &str = "ambiguous_width";

pub(crate) const CURSOR_STYLES: &[&str] = &["block", "bar"];
/// `[machine] universal_control` — the two spellings `atpkg::config` resolves.
pub(crate) const UNIVERSAL_CONTROL_POLICIES: &[&str] = &["off", "leave"];
pub(crate) const WINDOW_THEMES: &[&str] = &["auto", "light", "dark"];
/// `palette_generate`.
pub(crate) const PALETTE_GENERATE_MODES: &[&str] = &["theme", "always", "never"];
pub(crate) const BIDI_MODES: &[&str] = &["implicit", "disabled", "explicit"];
pub(crate) const AMBIGUOUS_WIDTHS: &[&str] = &["narrow", "wide"];
/// Closed domain for `predictive_echo` (`PredictMode::parse` also accepts aliases like auto/on/force).
pub(crate) const PREDICTIVE_ECHO_MODES: &[&str] = &["off", "adaptive", "always"];
/// Closed domain for `text_blending` (the anti-aliasing weight).
pub(crate) const TEXT_BLENDINGS: &[&str] = &["linear-corrected", "linear"];
/// Closed domain for the `motion` accessibility policy.
pub(crate) const MOTION_MODES: &[&str] = &["auto", "full", "reduced"];

/// The Motion row's value for a written `motion`: a spelling the engine reads as Full or
/// Reduced (`reduce`, `none`, `off`; `MotionMode::parse`) is that mode's row, so the row shows
/// it as the engine plays it. Anything else stays as written (an unknown spelling is shown as
/// itself, the current value outside the domain).
pub(crate) fn motion_row_value(value: &str) -> &str {
    use crate::motion::MotionMode;
    let token = value.trim();
    if token.eq_ignore_ascii_case("auto") {
        return value;
    }
    match MotionMode::parse(token) {
        MotionMode::Full => "full",
        MotionMode::Reduced => "reduced",
        MotionMode::Auto => value,
    }
}
/// Closed domain for `window_colorspace`.
pub(crate) const WINDOW_COLORSPACES: &[&str] = &["srgb", "display-p3"];
/// Closed domain for `background_material` (vibrancy).
pub(crate) const BACKGROUND_MATERIALS: &[&str] = &["none", "hud", "sidebar", "under-window"];
/// Closed domain for `sparkle_words.profanity.style`.
pub(crate) const SPARKLE_PROFANITY_STYLES: &[&str] = &["rainbow", "nova", "sparkle"];
/// Closed domain for `sparkle_words.feline.style` (unknown → `cat`).
pub(crate) const SPARKLE_FELINE_STYLES: &[&str] = &["cat", "paw"];
pub(crate) const EDIT_PREDICTIVE_ECHO: &str = "predictive_echo";
/// Predictive editing (`Config::predictive_echo_editing`, default on).
pub(crate) const EDIT_PREDICTIVE_ECHO_EDITING: &str = "predictive_echo_editing";
/// Focus-linked shell priority boost (Windows QoS; no-op elsewhere).
pub(crate) const EDIT_FOCUS_BOOST: &str = "focus_boost";

/// GPU rendering master switch (`Config::gpu`, default ON with CPU fallback).
pub(crate) const EDIT_GPU: &str = "gpu";
/// Indexed ANSI palette (`Config::palette`, `Vec<String>` of `#RRGGBB` by 0-based index).
pub(crate) const EDIT_PALETTE: &str = "palette";
/// Sparse indexed palette slots (`Config::palette_overrides`, a table of `#RRGGBB` by slot index applied over `palette`).
pub(crate) const EDIT_PALETTE_OVERRIDES: &str = "palette_overrides";
/// Trail sound level 0..=1 (`Config::trail_sound_volume`, default 0.4).
pub(crate) const EDIT_TRAIL_SOUND_VOLUME: &str = "trail_sound_volume";
/// Interactive shell program (`Config::shell`; discovery-resolved name or path).
pub(crate) const EDIT_SHELL: &str = "shell";
/// Extra shell argv (`Config::shell_args`) — a [`LIST_KEYS`] row.
pub(crate) const EDIT_SHELL_ARGS: &str = "shell_args";
/// amacs as the default `EDITOR` in tabs (`[amacs] default_editor`, default on), read by
/// `amacs::config` (docs/DESIGN-amacs-2026-10-19.md §8.4).
pub(crate) const EDIT_AMACS_DEFAULT_EDITOR: &str = "amacs.default_editor";
/// GPU-present colour-space tag (`window_colorspace`: srgb | display-p3).
pub(crate) const EDIT_WINDOW_COLORSPACE: &str = "window_colorspace";
/// Session restore at launch (`restore_session`, default ON).
pub(crate) const EDIT_RESTORE_SESSION: &str = "restore_session";
/// Opt-in shell custody, read at launch.
pub(crate) const EDIT_KEEPER_ENABLED: &str = "keeper.enabled";
/// Session activity on disk (on by default), read at launch.
pub(crate) const EDIT_SESSION_LOG_JOURNAL: &str = "session_log.journal";
/// Live activity in terminal tab tooltips and menus (on by default).
pub(crate) const EDIT_SESSION_LOG_CLAUSE: &str = "session_log.clause";
/// OpenType feature requests (`Config::font_features`) — a [`LIST_KEYS`] row.
pub(crate) const EDIT_FONT_FEATURES: &str = "font_features";
/// Extra `wght` on dark themes (`font_weight_dark_nudge`, clamp 0..=300).
pub(crate) const EDIT_FONT_WEIGHT_DARK_NUDGE: &str = "font_weight_dark_nudge";
/// Window glass opacity (`background_opacity`, default 1.0 solid, clamp 0..=1).
pub(crate) const EDIT_BACKGROUND_OPACITY: &str = "background_opacity";
/// macOS vibrancy material (`background_material`).
pub(crate) const EDIT_BACKGROUND_MATERIAL: &str = "background_material";
/// Terminal-tab backdrop image path (`wallpaper`).
pub(crate) const EDIT_WALLPAPER: &str = "wallpaper";
/// Wallpaper legibility dim (`wallpaper_dim`, default 0.3, clamp 0..=1).
pub(crate) const EDIT_WALLPAPER_DIM: &str = "wallpaper_dim";
/// Backdrop-hue glyph tint while a wallpaper is attached.
pub(crate) const EDIT_WALLPAPER_TEXT_TINT: &str = "wallpaper_text_tint";
/// Per-session temporal recorder (`temporal_recording`, default [`crate::app_config::DEFAULT_TEMPORAL_RECORDING`]).
pub(crate) const EDIT_TEMPORAL_RECORDING: &str = "temporal_recording";
/// All-edge interior window padding in logical px (`window_padding`, default 12, clamp 0..=64).
pub(crate) const EDIT_WINDOW_PADDING: &str = "window_padding";
/// Top-edge padding override in logical px.
pub(crate) const EDIT_WINDOW_PADDING_TOP: &str = "window_padding_top";

/// The list-valued keys the compatibility `settings set` writer accepts.
pub(crate) const LIST_KEYS: &[&str] = &[EDIT_PALETTE, EDIT_SHELL_ARGS, EDIT_FONT_FEATURES];

/// Collection-shaped values that have no lossless one-line Settings control.
pub(crate) fn manual_collection_key(key: &str) -> bool {
    LIST_KEYS.contains(&key) || matches!(key, EDIT_FALLBACK_FONTS | EDIT_FONT_VARIATION)
}

/// Config keys whose value names filesystem-backed assets.
pub(crate) fn config_asset_source_key(key: &str) -> bool {
    matches!(key, EDIT_CURSOR_NYAN_SPRITE)
}

/// Values edited only in Manual.
pub(crate) fn manual_only_key(key: &str) -> bool {
    manual_collection_key(key)
        || config_asset_source_key(key)
        || key == "sparkle_words.feline.style"
}

/// One scalar leaf of a nested config table (`[update]` / `[sparkle_words]` (+ sub-tables) / `[matrix_rain]`).
pub(crate) struct NestedLeaf {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) kind: EditKind,
}

/// Every registered nested scalar leaf.
pub(crate) const NESTED_LEAVES: &[NestedLeaf] = &[
    NestedLeaf {
        key: EDIT_AMACS_DEFAULT_EDITOR,
        label: "Use amacs as the default editor (EDITOR)",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_KEEPER_ENABLED,
        label: "Keep shells running after an aterm crash",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_SESSION_LOG_JOURNAL,
        label: "Save session activity across launches",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_SESSION_LOG_CLAUSE,
        label: "Show activity in tab tooltips and menus",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_UPDATE_ENABLED,
        label: UPDATE_CHECK_LABEL,
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "update.auto_apply",
        label: UPDATE_INSTALL_LABEL,
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_REROUTE_ANNOUNCE,
        label: "Note when cargo or rustc is rerouted",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_HARNESS_ENABLED,
        label: "Supervise coding agents",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_HARNESS_CLAUDE_ENABLED,
        label: "Supervise Claude Code sessions",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_HARNESS_CODEX_ENABLED,
        label: CODEX_SWITCH_LABEL,
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_HARNESS_AGY_ENABLED,
        label: "Supervise Antigravity CLI sessions",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_HARNESS_MUSE_ENABLED,
        label: "Supervise Muse Code sessions",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_HARNESS_KIMI_ENABLED,
        label: "Supervise Kimi Code sessions",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_HARNESS_AUTOPILOT,
        label: "Default autopilot",
        kind: EditKind::Enum {
            options: &HARNESS_AUTOPILOT_WORDS,
        },
    },
    NestedLeaf {
        key: EDIT_HARNESS_LAND_AT,
        label: "Land at",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: EDIT_HARNESS_APPROVE,
        label: "Approve permission boxes",
        kind: EditKind::Enum {
            options: &["all", "safe", "none"],
        },
    },
    NestedLeaf {
        key: EDIT_HARNESS_ANSWER_QUESTIONS,
        label: "Answer questions",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_HARNESS_QUESTION_PRIORITY,
        label: "Question priority",
        kind: EditKind::Enum {
            options: &aterm_agent::supervise::PriorityWord::WORDS,
        },
    },
    NestedLeaf {
        key: EDIT_AI_PREFERRED,
        label: "Preferred AI CLI",
        kind: EditKind::Enum {
            options: &aterm_agent::ai::connector::IDS,
        },
    },
    NestedLeaf {
        key: EDIT_AI_ENABLED,
        label: "Look for AI CLI problems",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_AI_REVIEWS,
        label: "Reviews across AIs",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_AI_REVIEW_PERMISSIONS,
        label: "Reviewer permissions",
        kind: EditKind::Enum {
            options: &aterm_agent::ai::permission::CHOICE_WORDS,
        },
    },
    NestedLeaf {
        key: EDIT_AI_MACHINE,
        label: "This machine's name in the fleet",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: EDIT_AI_FLEET,
        label: "Fleet document (path or https:// URL)",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: EDIT_AI_FLEET_KEY,
        label: "Fleet signer's public key",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: "ai.claude.use",
        label: "Use Claude Code on this machine",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "ai.codex.use",
        label: "Use Codex CLI on this machine",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "ai.agy.use",
        label: "Use Antigravity CLI on this machine",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "ai.muse.use",
        label: "Use Muse Code on this machine",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "ai.grok.use",
        label: "Use Grok Build on this machine",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "ai.kimi.use",
        label: "Use Kimi Code CLI on this machine",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.enabled",
        label: "All word effects",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.reduced_motion",
        label: "Static sparkles (no twinkle)",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.suppress_in_alt_screen",
        label: "Hide sparkles in full-screen apps",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.lexicon",
        label: "Extra word list (path)",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.enabled",
        label: "Sparkle words",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.style",
        label: "Profanity style",
        kind: EditKind::Enum {
            options: SPARKLE_PROFANITY_STYLES,
        },
    },
    NestedLeaf {
        key: "sparkle_words.profanity.supernova_chance",
        label: "Profanity supernova chance",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.magic",
        label: "Rare sparkle variants",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.density",
        label: "Profanity spark density",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.anim_ms",
        label: "Profanity sparkle duration",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.jitter",
        label: "Profanity jitter",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.intensity",
        label: "Profanity opacity",
        kind: EditKind::Float,
    },
    NestedLeaf {
        key: EDIT_SPARKLE_BONK,
        label: "Bonk when you type a swear word",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_SPARKLE_BONK_DETONATION,
        label: "Bonk when a sparkle word bursts",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.enabled",
        label: "Keyword kitties",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.style",
        label: "Keyword kitty look",
        kind: EditKind::Enum {
            options: SPARKLE_FELINE_STYLES,
        },
    },
    NestedLeaf {
        key: "sparkle_words.feline.magic",
        label: "Rare kitty variants",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.allow_bare_cat",
        label: "Decorate a bare \"cat\"",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.cjk_single_char",
        label: "Decorate a lone CJK cat character",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.log",
        label: "Record the Kitty Log",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.ink.enabled",
        label: "Ink shimmer",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.ink.strength",
        label: "Ink tint strength",
        kind: EditKind::Float,
    },
    NestedLeaf {
        key: "sparkle_words.ink.sweep_ms",
        label: "Ink sweep window",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "sparkle_words.ink.loop",
        label: "Ink re-sweep while visible",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.emphasis.enabled",
        label: "Emphasis ink class",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.enabled",
        label: "Matrix rain",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.fps",
        label: "Rain working fps",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.density",
        label: "Rain column density",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.speed",
        label: "Rain fall speed",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.trail",
        label: "Rain trail length",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.alpha",
        label: "Rain body coverage",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.head_alpha",
        label: "Rain head coverage",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.hue",
        label: "Rain hue",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: "matrix_rain.mutation_ms",
        label: "Rain glyph mutation",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.idle_secs",
        label: "Rain idle drain",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.suppress_in_alt_screen",
        label: "Hide rain in full-screen apps",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.output_material",
        label: "Rain output-material bank",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.turn_wave",
        label: "Rain turn-complete wave",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.bell_alert",
        label: "Rain bell alert",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.exit_tint",
        label: "Rain exit-status tint",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.seed",
        label: "Rain field seed",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: EDIT_OUTPUT_STREAK_ENABLED,
        label: "Output streak",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_OUTPUT_STREAK_SOUND,
        label: "Note when a program starts printing",
        kind: EditKind::Bool,
    },
];

/// Registry lookup for a dotted nested key.
pub(crate) fn nested_leaf(key: &str) -> Option<&'static NestedLeaf> {
    NESTED_LEAVES.iter().find(|l| l.key == key)
}

/// Every setting whose uncommitted value must have a renderer-native visual projection.
pub(crate) const VISUAL_PREVIEW_KEYS: &[&str] = &[
    EDIT_THEME,
    EDIT_FOREGROUND,
    EDIT_BACKGROUND,
    EDIT_CURSOR_COLOR,
    EDIT_SELECTION_COLOR,
    EDIT_SELECTION_FOREGROUND,
    EDIT_WINDOW_THEME,
    EDIT_MINIMUM_CONTRAST,
    EDIT_SELECTION_INACTIVE,
    EDIT_BOLD_IS_BRIGHT,
    EDIT_FAINT_OPACITY,
    EDIT_FONT_FAMILY,
    EDIT_FONT_PX,
    EDIT_FONT_FAMILY_BOLD,
    EDIT_FONT_FAMILY_ITALIC,
    EDIT_FONT_FAMILY_BOLD_ITALIC,
    EDIT_FONT_SYNTHETIC_STYLE,
    EDIT_FALLBACK_FONTS,
    EDIT_SYMBOL_FONT,
    EDIT_EMOJI_FONT,
    EDIT_LIGATURES,
    EDIT_LINE_HEIGHT,
    EDIT_FONT_SIZE_SCALE,
    EDIT_ADJUST_BASELINE,
    EDIT_ADJUST_UNDERLINE_POSITION,
    EDIT_ADJUST_UNDERLINE_THICKNESS,
    EDIT_UNDERLINE_SKIP_DESCENDERS,
    EDIT_CURSOR_BREAK_LIGATURES,
    EDIT_MERGED_LIGATURES,
    EDIT_TEXT_BLENDING,
    EDIT_FONT_THICKEN,
    EDIT_STEM_GAMMA,
    EDIT_FONT_WEIGHT,
    EDIT_FONT_VARIATION,
    EDIT_CURSOR_TRAIL_STYLE,
    EDIT_CURSOR_PET,
    EDIT_CURSOR_NYAN_SPRITE,
    EDIT_CURSOR_STYLE,
    EDIT_CURSOR_BLINK,
    EDIT_CURSOR_MOMENTUM_GLOW,
    EDIT_CURSOR_TRAIL_MS,
    EDIT_CURSOR_TRAIL_LENGTH,
    EDIT_CURSOR_TRAIL_INTENSITY,
    EDIT_CURSOR_TRAIL_RADIUS,
    EDIT_CURSOR_TRAIL_RING,
    EDIT_CURSOR_TRAIL_COLOR,
    EDIT_CURSOR_TRAIL_ACCENT,
    EDIT_CURSOR_TRAIL_BLOOM,
    EDIT_CURSOR_TRAIL_BLOOM_STRENGTH,
    EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
    EDIT_CURSOR_FIRE_SHIMMER,
    EDIT_HDR_GLOW,
    EDIT_CURSOR_GLOW_SDR_BOOST,
    EDIT_MOTION,
    EDIT_LOAD_ADAPTIVE_MOTION,
];

const DEFAULT_SCROLLBACK_LINES: usize = 100_000;

const DEFAULT_CURSOR_STYLE: &str = "block";

/// The default `cursor_trail_style` when unset.
pub(crate) const DEFAULT_CURSOR_TRAIL_STYLE: &str = "rainbow";

/// Every `cursor_pet`, in the order Settings lists them.
pub(crate) const CURSOR_PETS: &[&str] = &aterm_effects::cursor_pet::PET_NAMES;

/// The default `cursor_pet` when unset, where decorative effects are on by default.
pub(crate) const DEFAULT_CURSOR_PET: &str = aterm_effects::cursor_pet::DEFAULT_PET;

/// Every canonical `cursor_trail_style`, in the order the Settings Cursor Styles page and the Cursor & Motion picker list them.
pub(crate) const CURSOR_TRAIL_STYLES: &[&str] = &spellings::TRAIL_STYLE_NAMES;

/// `trail_sound_style` options, the typing-sound picker (Settings ▸ Sound ▸ "Typing sound").
pub(crate) const TRAIL_SOUND_STYLES: &[&str] = &[
    SoundVoice::Style.name(),
    SoundVoice::RainbowKittyV2.name(),
    SoundVoice::Of(GlowStyle::Lumen).name(),
    SoundVoice::Of(GlowStyle::Sparkle).name(),
    SoundVoice::Of(GlowStyle::Comet).name(),
    SoundVoice::Of(GlowStyle::Water).name(),
    SoundVoice::Of(GlowStyle::Phaser).name(),
    SoundVoice::Of(GlowStyle::Laser).name(),
    SoundVoice::Of(GlowStyle::Beam).name(),
    SoundVoice::Of(GlowStyle::Fire).name(),
    SoundVoice::Mech.name(),
    SoundVoice::Typewriter.name(),
    SoundVoice::Marimba.name(),
    SoundVoice::Felt.name(),
];

/// Default `trail_sound_style` (the `auto` follow-the-trail identity).
pub(crate) const DEFAULT_TRAIL_SOUND_STYLE: &str = SoundVoice::Style.name();

/// Resolve a `trail_sound_style` token or documented alias to its canonical name.
pub(crate) fn trail_sound_style_canonical(token: &str) -> Option<&'static str> {
    SoundVoice::parse(token).map(SoundVoice::name)
}

/// Resolve a `cursor_trail_style` spelling (a row's name, trimmed, case-insensitive) to its name. A retired spelling ([`retired_cursor_name`]) resolves to none.
pub(crate) fn cursor_trail_style_canonical(token: &str) -> Option<&'static str> {
    spellings::canonical(token)
}

/// The settings a retired `key` value `was` is written as now, as an error or a record says them.
pub(crate) fn retired_cursor_name_now(key: &str, was: &spellings::RetiredTrailStyle) -> String {
    match was.pet {
        Some(pet) => format!("{key} = {:?} and {EDIT_CURSOR_PET} = {pet:?}", was.style),
        None => format!("{key} = {:?}", was.style),
    }
}

/// One retired cursor style name an aterm.toml held, and what [`migrate_cursor_settings`]
/// wrote in its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetiredCursorName {
    /// The key that held it.
    pub(crate) key: &'static str,
    /// The retired name, as the file spelled it.
    pub(crate) old: String,
    /// The style written in its place.
    pub(crate) style: &'static str,
}

impl RetiredCursorName {
    /// The change in one line, as the record of it says.
    pub(crate) fn sentence(&self) -> String {
        let Self { key, old, style } = self;
        format!("{key} {old:?} is now {style:?}")
    }
}

/// What the one-time migration of an aterm.toml's cursor settings changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CursorMigration {
    /// The file as migrated.
    pub(crate) text: String,
    /// Each retired style name it held, renamed.
    pub(crate) renamed: Vec<RetiredCursorName>,
    /// The `cursor_pet` written, for a file that named none: the pet its cursor had before
    /// the pet was a setting.
    pub(crate) pet: Option<&'static str>,
}

impl CursorMigration {
    /// Each change in one line, as the record of the migration says them.
    pub(crate) fn sentences(&self) -> Vec<String> {
        let mut said: Vec<String> = self
            .renamed
            .iter()
            .map(RetiredCursorName::sentence)
            .collect();
        match self.pet {
            Some("off") => said.push(format!(
                "{EDIT_CURSOR_PET} is now its own setting, written \"off\": this cursor had no pet"
            )),
            Some(pet) => said.push(format!(
                "{EDIT_CURSOR_PET} is now its own setting, written {pet:?}"
            )),
            None => {}
        }
        said
    }
}

/// The pet a file's `cursor_trail_style` drew before the pet was its own setting. An animal's
/// name drew its animal and a flying name the flying cat; `off` and every other style drew
/// none; a name that was no style drew the default style, which had the cat.
fn pet_a_style_drew(style: &str) -> &'static str {
    if let Some(was) = spellings::retired(style) {
        return was.pet.unwrap_or("off");
    }
    if spellings::canonical(style).is_some() {
        "off"
    } else {
        DEFAULT_CURSOR_PET
    }
}

/// THE ONE-TIME MIGRATION OF AN ATERM.TOML'S CURSOR SETTINGS, from the style list that named
/// trail, animal and flight in one word to one style and the pet beside it. The caller runs it
/// once per file and records that it ran (`native_config_service::CursorMigrationStamp`);
/// nothing here can tell a migrated file from one written afterwards. `None` for a file it
/// changes nothing in, and for one that does not parse (its reader reports that).
///
/// - Each retired name under `cursor_trail_style` and `cursor_trail_style_before_off` becomes
///   the style it became. The old-name map is [`spellings::RETIRED_TRAIL_STYLES`], and this is
///   its only reader that accepts a name: [`typed_item`] refuses one and no style resolves
///   from one.
/// - A file that names no `cursor_pet` gets the pet its cursor had, so nobody gains or loses
///   an animal by upgrading: the retired `cursor_trail` master off, `off` and every style that
///   was not an animal's write `"off"` ([`pet_a_style_drew`]); the master on with no style
///   drew the default style and its cat on every platform. The style Off keeps
///   (`cursor_trail_style_before_off`) names no pet: the cursor drawn is plain. One style is
///   left to the default: a bare `rainbow`. It was an alias of the flying style, and it is
///   the theme's name now, in files written since; the file's later reader is the likelier.
/// - A file that names neither a style nor the master is left alone: it draws the defaults.
pub(crate) fn migrate_cursor_settings(existing_toml: &str) -> Option<CursorMigration> {
    let mut doc = existing_toml
        .parse::<aterm_toml::edit::DocumentMut>()
        .ok()?;
    let master = doc
        .get(RETIRED_CURSOR_TRAIL)
        .and_then(aterm_toml::edit::Item::as_bool);
    let drawn_style = doc
        .get(EDIT_CURSOR_TRAIL_STYLE)
        .and_then(aterm_toml::edit::Item::as_str)
        .map(str::to_string);
    let mut renamed = Vec::new();
    for key in [EDIT_CURSOR_TRAIL_STYLE, EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF] {
        let Some(old) = doc
            .get(key)
            .and_then(aterm_toml::edit::Item::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(was) = spellings::retired(&old) else {
            continue;
        };
        let mut item = aterm_toml::edit::Item::Value(aterm_toml::edit::Value::from(was.style));
        adopt_inline_decor(doc.get(key), &mut item);
        doc[key] = item;
        renamed.push(RetiredCursorName {
            key,
            old,
            style: was.style,
        });
    }
    let pet = if doc.get(EDIT_CURSOR_PET).is_some() {
        None
    } else {
        match (master, drawn_style.as_deref()) {
            (Some(false), _) => Some("off"),
            (_, Some(style))
                if style
                    .trim()
                    .eq_ignore_ascii_case(DEFAULT_CURSOR_TRAIL_STYLE) =>
            {
                None
            }
            (_, Some(style)) => Some(pet_a_style_drew(style)),
            (Some(true), None) => Some(DEFAULT_CURSOR_PET),
            (None, None) => None,
        }
    };
    if let Some(pet) = pet {
        doc[EDIT_CURSOR_PET] = aterm_toml::edit::Item::Value(aterm_toml::edit::Value::from(pet));
    }
    if renamed.is_empty() && pet.is_none() {
        return None;
    }
    Some(CursorMigration {
        text: doc.to_string(),
        renamed,
        pet,
    })
}

/// How a Preferences key should be typed in the written TOML.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EditKind {
    Float,
    Integer,
    Bool,
    Text,
    Enum { options: &'static [&'static str] },
    Theme,
    Color,
}

/// The TOML type of each editable key, so [`apply_prefs_edits`] can parse the raw control text into a correctly-typed `aterm-toml` value.
pub(crate) fn edit_kind(key: &str) -> EditKind {
    if COLOR_KEYS.contains(&key) {
        return EditKind::Color;
    }
    if key
        .strip_prefix(EDIT_PALETTE_OVERRIDES)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(crate::resolved_colors::palette_override_index)
        .is_some()
    {
        return EditKind::Color;
    }
    if SECURITY_BOOL_KEYS.contains(&key) {
        return EditKind::Bool;
    }
    if let Some(leaf) = nested_leaf(key) {
        return leaf.kind;
    }
    if LIST_KEYS.contains(&key) {
        return EditKind::Text;
    }
    match key {
        EDIT_THEME => EditKind::Theme,
        EDIT_CURSOR_STYLE => EditKind::Enum {
            options: CURSOR_STYLES,
        },
        EDIT_WINDOW_THEME => EditKind::Enum {
            options: WINDOW_THEMES,
        },
        EDIT_PALETTE_GENERATE => EditKind::Enum {
            options: PALETTE_GENERATE_MODES,
        },
        EDIT_BIDI => EditKind::Enum {
            options: BIDI_MODES,
        },
        EDIT_PREDICTIVE_ECHO => EditKind::Enum {
            options: PREDICTIVE_ECHO_MODES,
        },
        EDIT_AMBIGUOUS_WIDTH => EditKind::Enum {
            options: AMBIGUOUS_WIDTHS,
        },
        EDIT_TEXT_BLENDING => EditKind::Enum {
            options: TEXT_BLENDINGS,
        },
        EDIT_MOTION => EditKind::Enum {
            options: MOTION_MODES,
        },
        EDIT_WINDOW_COLORSPACE => EditKind::Enum {
            options: WINDOW_COLORSPACES,
        },
        EDIT_BACKGROUND_MATERIAL => EditKind::Enum {
            options: BACKGROUND_MATERIALS,
        },
        EDIT_CURSOR_TRAIL_COLOR
        | EDIT_CURSOR_TRAIL_ACCENT
        | EDIT_ACTIVE_TAB_COLOR
        | EDIT_ACCENT_COLOR
        | EDIT_BORDER_COLOR
        | EDIT_SEARCH_MATCH_COLOR => EditKind::Color,
        EDIT_DISPLAY_FONT => EditKind::Enum {
            options: DISPLAY_FONT_OPTIONS,
        },
        EDIT_FONT_PX
        | EDIT_LINE_HEIGHT
        | EDIT_FONT_SIZE_SCALE
        | EDIT_MINIMUM_CONTRAST
        | EDIT_FAINT_OPACITY
        | EDIT_STEM_GAMMA
        | EDIT_TRAIL_SOUND_VOLUME
        | EDIT_CURSOR_TRAIL_INTENSITY
        | EDIT_CURSOR_TRAIL_RADIUS
        | EDIT_CURSOR_TRAIL_BLOOM_STRENGTH
        | EDIT_CURSOR_TRAIL_BLOOM_RADIUS
        | EDIT_CURSOR_GLOW_SDR_BOOST
        | EDIT_FONT_WEIGHT_DARK_NUDGE
        | EDIT_BACKGROUND_OPACITY
        | EDIT_WALLPAPER_DIM
        | EDIT_WINDOW_PADDING
        | EDIT_WINDOW_PADDING_TOP => EditKind::Float,
        EDIT_SCROLLBACK
        | EDIT_CURSOR_TRAIL_MS
        | EDIT_CURSOR_TRAIL_LENGTH
        | EDIT_COLUMNS
        | EDIT_LINES
        | EDIT_TAB_STRIP_ROWS
        | EDIT_TAB_STATUS_QUIET_AFTER_MS
        | EDIT_TAB_STATUS_DWELL_MS
        | EDIT_SEARCH_HISTORY_LINES
        | EDIT_ADJUST_BASELINE
        | EDIT_ADJUST_UNDERLINE_POSITION
        | EDIT_ADJUST_UNDERLINE_THICKNESS
        | EDIT_FONT_WEIGHT => EditKind::Integer,
        EDIT_COPY_ON_SELECT
        | EDIT_LIGATURES
        | EDIT_TRAIL_SOUNDS
        | EDIT_TONE_MELODY
        | EDIT_TRAIL_SOUND_BED
        | EDIT_TRAIL_SOUND_RIFF
        | EDIT_BELL_SOUND
        | EDIT_CURSOR_BLINK
        | EDIT_CURSOR_MOMENTUM_GLOW
        | EDIT_OPTION_AS_META
        | EDIT_CONFIRM_MULTILINE_PASTE
        | EDIT_PREDICTIVE_ECHO_EDITING
        | EDIT_SELECTION_INACTIVE
        | EDIT_SPLIT_FOCUS_MARK
        | EDIT_CURSOR_BREAK_LIGATURES
        | EDIT_MERGED_LIGATURES
        | EDIT_FONT_SYNTHETIC_STYLE
        | EDIT_BOLD_IS_BRIGHT
        | EDIT_FONT_THICKEN
        | EDIT_SHOW_BUILD_BADGE
        | EDIT_TAB_STATUS
        | EDIT_TAB_STATUS_BADGE
        | EDIT_TAB_CONNECTION_BADGE
        | EDIT_SERIOUS_MODE
        | EDIT_LOAD_ADAPTIVE_MOTION
        | EDIT_CURSOR_TRAIL_RING
        | EDIT_CURSOR_TRAIL_BLOOM
        | EDIT_CURSOR_FIRE_SHIMMER
        | EDIT_HDR_GLOW
        | EDIT_UNDERLINE_SKIP_DESCENDERS
        | EDIT_FOCUS_BOOST
        | EDIT_GPU
        | EDIT_RESTORE_SESSION
        | EDIT_TEMPORAL_RECORDING
        | EDIT_PACKAGES_ENABLED
        | EDIT_PACKAGES_AUTO_INSTALL
        | RETIRED_PACKAGES_AUTO_UPDATE
        | RETIRED_PACKAGES_SEED_INSTALL
        | EDIT_WALLPAPER_TEXT_TINT => EditKind::Bool,
        EDIT_CURSOR_TRAIL_STYLE | EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF => EditKind::Enum {
            options: CURSOR_TRAIL_STYLES,
        },
        EDIT_CURSOR_PET => EditKind::Enum {
            options: CURSOR_PETS,
        },
        EDIT_TRAIL_SOUND_STYLE => EditKind::Enum {
            options: TRAIL_SOUND_STYLES,
        },
        EDIT_MACHINE_UNIVERSAL_CONTROL => EditKind::Enum {
            options: UNIVERSAL_CONTROL_POLICIES,
        },
        _ => EditKind::Text,
    }
}

/// How the Settings theme card is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeMode {
    Always,
    FollowSystem,
}

/// The mode a configured `theme` value (`None` when unset) is in, by the one selector grammar ([`aterm_types::scheme::ThemeSelector::parse`]).
pub(crate) fn theme_mode(raw: Option<&str>) -> ThemeMode {
    use aterm_types::scheme::ThemeSelector;
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return ThemeMode::Always;
    };
    // Parsed once: the selector that says whether it names a theme says how.
    match ThemeSelector::parse(raw).0 {
        selector @ ThemeSelector::Split { .. } if !selector_unset(&selector) => {
            ThemeMode::FollowSystem
        }
        _ => ThemeMode::Always,
    }
}

const DEFAULT_THEME_NAME: &str = "Default";

fn theme_pair_seed(name: &str, themes: &crate::app_config::ThemeCatalog) -> (String, String) {
    let meta = themes.meta(name);
    let appearance = meta
        .as_ref()
        .map_or(aterm_types::Appearance::Dark, |meta| meta.appearance);
    let opposite = match appearance {
        aterm_types::Appearance::Dark => aterm_types::Appearance::Light,
        aterm_types::Appearance::Light => aterm_types::Appearance::Dark,
    };
    let other = meta
        .and_then(|meta| meta.pair)
        .unwrap_or_else(|| crate::app_config::default_pair_side(opposite).to_string());
    match appearance {
        aterm_types::Appearance::Dark => (name.to_string(), other),
        aterm_types::Appearance::Light => (other, name.to_string()),
    }
}

/// What the Follow-system card's Dark and Light slots show for a configured value, as `(dark, light)`.
pub(crate) fn theme_slots(
    raw: Option<&str>,
    themes: &crate::app_config::ThemeCatalog,
) -> (String, String) {
    use aterm_types::scheme::ThemeSelector;
    let Some(raw) = raw.map(str::trim).filter(|_| !theme_is_unset(raw)) else {
        return theme_pair_seed(DEFAULT_THEME_NAME, themes);
    };
    match ThemeSelector::parse(raw).0 {
        ThemeSelector::Single(name) => theme_pair_seed(&name, themes),
        selector @ ThemeSelector::Split { .. } => {
            let side = |appearance| {
                selector
                    .name_for(appearance)
                    .unwrap_or(DEFAULT_THEME_NAME)
                    .to_string()
            };
            (
                side(aterm_types::Appearance::Dark),
                side(aterm_types::Appearance::Light),
            )
        }
    }
}

/// The `(dark, light)` themes a Follow-system `theme` value draws, read off the value alone.
pub(crate) fn theme_pair_names(raw: &str) -> Option<(String, String)> {
    use aterm_types::scheme::ThemeSelector;
    let (selector, _) = ThemeSelector::parse(raw.trim());
    if !matches!(selector, ThemeSelector::Split { .. }) {
        return None;
    }
    let side = |appearance| {
        selector
            .name_for(appearance)
            .unwrap_or(DEFAULT_THEME_NAME)
            .to_string()
    };
    Some((
        side(aterm_types::Appearance::Dark),
        side(aterm_types::Appearance::Light),
    ))
}

/// How a Follow-system `theme` value reads on a control.
pub(crate) fn theme_pair_label(raw: &str) -> Option<String> {
    theme_pair_names(raw).map(|(dark, light)| {
        if dark == light {
            dark
        } else {
            format!("{dark} / {light} \u{b7} follows system")
        }
    })
}

/// [`theme_pair_label`] without its tail, for a control too narrow for the whole of it.
pub(crate) fn theme_pair_short_label(raw: &str) -> Option<String> {
    theme_pair_names(raw).map(|(dark, light)| {
        if dark == light {
            dark
        } else {
            format!("{dark} / {light}")
        }
    })
}

/// The `theme` value for a `(dark, light)` pair, in the selector grammar's spelling.
pub(crate) fn theme_split_value(dark: &str, light: &str) -> String {
    format!("dark:{dark},light:{light}")
}

/// What turning Follow system off replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThemeFollowKept {
    pub(crate) always: String,
    pub(crate) pair: String,
}

/// The value turning Follow system on writes.
pub(crate) fn theme_follow_system_value(
    raw: Option<&str>,
    kept: Option<&ThemeFollowKept>,
    themes: &crate::app_config::ThemeCatalog,
) -> String {
    if let Some(raw) = raw
        && theme_mode(Some(raw)) == ThemeMode::FollowSystem
    {
        return raw.to_string();
    }
    // The one theme it wrote may since read as no value at all (Default, which a write removes).
    let still_kept = |always: &str| {
        raw.is_some_and(|raw| raw.trim().eq_ignore_ascii_case(always.trim()))
            || (theme_is_unset(raw) && theme_is_unset(Some(always)))
    };
    if let Some(kept) = kept
        && still_kept(&kept.always)
        && theme_mode(Some(&kept.pair)) == ThemeMode::FollowSystem
    {
        return kept.pair.clone();
    }
    let (dark, light) = theme_slots(raw, themes);
    theme_split_value(&dark, &light)
}

/// The value choosing `name` in the `side` slot writes.
pub(crate) fn theme_with_side(
    raw: Option<&str>,
    side: aterm_types::Appearance,
    name: &str,
    themes: &crate::app_config::ThemeCatalog,
) -> String {
    let (dark, light) = theme_slots(raw, themes);
    match side {
        aterm_types::Appearance::Dark => theme_split_value(name.trim(), &light),
        aterm_types::Appearance::Light => theme_split_value(&dark, name.trim()),
    }
}

/// Whether a configured `theme` value draws what an unset one does (Default, on every appearance):
/// a value naming no theme, or the one name [`aterm_types::scheme::UNSET_THEME`], which no theme
/// file shadows (a built-in always wins). A pair naming Default on both sides is not one: it
/// turns Follow system on.
pub(crate) fn theme_is_unset(raw: Option<&str>) -> bool {
    use aterm_types::scheme::ThemeSelector;
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return true;
    };
    match ThemeSelector::parse(raw).0 {
        ThemeSelector::Single(name) => name.eq_ignore_ascii_case(aterm_types::scheme::UNSET_THEME),
        selector => selector_unset(&selector),
    }
}

/// Whether `selector` names no theme for either side.
fn selector_unset(selector: &aterm_types::scheme::ThemeSelector) -> bool {
    selector.name_for(aterm_types::Appearance::Dark).is_none()
        && selector.name_for(aterm_types::Appearance::Light).is_none()
}

/// Format the `theme` config value selecting theme `id` for a specific appearance side.
pub(crate) fn theme_choice_value(
    raw: Option<&str>,
    side: aterm_types::Appearance,
    id: &str,
    themes: &crate::app_config::ThemeCatalog,
) -> (String, Option<aterm_types::Appearance>) {
    let id = id.trim();
    if theme_mode(raw) != ThemeMode::FollowSystem {
        return (id.to_string(), None);
    }
    (theme_with_side(raw, side, id, themes), Some(side))
}

/// The `theme` value a choice that names NO side writes (the command palette, the whole-theme picker).
pub(crate) fn theme_sideless_choice_value(
    raw: Option<&str>,
    shown: aterm_types::Appearance,
    id: &str,
    themes: &crate::app_config::ThemeCatalog,
) -> (String, Option<aterm_types::Appearance>) {
    let made_for = themes.meta(id.trim()).map_or(shown, |meta| meta.appearance);
    if theme_mode(raw) == ThemeMode::FollowSystem && made_for != shown {
        return (id.trim().to_string(), None);
    }
    theme_choice_value(raw, shown, id, themes)
}

/// The value turning Follow system off writes.
pub(crate) fn theme_always_value(
    raw: Option<&str>,
    visible: aterm_types::Appearance,
    themes: &crate::app_config::ThemeCatalog,
) -> String {
    let (dark, light) = theme_slots(raw, themes);
    match visible {
        aterm_types::Appearance::Dark => dark,
        aterm_types::Appearance::Light => light,
    }
}

#[cfg(test)]
#[path = "prefs_theme_pair_tests.rs"]
mod theme_pair_tests;

/// An error from [`apply_prefs_edits`].
#[derive(Debug)]
pub(crate) enum PrefsEditError {
    Parse(String),
    BadValue {
        key: String,
        raw: String,
        expected: Option<String>,
    },
    OutOfRange {
        key: String,
        range: String,
    },
    /// A value the file keeps only in another state, which nothing would read now.
    NotKept {
        key: String,
        why: &'static str,
    },
    /// A cursor style name this build retired, with the settings that say it now.
    RetiredName {
        key: String,
        raw: String,
        now: String,
    },
}

impl std::fmt::Display for PrefsEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrefsEditError::Parse(e) => write!(f, "existing aterm.toml is not valid TOML: {e}"),
            PrefsEditError::BadValue {
                key,
                raw,
                expected: None,
            } => write!(f, "invalid value for {key}: {raw:?}"),
            PrefsEditError::BadValue {
                key,
                raw,
                expected: Some(expected),
            } => write!(f, "invalid value for {key}: {raw:?}; it must be {expected}"),
            PrefsEditError::OutOfRange { key, range } => {
                write!(f, "invalid value for {key}: must be {range}")
            }
            PrefsEditError::NotKept { key, why } => write!(f, "{key} {why}"),
            PrefsEditError::RetiredName { key, raw, now } => {
                write!(f, "{key} no longer takes {raw:?}; set {now}")
            }
        }
    }
}

impl std::error::Error for PrefsEditError {}

/// Apply settings edits to the current `aterm.toml` text non-destructively, returning the new text.
pub(crate) fn apply_prefs_edits(
    existing_toml: &str,
    edits: &[(&str, Option<String>)],
) -> Result<String, PrefsEditError> {
    let mut doc = existing_toml
        .parse::<aterm_toml::edit::DocumentMut>()
        .map_err(|e| PrefsEditError::Parse(e.to_string()))?;

    let cursor_before = CursorStyleFacts::of(&doc);
    fold_retired_cursor_trail(&mut doc, edits);
    for (key, value) in edits {
        // DOTTED keys edit a nested-table leaf (`update.auto_apply`,
        // `sparkle_words.profanity.enabled`): the writer walks the table chain
        // — creating missing tables, refusing to clobber a non-table — instead
        // of the top-level `doc[key]` path, which would write a literal
        // `"update.auto_apply"` ROOT key serde ignores.
        if key.contains('.') {
            let parts: Vec<&str> = key.split('.').collect();
            match value {
                None => {
                    doc.remove_path_pruning(&parts);
                }
                Some(raw) => {
                    let item = typed_item(key, raw)?;
                    set_nested_key(&mut doc, &parts, item)?;
                }
            }
            for (retired, current) in RETIRED_PACKAGES_SPELLINGS {
                if current == key {
                    let retired: Vec<&str> = retired.split('.').collect();
                    doc.remove_path_pruning(&retired);
                }
            }
            continue;
        }
        match value {
            None => {
                doc.remove(key);
            }
            Some(raw) => {
                let mut item = typed_item(key, raw)?;
                adopt_inline_decor(doc.get(key), &mut item);
                doc[*key] = item;
            }
        }
    }
    keep_cursor_style_before_off(&cursor_before, &mut doc, edits)?;
    keep_classic_mono_color(&cursor_before, &mut doc, edits);

    Ok(doc.to_string())
}

fn keep_classic_mono_color(
    before: &CursorStyleFacts,
    doc: &mut aterm_toml::edit::DocumentMut,
    edits: &[(&str, Option<String>)],
) {
    let names_mono = |style: Option<&str>| {
        style.is_some_and(aterm_effects::cursor_glow::GlowStyle::style_names_classic_mono)
    };
    let is_classic = |style: &str| {
        crate::app_config::resolve_trail_style(style).style
            == Some(aterm_effects::cursor_glow::GlowStyle::Classic)
    };
    let after = CursorStyleFacts::of(doc);
    let classic_stays = is_classic(after.drawn())
        || doc
            .get(EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF)
            .and_then(aterm_toml::edit::Item::as_str)
            .is_some_and(is_classic);
    if !names_mono(before.style.as_deref())
        || !classic_stays
        || names_mono(after.style.as_deref())
        || trail_color_setting(
            doc.get(EDIT_CURSOR_TRAIL_COLOR)
                .and_then(aterm_toml::edit::Item::as_str),
        )
        .chosen()
        || edits.iter().any(|(key, _)| *key == EDIT_CURSOR_TRAIL_COLOR)
    {
        return;
    }
    if let Ok(item) = typed_item(EDIT_CURSOR_TRAIL_COLOR, TRAIL_COLOR_CURSOR) {
        doc[EDIT_CURSOR_TRAIL_COLOR] = item;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CursorStyleFacts {
    master: Option<bool>,
    style: Option<String>,
}

impl CursorStyleFacts {
    fn of(doc: &aterm_toml::edit::DocumentMut) -> Self {
        Self {
            master: doc
                .get(RETIRED_CURSOR_TRAIL)
                .and_then(aterm_toml::edit::Item::as_bool),
            style: doc
                .get(EDIT_CURSOR_TRAIL_STYLE)
                .and_then(aterm_toml::edit::Item::as_str)
                .map(str::to_string),
        }
    }

    fn drawn(&self) -> &str {
        crate::app_config::drawn_cursor_trail_style(self.master, self.style.as_deref())
    }
}

fn style_draws(style: &str) -> bool {
    crate::app_config::resolve_trail_style(style)
        .style
        .is_some()
}

/// THE STYLE BEFORE OFF IS A STYLE THAT DRAWS, KEPT WHILE THE CURSOR STYLE IS OFF: Cursor
/// Styles marks it there, and nothing reads it while a style draws (a later Off writes the style
/// it replaces over it). A write of it while a style draws is refused, and one naming no style
/// that draws (`off`) is no value.
fn keep_cursor_style_before_off(
    before: &CursorStyleFacts,
    doc: &mut aterm_toml::edit::DocumentMut,
    edits: &[(&str, Option<String>)],
) -> Result<(), PrefsEditError> {
    let after = CursorStyleFacts::of(doc);
    if edits
        .iter()
        .any(|(key, _)| *key == EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF)
    {
        let written = doc
            .get(EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF)
            .and_then(aterm_toml::edit::Item::as_str)
            .map(str::to_string);
        match written {
            Some(style) if !style_draws(&style) => {
                doc.remove(EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF);
            }
            Some(_) if style_draws(after.drawn()) => {
                return Err(PrefsEditError::NotKept {
                    key: EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF.to_string(),
                    why: "is kept only while Cursor style is Off",
                });
            }
            _ => {}
        }
        return Ok(());
    }
    if after == *before {
        return Ok(());
    }
    if style_draws(after.drawn()) {
        doc.remove(EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF);
        return Ok(());
    }
    let kept = if style_draws(before.drawn()) {
        Some(before.drawn())
    } else if before.master == Some(false) {
        Some(
            before
                .style
                .as_deref()
                .map_or(DEFAULT_CURSOR_TRAIL_STYLE, str::trim),
        )
        .filter(|held| style_draws(held))
    } else {
        None
    };
    let named = kept.and_then(|kept| crate::app_config::resolve_trail_style(kept).canonical);
    if let Some(kept) = named
        && let Ok(item) = typed_item(EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF, kept)
    {
        doc[EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF] = item;
    }
    Ok(())
}

fn fold_retired_cursor_trail(
    doc: &mut aterm_toml::edit::DocumentMut,
    edits: &[(&str, Option<String>)],
) {
    if !edits
        .iter()
        .any(|(key, _)| RETIRED_CURSOR_TRAIL_SUCCESSORS.contains(key))
    {
        return;
    }
    let Some(master) = doc.get(RETIRED_CURSOR_TRAIL).map(|item| item.as_bool()) else {
        return;
    };
    doc.remove(RETIRED_CURSOR_TRAIL);
    let Some(master) = master else {
        return;
    };
    let decorative = crate::app_config::DEFAULT_DECORATIVE_EFFECTS;
    let untouched = |key: &str| !edits.iter().any(|(edited, _)| *edited == key);
    if untouched(EDIT_CURSOR_TRAIL_STYLE) {
        let drawn = if master {
            doc.get(EDIT_CURSOR_TRAIL_STYLE)
                .is_none()
                .then_some(DEFAULT_CURSOR_TRAIL_STYLE)
        } else {
            Some("off")
        };
        if let Some(style) = drawn
            && (doc.get(EDIT_CURSOR_TRAIL_STYLE).is_some()
                || style != crate::app_config::default_cursor_trail_style())
            && let Ok(item) = typed_item(EDIT_CURSOR_TRAIL_STYLE, style)
        {
            doc[EDIT_CURSOR_TRAIL_STYLE] = item;
        }
    }
    // The master off drew no pet: the fold keeps that, or the pet's default would walk in
    // beside a trail that stays off. A style chosen in the same write turns the cursor's
    // effects on, as choosing one over the master always did, and the pet comes with it.
    let style_chosen = edits.iter().any(|(key, value)| {
        *key == EDIT_CURSOR_TRAIL_STYLE
            && value.as_deref().is_none_or(|style| {
                crate::app_config::resolve_trail_style(style)
                    .style
                    .is_some()
            })
    });
    if !master
        && !style_chosen
        && untouched(EDIT_CURSOR_PET)
        && doc.get(EDIT_CURSOR_PET).is_none()
        && crate::app_config::default_cursor_pet() != "off"
        && let Ok(item) = typed_item(EDIT_CURSOR_PET, "off")
    {
        doc[EDIT_CURSOR_PET] = item;
    }
    // The master on drew the default style's cat on every platform: where the platform's own
    // default is no pet (Windows), the fold writes the cat it kept (`drawn_cursor_pet`).
    if master
        && untouched(EDIT_CURSOR_PET)
        && doc.get(EDIT_CURSOR_PET).is_none()
        && crate::app_config::default_cursor_pet() != DEFAULT_CURSOR_PET
        && let Ok(item) = typed_item(EDIT_CURSOR_PET, DEFAULT_CURSOR_PET)
    {
        doc[EDIT_CURSOR_PET] = item;
    }
    if untouched(EDIT_CURSOR_MOMENTUM_GLOW) {
        let written = doc
            .get(EDIT_CURSOR_MOMENTUM_GLOW)
            .and_then(aterm_toml::edit::Item::as_bool);
        let drawn = if master {
            written.unwrap_or(true)
        } else {
            false
        };
        if (written.is_some() || drawn != decorative)
            && let Ok(item) = typed_item(EDIT_CURSOR_MOMENTUM_GLOW, &drawn.to_string())
        {
            doc[EDIT_CURSOR_MOMENTUM_GLOW] = item;
        }
    }
}

/// The TOML item a Settings row writes for `key` from `raw`.
pub(crate) fn typed_toml_item(
    key: &str,
    raw: &str,
) -> Result<aterm_toml::edit::Item, PrefsEditError> {
    typed_item(key, raw)
}

/// The value that means unset. A number's is the one its unset placeholder
/// names before the word it adds: "(default)", "(automatic)" where the
/// platform chooses it, or what it does there ("0 (off)", "1 (solid)"). An
/// explicit font size equal to the automatic one resolves, scales and draws
/// exactly as no `font_px` does, and an opacity of 1 as no `background_opacity`.
pub(crate) fn unset_literal(field: &EditField) -> Option<String> {
    if ai_key(field.key) {
        return None;
    }
    let placeholder = field.placeholder.trim();
    let marked = placeholder
        .strip_suffix(" (default)")
        .or_else(|| {
            matches!(field.kind, EditKind::Float | EditKind::Integer)
                .then(|| {
                    placeholder
                        .strip_suffix(')')
                        .and_then(|rest| rest.rsplit_once(" ("))
                        .map(|(number, _mark)| number)
                })
                .flatten()
        })
        .map(str::trim);
    match field.kind {
        EditKind::Bool => field.seed.clone(),
        EditKind::Enum { options } => {
            let named = marked
                .or_else(|| {
                    placeholder
                        .strip_suffix(')')
                        .and_then(|rest| rest.split_once(" ("))
                        .map(|(value, _)| value.trim())
                })
                .unwrap_or(placeholder);
            options
                .iter()
                .find(|option| option.eq_ignore_ascii_case(named))
                .map(|option| (*option).to_string())
        }
        EditKind::Float | EditKind::Integer => marked
            .and_then(|marked| marked.split_whitespace().next())
            .filter(|token| token.parse::<f64>().is_ok_and(f64::is_finite))
            .map(str::to_string),
        _ => text_unset_literal(field.key).map(str::to_string),
    }
}

/// The `ai` table's keys, whose value equal to aterm's own choice still says something: an
/// explicit value is kept over the layer under it, so none has a value that means unset.
fn ai_key(key: &str) -> bool {
    key == "ai" || key.starts_with("ai.")
}

/// Whether `value` is `field`'s [`unset_literal`], or for the theme any value that draws what no
/// theme does ([`theme_is_unset`]): the built-in Default in any spelling ("default" too), never a
/// malformed selector, which keeps its diagnostic. (The theme has no literal of its own: Manual
/// adds an absent `theme` for a name to be typed, never as "Default".) A text or colour key also
/// takes its row's words for no value as none ([`spells_no_value_words`]).
pub(crate) fn is_unset_literal(field: &EditField, value: &str) -> bool {
    if field.kind == EditKind::Theme {
        return aterm_types::scheme::ThemeSelector::parse(value)
            .1
            .is_empty()
            && theme_is_unset(Some(value));
    }
    if spells_no_value_words(field, value) {
        return true;
    }
    let Some(literal) = unset_literal(field) else {
        return false;
    };
    let (value, literal) = (value.trim(), literal.trim());
    match field.kind {
        EditKind::Bool => {
            let read = |text: &str| text.to_ascii_lowercase().parse::<bool>().ok();
            read(value).is_some() && read(value) == read(literal)
        }
        EditKind::Enum { .. } => {
            let stored = |text: &str| {
                typed_item(field.key, text)
                    .ok()
                    .and_then(|item| item.as_str().map(str::to_string))
            };
            stored(value).is_some_and(|stored_value| {
                stored(literal).is_some_and(|stored_literal| {
                    stored_value.eq_ignore_ascii_case(&stored_literal)
                })
            })
        }
        EditKind::Float | EditKind::Integer => match (value.parse::<f64>(), literal.parse::<f64>())
        {
            (Ok(set), Ok(default)) if set.is_finite() && default.is_finite() => {
                (set - default).abs() <= 1e-6 * default.abs().max(1.0)
            }
            _ => false,
        },
        _ => value == literal,
    }
}

/// The font keys whose row offers aterm's own choice, "Automatic", for no font of their own.
const AUTOMATIC_FONT_KEYS: &[&str] = &[
    EDIT_FONT_FAMILY,
    EDIT_FONT_FAMILY_BOLD,
    EDIT_FONT_FAMILY_ITALIC,
    EDIT_FONT_FAMILY_BOLD_ITALIC,
    EDIT_SYMBOL_FONT,
    EDIT_EMOJI_FONT,
    EDIT_FALLBACK_FONTS,
];

/// Whether `value` is the words font key `key`'s row shows for no font of its own: "Automatic",
/// or "Automatic (SF Mono)" naming the face aterm picks; where the theme names one, its picker's
/// "Use theme's (Menlo)" or the row's "Menlo (from theme Studio)". No font goes by them, so a
/// writer takes them as that row's no-value choice does, as no value: written, they named a font
/// that does not resolve.
pub(crate) fn font_words_mean_unset(key: &str, value: &str) -> bool {
    if !AUTOMATIC_FONT_KEYS.contains(&key) {
        return false;
    }
    let value = value.trim();
    let after = |word: &str| {
        value
            .get(..word.len())
            .filter(|head| head.eq_ignore_ascii_case(word))
            .and_then(|_| value.get(word.len()..))
            .map(str::trim_start)
    };
    let parenthesized = |rest: &str| rest.starts_with('(') && rest.ends_with(')');
    after("Automatic").is_some_and(|rest| rest.is_empty() || parenthesized(rest))
        || after("Use theme's").is_some_and(parenthesized)
        || value
            .strip_suffix(')')
            .and_then(|head| head.rsplit_once(" (from theme "))
            .is_some_and(|(font, theme)| !font.trim().is_empty() && !theme.trim().is_empty())
}

/// Whether `value` spells the words unset text, colour or number field `field` shows for no
/// value of its own, its [`EditField::placeholder`], which `ctl settings unset` answers too:
/// "Platform", "No image", "none", "Theme", "builtin only", a number's "theme-derived", read
/// without what follows them (" (default)", a face it names, an " — e.g. …" hint). No reader
/// takes them as that: written, they named a shell called "Platform", a wallpaper file "No
/// image", a shell argument "none", and a number refused them. A colour's or a number's words
/// count only where they are no value of their own: the trail color's "Cursor color" is one,
/// which a later style keeps, and a number is its [`unset_literal`]'s to judge.
fn spells_no_value_words(field: &EditField, value: &str) -> bool {
    let numeric = matches!(field.kind, EditKind::Float | EditKind::Integer);
    if field.seed.is_some()
        || ai_key(field.key)
        || !(numeric || matches!(field.kind, EditKind::Text | EditKind::Color))
        || (field.kind == EditKind::Color && color_value_is_valid(field.key, value))
        || (numeric && value.trim().parse::<f64>().is_ok())
    {
        return false;
    }
    let words = bare_words(&field.placeholder);
    !words.is_empty() && bare_words(value).eq_ignore_ascii_case(words)
}

/// `text`'s words without what a row adds after them: an " — e.g. …" hint, then everything
/// from its first parenthesis on, such as a " (default)" mark or the face aterm draws in
/// "Automatic (SF Mono)".
fn bare_words(text: &str) -> &str {
    let text = text.trim();
    let text = text
        .split_once(" \u{2014} ")
        .map_or(text, |(words, _hint)| words);
    text.split_once(" (")
        .map_or(text, |(words, _)| words)
        .trim()
}

/// Whether collection key `key` written from `value` holds no entry: `[]`, or text whose
/// entries are all blank, which every reader takes as no list at all.
fn written_list_is_empty(key: &str, value: &str) -> bool {
    use aterm_toml::edit::Value;
    typed_item(key, value)
        .ok()
        .is_some_and(|item| match item.as_value() {
            Some(Value::Array(array)) => array.is_empty(),
            Some(Value::String(text)) => {
                text.value().split(',').all(|entry| entry.trim().is_empty())
            }
            _ => false,
        })
}

/// Whether each edit in `edits` sets a value equivalent to unsetting the key.
pub(crate) fn unset_equivalent_edits(
    existing_toml: &str,
    edits: &[(&str, Option<String>)],
    themes: &crate::app_config::ThemeCatalog,
) -> Vec<bool> {
    edits
        .iter()
        .enumerate()
        .map(|(index, (key, value))| {
            let Some(value) = value.as_deref() else {
                return false;
            };
            if font_words_mean_unset(key, value) {
                return true;
            }
            let without: Vec<(&str, Option<String>)> = edits
                .iter()
                .enumerate()
                .map(|(other, (key, value))| {
                    (*key, (other != index).then(|| value.clone()).flatten())
                })
                .collect();
            let Ok(text) = apply_prefs_edits(existing_toml, &without) else {
                return false;
            };
            let Ok(config) = Config::parse(&text) else {
                return false;
            };
            if *key == EDIT_DISPLAY_FONT && display_layer_under(&config, themes) {
                return false;
            }
            if theme_sets_type_key(&config, themes, key) {
                return false;
            }
            if manual_collection_key(key) && written_list_is_empty(key, value) {
                return true;
            }
            editable_fields(&config)
                .iter()
                .find(|field| field.key == *key)
                .is_some_and(|field| is_unset_literal(field, value))
        })
        .collect()
}

fn retired_successor_pairs() -> impl Iterator<Item = (&'static str, &'static str)> {
    RETIRED_PACKAGES_SPELLINGS.iter().copied().chain(
        RETIRED_CURSOR_TRAIL_SUCCESSORS
            .iter()
            .map(|successor| (RETIRED_CURSOR_TRAIL, *successor)),
    )
}

/// The settings a retired spelling still authors in `text`.
pub(crate) fn retired_authored_keys(text: &str) -> std::collections::BTreeSet<&'static str> {
    let mut authored = std::collections::BTreeSet::new();
    let Ok(doc) = text.parse::<aterm_toml::edit::DocumentMut>() else {
        return authored;
    };
    let holds = |doc: &aterm_toml::edit::DocumentMut, path: &str| {
        let mut parts = path.split('.');
        let first = parts.next().and_then(|first| doc.get(first));
        parts
            .fold(first, |item, part| item.and_then(|item| item.get(part)))
            .is_some()
    };
    // A config that holds no retired spelling is parsed no further.
    let held: Vec<_> = retired_successor_pairs()
        .filter(|(retired, _)| holds(&doc, retired))
        .collect();
    if held.is_empty() {
        return authored;
    }
    let Ok(with) = Config::parse(text) else {
        return authored;
    };
    let fields_with = editable_fields(&with);
    for (retired, successor) in held {
        let mut without = doc.clone();
        without.remove_path_pruning(&retired.split('.').collect::<Vec<_>>());
        let Ok(without) = Config::parse(&without.to_string()) else {
            continue;
        };
        let reads = |fields: &[EditField]| {
            fields
                .iter()
                .find(|field| field.key == successor)
                .map(|field| {
                    field.seed.clone().unwrap_or_else(|| {
                        let shown = field.placeholder.trim();
                        shown
                            .strip_suffix(" (default)")
                            .unwrap_or(shown)
                            .to_string()
                    })
                })
        };
        if reads(&fields_with) != reads(&editable_fields(&without)) {
            authored.insert(successor);
        }
    }
    authored
}

fn theme_sets_type_key(cfg: &Config, themes: &crate::app_config::ThemeCatalog, key: &str) -> bool {
    let theme_only = Config {
        theme: cfg.theme.clone(),
        ..Config::default()
    };
    [
        aterm_types::Appearance::Light,
        aterm_types::Appearance::Dark,
    ]
    .into_iter()
    .any(|side| {
        let typo = theme_only.resolved_typography_with_flag(side, themes, None);
        theme_value_for(&Config::default(), &typo, key).is_some()
    })
}

/// Whether an unset `display_font` in `cfg` still draws display faces.
pub(crate) fn display_layer_under(cfg: &Config, themes: &crate::app_config::ThemeCatalog) -> bool {
    let draws = |family: &str| aterm_render::display_face_ids_for_family(family).is_some();
    if let Some(family) = cfg
        .font_family
        .as_deref()
        .filter(|family| !family.trim().is_empty())
    {
        return draws(family);
    }
    let theme_only = Config {
        theme: cfg.theme.clone(),
        ..Config::default()
    };
    [
        aterm_types::Appearance::Light,
        aterm_types::Appearance::Dark,
    ]
    .into_iter()
    .any(|side| {
        let typo = theme_only.resolved_typography_with_flag(side, themes, None);
        theme_value_for(&Config::default(), &typo, EDIT_FONT_FAMILY)
            .is_some_and(|family| draws(&family))
    })
}

fn text_unset_literal(key: &str) -> Option<&'static str> {
    match key {
        "matrix_rain.hue" => Some(MATRIX_RAIN_HUE_UNSET),
        _ => None,
    }
}

const MATRIX_RAIN_HUE_UNSET: &str = "matrix";

/// `existing` with `key` set to `item` where TOML reads it.
pub(crate) fn with_toml_item(
    existing: &str,
    key: &str,
    item: aterm_toml::edit::Item,
) -> Result<String, PrefsEditError> {
    let mut doc = existing
        .parse::<aterm_toml::edit::DocumentMut>()
        .map_err(|e| PrefsEditError::Parse(e.to_string()))?;
    if key.contains('.') {
        let parts: Vec<&str> = key.split('.').collect();
        set_nested_key(&mut doc, &parts, item)?;
    } else {
        doc[key] = item;
    }
    Ok(doc.to_string())
}

fn adopt_inline_decor(old: Option<&aterm_toml::edit::Item>, new: &mut aterm_toml::edit::Item) {
    let Some(old_value) = old.and_then(aterm_toml::edit::Item::as_value) else {
        return;
    };
    let Some(new_value) = new.as_value_mut() else {
        return;
    };
    let decor = old_value.decor().clone();
    if let Some(prefix) = decor.prefix() {
        new_value.decor_mut().set_prefix(prefix.clone());
    }
    if let Some(suffix) = decor.suffix() {
        new_value.decor_mut().set_suffix(suffix.clone());
    }
}

fn set_nested_key(
    doc: &mut aterm_toml::edit::DocumentMut,
    parts: &[&str],
    mut item: aterm_toml::edit::Item,
) -> Result<(), PrefsEditError> {
    let not_a_table = |part: &str| {
        PrefsEditError::Parse(format!(
            "config key {part:?} exists but is not a table; refusing to overwrite it"
        ))
    };
    let (leaf, tables) = parts.split_last().expect("dotted key has segments");
    let mut cur: &mut aterm_toml::edit::Item = doc.as_item_mut();
    for part in tables {
        let table = cur.as_table_like_mut().ok_or_else(|| not_a_table(part))?;
        if table.get(part).is_none_or(aterm_toml::edit::Item::is_none) {
            let mut fresh = aterm_toml::edit::Table::new();
            fresh.set_implicit(true);
            table.insert(part, aterm_toml::edit::Item::Table(fresh));
        }
        cur = table.get_mut(part).expect("just ensured present");
    }
    let table = cur
        .as_table_like_mut()
        .ok_or_else(|| not_a_table(parts[parts.len() - 2]))?;
    if table
        .get(leaf)
        .is_some_and(aterm_toml::edit::Item::is_table_like)
    {
        return Err(not_a_table(leaf));
    }
    adopt_inline_decor(table.get(leaf), &mut item);
    table.insert(leaf, item);
    Ok(())
}

fn integer_domain(key: &str) -> std::ops::RangeInclusive<i64> {
    match key {
        EDIT_TAB_STATUS_QUIET_AFTER_MS => {
            i64::try_from(MIN_TAB_STATUS_QUIET_AFTER_MS).expect("tab-status quiet floor fits i64")
                ..=i64::try_from(MAX_TAB_STATUS_QUIET_AFTER_MS)
                    .expect("tab-status quiet ceiling fits i64")
        }
        EDIT_TAB_STATUS_DWELL_MS => {
            i64::try_from(MIN_TAB_STATUS_DWELL_MS).expect("tab-status dwell floor fits i64")
                ..=i64::try_from(MAX_TAB_STATUS_DWELL_MS)
                    .expect("tab-status dwell ceiling fits i64")
        }
        EDIT_COLUMNS | EDIT_LINES | EDIT_TAB_STRIP_ROWS => 0..=i64::from(u16::MAX),
        EDIT_SEARCH_HISTORY_LINES
        | EDIT_FONT_WEIGHT
        | "sparkle_words.profanity.supernova_chance"
        | "sparkle_words.profanity.density"
        | "sparkle_words.ink.sweep_ms"
        | "matrix_rain.fps"
        | "matrix_rain.density"
        | "matrix_rain.speed"
        | "matrix_rain.trail"
        | "matrix_rain.alpha"
        | "matrix_rain.head_alpha" => 0..=i64::from(u32::MAX),
        "sparkle_words.profanity.jitter" => i64::from(i8::MIN)..=i64::from(i8::MAX),
        EDIT_ADJUST_BASELINE | EDIT_ADJUST_UNDERLINE_POSITION | EDIT_ADJUST_UNDERLINE_THICKNESS => {
            i64::MIN..=i64::MAX
        }
        _ => 0..=i64::MAX,
    }
}

/// Validates whether the writer accepts `raw` for `key`.
pub(crate) fn check_edit_value(key: &str, raw: &str) -> Result<(), PrefsEditError> {
    typed_item(key, raw).map(|_| ())
}

/// What a whole number for Integer `key` must be, worded for `raw`, a value the writer refused ([`integer_domain`]).
pub(crate) fn integer_requirement(key: &str, raw: &str) -> String {
    let (start, end) = integer_domain(key).into_inner();
    let raw = raw.trim();
    let digits = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    let spelled_whole = !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
    if spelled_whole || whole_number(raw).is_some() {
        let above = whole_number(raw).map_or(!raw.starts_with('-'), |value| value > end);
        return if above {
            format!("at most {}", integer_reading(key, end))
        } else if start == 0 {
            format!("{} or more", integer_reading(key, 0))
        } else {
            format!("at least {}", integer_reading(key, start))
        };
    }
    if start == 0 && end >= 1_000_000 {
        format!("a whole number, {} or more", integer_reading(key, 0))
    } else if start == i64::MIN && end == i64::MAX {
        "a whole number".to_string()
    } else {
        format!(
            "a whole number from {} to {}",
            integer_reading(key, start),
            integer_reading(key, end)
        )
    }
}

const EXACT_WHOLE_F64: f64 = 9_007_199_254_740_992.0;

fn exact_whole(value: f64) -> Option<i64> {
    (value.is_finite() && value.fract() == 0.0 && value.abs() <= EXACT_WHOLE_F64)
        .then_some(value as i64)
}

fn whole_number(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if let Ok(value) = raw.parse::<i64>() {
        return Some(value);
    }
    let negative = raw.starts_with('-');
    let raw = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    let (mantissa, exponent) = raw.split_once(['e', 'E']).unwrap_or((raw, "0"));
    let exponent_digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    if exponent_digits.is_empty() || !exponent_digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let exponent = exponent.parse::<i64>().unwrap_or_else(|_| {
        if exponent.starts_with('-') {
            i64::MIN
        } else {
            i64::MAX
        }
    });
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let coefficient = [whole, fraction].concat();
    let digits = coefficient.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    let power = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
    let (digits, zeros) = if power < 0 {
        let remove = usize::try_from(power.checked_neg()?).ok()?;
        let kept = digits.len().checked_sub(remove)?;
        if digits[kept..].bytes().any(|b| b != b'0') {
            return None;
        }
        (&digits[..kept], 0)
    } else {
        (digits, usize::try_from(power).ok()?)
    };
    if digits.len().checked_add(zeros)? > 19 {
        return None;
    }
    digits
        .bytes()
        .chain(std::iter::repeat_n(b'0', zeros))
        .try_fold(0i64, |value, digit| {
            let value = value.checked_mul(10)?;
            let digit = i64::from(digit - b'0');
            if negative {
                value.checked_sub(digit)
            } else {
                value.checked_add(digit)
            }
        })
}

/// TOML value written for a fractional ([`EditKind::Float`]) setting.
pub(crate) fn written_number(value: f64) -> aterm_toml::edit::Value {
    exact_whole(value).map_or_else(
        || aterm_toml::edit::Value::from(value),
        aterm_toml::edit::Value::from,
    )
}

fn font_px_admissible(px: f64) -> bool {
    px.is_finite() && (f64::from(crate::FONT_PX_MIN)..=f64::from(crate::FONT_PX_MAX)).contains(&px)
}

fn font_px_range() -> String {
    format!("{}..={}", crate::FONT_PX_MIN, crate::FONT_PX_MAX)
}

fn structured_value(raw: &str) -> Option<aterm_toml::edit::Value> {
    use aterm_toml::edit::{DocumentMut, InlineTable, Item, TableLike, Value};
    if let Ok(value) = raw.parse::<Value>() {
        return (value.is_array() || value.as_inline_table().is_some()).then(|| {
            let mut value = value;
            value.decor_mut().clear();
            value
        });
    }
    let body = raw.parse::<DocumentMut>().ok()?;
    if body.iter().next().is_none() && !raw.trim().is_empty() {
        return None;
    }
    let mut table = InlineTable::new();
    for (key, item) in body.iter() {
        let mut value = item.as_value()?.clone();
        value.decor_mut().clear();
        table.insert(key, Item::Value(value));
    }
    Some(Value::InlineTable(table))
}

fn typed_item(key: &str, raw: &str) -> Result<aterm_toml::edit::Item, PrefsEditError> {
    use aterm_toml::edit::{Item, Value};
    let bad = || PrefsEditError::BadValue {
        key: key.to_string(),
        raw: raw.to_string(),
        expected: None,
    };
    let trimmed = raw.trim();
    let structured = if key == EDIT_PALETTE_OVERRIDES {
        Some(structured_value(trimmed).ok_or_else(bad)?)
    } else if manual_collection_key(key) && trimmed.starts_with('[') {
        trimmed
            .parse::<Value>()
            .ok()
            .filter(Value::is_array)
            .map(|mut value| {
                value.decor_mut().clear();
                value
            })
    } else {
        None
    };
    if let Some(value) = structured {
        let colour = |v: &Value| {
            v.as_str()
                .and_then(aterm_types::scheme::parse_hex)
                .is_some()
        };
        let fits = match &value {
            Value::InlineTable(table) => {
                key == EDIT_PALETTE_OVERRIDES
                    && aterm_toml::edit::TableLike::iter(table).all(|(slot, item)| {
                        crate::resolved_colors::palette_override_index(slot).is_some()
                            && item.as_value().is_some_and(colour)
                    })
            }
            Value::Array(array) => {
                key != EDIT_PALETTE_OVERRIDES
                    && array.iter().all(|v| {
                        if key == EDIT_PALETTE {
                            colour(v)
                        } else {
                            v.as_str().is_some()
                        }
                    })
            }
            _ => false,
        };
        return if fits {
            Ok(Item::Value(value))
        } else {
            Err(bad())
        };
    }
    if LIST_KEYS.contains(&key) {
        let entries: Vec<&str> = trimmed
            .split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .collect();
        if entries.is_empty() {
            return Err(bad());
        }
        if key == EDIT_PALETTE
            && entries
                .iter()
                .any(|e| aterm_types::scheme::parse_hex(e).is_none())
        {
            return Err(bad());
        }
        let array: aterm_toml::edit::Array = entries.into_iter().collect();
        return Ok(Item::Value(Value::Array(array)));
    }
    let value = match edit_kind(key) {
        EditKind::Float => {
            let not_a_number = || PrefsEditError::BadValue {
                key: key.to_string(),
                raw: raw.to_string(),
                expected: Some("a number".to_string()),
            };
            let px = trimmed.parse::<f64>().map_err(|_| not_a_number())?;
            if key == EDIT_FONT_PX && !font_px_admissible(px) {
                return Err(PrefsEditError::OutOfRange {
                    key: key.to_string(),
                    range: font_px_range(),
                });
            }
            if !px.is_finite() {
                return Err(not_a_number());
            }
            written_number(px)
        }
        EditKind::Integer => match whole_number(trimmed) {
            Some(n) if integer_domain(key).contains(&n) => Value::from(n),
            _ => {
                return Err(PrefsEditError::BadValue {
                    key: key.to_string(),
                    raw: raw.to_string(),
                    expected: Some(integer_requirement(key, trimmed)),
                });
            }
        },
        EditKind::Bool => Value::from(trimmed.parse::<bool>().map_err(|_| bad())?),
        EditKind::Enum { options } => {
            let not_an_option = || PrefsEditError::BadValue {
                key: key.to_string(),
                raw: raw.to_string(),
                expected: Some(format!("one of: {}", options.join(", "))),
            };
            if key == EDIT_CURSOR_TRAIL_STYLE || key == EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF {
                if let Some(was) = spellings::retired(trimmed) {
                    return Err(PrefsEditError::RetiredName {
                        key: key.to_string(),
                        raw: raw.to_string(),
                        now: retired_cursor_name_now(key, was),
                    });
                }
                let canonical = cursor_trail_style_canonical(trimmed).ok_or_else(not_an_option)?;
                if GlowStyle::style_names_classic_mono(trimmed) {
                    Value::from("classic mono")
                } else {
                    Value::from(canonical)
                }
            } else if key == EDIT_DISPLAY_FONT && trimmed.contains('+') {
                let parts: Vec<&str> = trimmed
                    .split('+')
                    .map(|part| {
                        aterm_render::display_face_canonical_id(part).unwrap_or(part.trim())
                    })
                    .collect();
                let distinct = parts
                    .iter()
                    .all(|part| parts.iter().filter(|other| other == &part).count() == 1);
                if parts.len() < 2
                    || parts.len() > aterm_render::DISPLAY_FACE_MIX_MAX
                    || !distinct
                    || parts
                        .iter()
                        .any(|part| aterm_render::display_face_bytes(part).is_none())
                {
                    return Err(PrefsEditError::BadValue {
                        key: key.to_string(),
                        raw: raw.to_string(),
                        expected: Some(format!(
                            "2 to {} different faces joined by \"+\", each one of: {}",
                            aterm_render::DISPLAY_FACE_MIX_MAX,
                            options
                                .iter()
                                .filter(|option| **option != "off")
                                .copied()
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                    });
                }
                Value::from(parts.join("+"))
            } else if key == EDIT_DISPLAY_FONT
                && let Some(canon) = aterm_render::display_face_canonical_id(trimmed)
            {
                Value::from(canon)
            } else {
                let canon = options
                    .iter()
                    .find(|o| trimmed.eq_ignore_ascii_case(o))
                    .ok_or_else(not_an_option)?;
                Value::from(*canon)
            }
        }
        EditKind::Text
            if key == EDIT_HARNESS_LAND_AT
                && aterm_agent::supervise::LandAt::parse(trimmed).is_none() =>
        {
            return Err(bad());
        }
        EditKind::Text | EditKind::Theme => Value::from(trimmed),
        EditKind::Color => {
            if !color_value_is_valid(key, trimmed) {
                return Err(bad());
            }
            if key == EDIT_CURSOR_TRAIL_COLOR && names_trail_color_cursor(trimmed) {
                Value::from(TRAIL_COLOR_CURSOR)
            } else {
                Value::from(trimmed)
            }
        }
    };
    Ok(Item::Value(value))
}

/// One editable field of Settings.
pub(crate) struct EditField {
    pub(crate) label: &'static str,
    pub(crate) key: &'static str,
    pub(crate) kind: EditKind,
    pub(crate) seed: Option<String>,
    pub(crate) placeholder: String,
}

impl EditField {
    /// The text this field's text box holds for its saved value: the value, in the words its
    /// row reads elsewhere (the trail color's cursor word as [`TRAIL_COLOR_CURSOR_LABEL`], as
    /// its placeholder and its color wheel say it), empty while unset.
    pub(crate) fn seed_text(&self) -> std::borrow::Cow<'_, str> {
        let seed = self.seed.as_deref().unwrap_or_default();
        if self.key == EDIT_CURSOR_TRAIL_COLOR && names_trail_color_cursor(seed) {
            std::borrow::Cow::Borrowed(TRAIL_COLOR_CURSOR_LABEL)
        } else {
            std::borrow::Cow::Borrowed(seed)
        }
    }
}

fn configured_str(field: Option<&str>) -> Option<String> {
    field
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn nested_seed_placeholder(cfg: &Config, key: &str) -> (Option<String>, String) {
    let sw = cfg.sparkle_words.as_ref();
    let prof = sw.and_then(|s| s.profanity.as_ref());
    let fel = sw.and_then(|s| s.feline.as_ref());
    let ink = sw.and_then(|s| s.ink.as_ref());
    let emph = sw.and_then(|s| s.emphasis.as_ref());
    let mr = cfg.matrix_rain.as_ref();
    let upd = cfg.update.as_ref();
    fn boolean(v: Option<bool>, default: bool) -> (Option<String>, String) {
        (Some(v.unwrap_or(default).to_string()), String::new())
    }
    fn num<T: ToString>(v: Option<T>, ph: &str) -> (Option<String>, String) {
        (v.map(|n| n.to_string()), ph.to_string())
    }
    fn txt(v: Option<&str>, ph: &str) -> (Option<String>, String) {
        (configured_str(v), ph.to_string())
    }
    match key {
        EDIT_AMACS_DEFAULT_EDITOR => boolean(Some(cfg.amacs.config.default_editor), true),
        EDIT_KEEPER_ENABLED => boolean(Some(cfg.keeper_enabled()), crate::keeper_link::DEFAULT_ON),
        EDIT_SESSION_LOG_JOURNAL => boolean(Some(cfg.session_log_journal()), true),
        EDIT_SESSION_LOG_CLAUSE => boolean(Some(cfg.session_log_clause()), true),
        EDIT_UPDATE_ENABLED => boolean(upd.and_then(|u| u.enabled), true),
        "update.auto_apply" => boolean(upd.and_then(|u| u.auto_apply), true),
        EDIT_REROUTE_ANNOUNCE => boolean(cfg.reroute.as_ref().and_then(|r| r.announce), true),
        EDIT_HARNESS_ENABLED => boolean(Some(cfg.harness.policy.enabled), true),
        EDIT_HARNESS_AUTOPILOT => {
            let mode = cfg.harness.policy.autopilot;
            (
                (mode != aterm_agent::supervise::Autopilot::Cruise)
                    .then(|| mode.word().to_string()),
                "cruise (default)".to_string(),
            )
        }
        EDIT_HARNESS_LAND_AT => (
            cfg.harness
                .policy
                .land_at
                .map(aterm_agent::supervise::LandAt::text),
            "none (HH:MM)".to_string(),
        ),
        EDIT_HARNESS_ANSWER_QUESTIONS => (
            Some(cfg.harness.policy.answer_questions.to_string()),
            "true".to_string(),
        ),
        EDIT_HARNESS_APPROVE => {
            let level = cfg.harness.policy.approve.word();
            (
                (level != "all").then(|| level.to_string()),
                "all (default)".to_string(),
            )
        }
        EDIT_HARNESS_QUESTION_PRIORITY => {
            let word = cfg.harness.policy.question_priority;
            (
                (word != aterm_agent::supervise::PriorityWord::Recommended)
                    .then(|| word.word().to_string()),
                "recommended (default)".to_string(),
            )
        }
        k if let Some(agent) = agent_switch(k) => {
            boolean(Some(cfg.harness_agent_enabled(agent)), true)
        }
        EDIT_AI_ENABLED => boolean(Some(cfg.ai.enabled()), true),
        EDIT_AI_PREFERRED => {
            let (cli, from) = cfg.ai.settings().preferred;
            (
                (from != aterm_agent::ai::config::From::Default).then(|| cli.id.to_string()),
                format!(
                    "{} (default)",
                    aterm_agent::ai::config::default_preferred().id
                ),
            )
        }
        EDIT_AI_REVIEWS => boolean(Some(cfg.ai.settings().reviews.0), true),
        EDIT_AI_REVIEW_PERMISSIONS => {
            let (choice, from) = cfg.ai.settings().review_permissions;
            (
                (from != aterm_agent::ai::config::From::Default).then(|| choice.word().to_string()),
                "inherit (default)".to_string(),
            )
        }
        EDIT_AI_MACHINE => txt(cfg.ai.local.machine.as_deref(), "none"),
        EDIT_AI_FLEET => {
            let shown = cfg
                .ai
                .local
                .fleet
                .as_ref()
                .map(aterm_agent::ai::config::FleetSource::shown);
            txt(shown.as_deref(), "ai-fleet.toml beside aterm.toml")
        }
        EDIT_AI_FLEET_KEY => txt(cfg.ai.local.fleet_key.as_deref(), "none"),
        k if let Some(cli) = ai_use_switch(k) => {
            boolean(Some(cfg.ai.settings().wants(cli.id)), true)
        }
        "sparkle_words.enabled" => boolean(sw.and_then(|s| s.enabled), true),
        "sparkle_words.reduced_motion" => boolean(sw.and_then(|s| s.reduced_motion), false),
        "sparkle_words.suppress_in_alt_screen" => {
            boolean(sw.and_then(|s| s.suppress_in_alt_screen), false)
        }
        "sparkle_words.lexicon" => txt(sw.and_then(|s| s.lexicon.as_deref()), "builtin only"),
        "sparkle_words.profanity.enabled" => boolean(prof.and_then(|p| p.enabled), true),
        "sparkle_words.profanity.style" => {
            txt(prof.and_then(|p| p.style.as_deref()), "rainbow (default)")
        }
        "sparkle_words.profanity.supernova_chance" => {
            num(prof.and_then(|p| p.supernova_chance), "10 (default)")
        }
        "sparkle_words.profanity.magic" => boolean(prof.and_then(|p| p.magic), true),
        "sparkle_words.profanity.density" => num(prof.and_then(|p| p.density), "3 (default)"),
        "sparkle_words.profanity.anim_ms" => num(prof.and_then(|p| p.anim_ms), "2500 (default)"),
        "sparkle_words.profanity.jitter" => num(prof.and_then(|p| p.jitter), "2 (default)"),
        "sparkle_words.profanity.intensity" => {
            num(prof.and_then(|p| p.intensity), "0.85 (default)")
        }
        "sparkle_words.profanity.bonk" => boolean(prof.and_then(|p| p.bonk), true),
        "sparkle_words.profanity.bonk_detonation" => {
            boolean(prof.and_then(|p| p.bonk_detonation), false)
        }
        "sparkle_words.feline.enabled" => boolean(fel.and_then(|f| f.enabled), true),
        "sparkle_words.feline.style" => txt(fel.and_then(|f| f.style.as_deref()), "cat (default)"),
        "sparkle_words.feline.magic" => boolean(fel.and_then(|f| f.magic), true),
        "sparkle_words.feline.allow_bare_cat" => boolean(fel.and_then(|f| f.allow_bare_cat), true),
        "sparkle_words.feline.cjk_single_char" => {
            boolean(fel.and_then(|f| f.cjk_single_char), false)
        }
        "sparkle_words.feline.log" => boolean(fel.and_then(|f| f.log), true),
        "sparkle_words.ink.enabled" => boolean(ink.and_then(|i| i.enabled), true),
        "sparkle_words.ink.strength" => num(ink.and_then(|i| i.strength), "0.75 (default)"),
        "sparkle_words.ink.sweep_ms" => num(ink.and_then(|i| i.sweep_ms), "2200 (default)"),
        "sparkle_words.ink.loop" => boolean(ink.and_then(|i| i.loop_), false),
        "sparkle_words.emphasis.enabled" => boolean(emph.and_then(|e| e.enabled), true),
        "matrix_rain.enabled" => boolean(mr.and_then(|m| m.enabled), false),
        "matrix_rain.fps" => num(mr.and_then(|m| m.fps), "30 (default)"),
        "matrix_rain.density" => num(mr.and_then(|m| m.density), "6 (default)"),
        "matrix_rain.speed" => num(mr.and_then(|m| m.speed), "5 (default)"),
        "matrix_rain.trail" => num(mr.and_then(|m| m.trail), "5 (default)"),
        "matrix_rain.alpha" => num(mr.and_then(|m| m.alpha), "theme-derived"),
        "matrix_rain.head_alpha" => num(mr.and_then(|m| m.head_alpha), "derived (>= alpha)"),
        "matrix_rain.hue" => txt(
            mr.and_then(|m| m.hue.as_deref()),
            &format!("{MATRIX_RAIN_HUE_UNSET} (default)"),
        ),
        "matrix_rain.mutation_ms" => num(mr.and_then(|m| m.mutation_ms), "133 (default)"),
        "matrix_rain.idle_secs" => num(mr.and_then(|m| m.idle_secs), "8 (default)"),
        "matrix_rain.suppress_in_alt_screen" => {
            boolean(mr.and_then(|m| m.suppress_in_alt_screen), false)
        }
        "matrix_rain.output_material" => boolean(mr.and_then(|m| m.output_material), true),
        "matrix_rain.turn_wave" => boolean(mr.and_then(|m| m.turn_wave), true),
        "matrix_rain.bell_alert" => boolean(mr.and_then(|m| m.bell_alert), true),
        "matrix_rain.exit_tint" => boolean(mr.and_then(|m| m.exit_tint), true),
        "matrix_rain.seed" => num(mr.and_then(|m| m.seed), "0 (stable per-window)"),
        EDIT_OUTPUT_STREAK_SOUND => boolean(Some(cfg.output_streak_sound_or_default()), true),
        EDIT_OUTPUT_STREAK_ENABLED => boolean(
            Some(cfg.output_streak_enabled_or_default()),
            crate::app_config::DEFAULT_DECORATIVE_EFFECTS,
        ),
        _ => (None, String::new()),
    }
}

/// A settings section, the header controls are grouped under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    Appearance,
    Cursor,
    CursorStyles,
    Typography,
    Window,
    Input,
    Terminal,
    Security,
    Packages,
    Harness,
    Updates,
    Sound,
}

impl Section {
    /// Display order of the section headers (top → bottom).
    pub(crate) const ORDER: [Section; 12] = [
        Section::Appearance,
        Section::Cursor,
        Section::CursorStyles,
        Section::Sound,
        Section::Typography,
        Section::Window,
        Section::Input,
        Section::Terminal,
        Section::Security,
        Section::Updates,
        Section::Packages,
        Section::Harness,
    ];

    /// The header label shown above the section's controls.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Cursor => "Cursor & Motion",
            Section::CursorStyles => "Cursor Styles",
            Section::Typography => "Text & Fonts",
            Section::Window => "Window",
            Section::Input => "Keyboard & Input",
            Section::Terminal => "Terminal",
            Section::Security => "Security",
            Section::Packages => "Packages",
            Section::Harness => "Harness",
            Section::Updates => "Software Update",
            Section::Sound => "Sound",
        }
    }

    /// Index of this section in [`Section::ORDER`] (the sort key for grouping rows).
    pub(crate) fn order_index(self) -> usize {
        Section::ORDER.iter().position(|&s| s == self).unwrap_or(0)
    }
}

/// The section a control belongs to, by its key.
pub(crate) fn section_of(key: &str) -> Section {
    if SECURITY_BOOL_KEYS.contains(&key) {
        return Section::Security;
    }
    if matches!(key, EDIT_CURSOR_TRAIL_COLOR | EDIT_CURSOR_TRAIL_ACCENT) {
        return Section::CursorStyles;
    }
    if COLOR_KEYS.contains(&key) {
        return Section::Appearance;
    }
    if key.starts_with("update.") {
        return Section::Updates;
    }
    if key.starts_with("harness.") || key.starts_with("ai.") {
        return Section::Harness;
    }
    if SOUND_MENU_KEYS.contains(&key) {
        return Section::Sound;
    }
    if key.starts_with("sparkle_words.")
        || key.starts_with("matrix_rain.")
        || key.starts_with("output_streak.")
    {
        return Section::Appearance;
    }
    if matches!(
        key,
        EDIT_CURSOR_TRAIL_STYLE | EDIT_CURSOR_PET | EDIT_CURSOR_NYAN_SPRITE
    ) || CURSOR_STYLE_TUNING_KEYS.contains(&key)
    {
        return Section::CursorStyles;
    }
    match key {
        EDIT_THEME | EDIT_WINDOW_THEME | EDIT_WINDOW_COLORSPACE | EDIT_PALETTE_GENERATE => {
            Section::Appearance
        }
        EDIT_SELECTION_INACTIVE
        | EDIT_MINIMUM_CONTRAST
        | EDIT_BOLD_IS_BRIGHT
        | EDIT_FAINT_OPACITY => Section::Appearance,
        EDIT_PALETTE
        | EDIT_BACKGROUND_OPACITY
        | EDIT_BACKGROUND_MATERIAL
        | EDIT_WALLPAPER
        | EDIT_WALLPAPER_DIM
        | EDIT_WALLPAPER_TEXT_TINT => Section::Appearance,
        EDIT_COLUMNS
        | EDIT_LINES
        | EDIT_TAB_STRIP_ROWS
        | EDIT_SHOW_BUILD_BADGE
        | EDIT_ACTIVE_TAB_COLOR
        | EDIT_SPLIT_FOCUS_MARK => Section::Window,
        EDIT_WINDOW_PADDING
        | EDIT_WINDOW_PADDING_TOP
        | EDIT_RESTORE_SESSION
        | EDIT_KEEPER_ENABLED
        | EDIT_SESSION_LOG_JOURNAL
        | EDIT_SESSION_LOG_CLAUSE => Section::Window,
        EDIT_TAB_STATUS
        | EDIT_TAB_STATUS_QUIET_AFTER_MS
        | EDIT_TAB_STATUS_DWELL_MS
        | EDIT_TAB_STATUS_BADGE
        | EDIT_TAB_CONNECTION_BADGE => Section::Window,
        EDIT_COPY_ON_SELECT
        | EDIT_CONFIRM_MULTILINE_PASTE
        | EDIT_OPTION_AS_META
        | EDIT_PREDICTIVE_ECHO
        | EDIT_PREDICTIVE_ECHO_EDITING => Section::Input,
        EDIT_FOCUS_BOOST | EDIT_GPU | EDIT_TEMPORAL_RECORDING => Section::Terminal,
        EDIT_PACKAGES_ENABLED | EDIT_PACKAGES_AUTO_INSTALL | EDIT_REROUTE_ANNOUNCE => {
            Section::Packages
        }
        EDIT_MACHINE_UNIVERSAL_CONTROL => Section::Security,
        EDIT_CURSOR_STYLE
        | EDIT_CURSOR_BLINK
        | EDIT_CURSOR_MOMENTUM_GLOW
        | EDIT_MOTION
        | EDIT_SERIOUS_MODE
        | EDIT_LOAD_ADAPTIVE_MOTION => Section::Cursor,
        EDIT_SHELL | EDIT_SHELL_ARGS | EDIT_AMACS_DEFAULT_EDITOR => Section::Terminal,
        EDIT_FONT_PX
        | EDIT_FONT_FAMILY
        | EDIT_DISPLAY_FONT
        | EDIT_LIGATURES
        | EDIT_LINE_HEIGHT
        | EDIT_FONT_SIZE_SCALE
        | EDIT_ADJUST_BASELINE
        | EDIT_CURSOR_BREAK_LIGATURES
        | EDIT_MERGED_LIGATURES
        | EDIT_FONT_FAMILY_BOLD
        | EDIT_FONT_FAMILY_ITALIC
        | EDIT_FONT_FAMILY_BOLD_ITALIC
        | EDIT_FONT_SYNTHETIC_STYLE
        | EDIT_FALLBACK_FONTS
        | EDIT_SYMBOL_FONT
        | EDIT_EMOJI_FONT
        | EDIT_ADJUST_UNDERLINE_POSITION
        | EDIT_ADJUST_UNDERLINE_THICKNESS
        | EDIT_UNDERLINE_SKIP_DESCENDERS
        | EDIT_TEXT_BLENDING
        | EDIT_FONT_THICKEN
        | EDIT_STEM_GAMMA
        | EDIT_FONT_VARIATION
        | EDIT_FONT_WEIGHT
        | EDIT_FONT_FEATURES
        | EDIT_FONT_WEIGHT_DARK_NUDGE => Section::Typography,
        _ => Section::Terminal,
    }
}

/// The group box a control belongs to inside its [`Section`] pane.
pub(crate) fn group_of(key: &str) -> (&'static str, u8) {
    // The Sound page, before every prefix rule below: the synth voices under the
    // Music effects switch, then the system bell, which that switch and the
    // volume never reach.
    if key == EDIT_BELL_SOUND {
        return ("Terminal bell", 1);
    }
    if SOUND_MENU_KEYS.contains(&key) {
        return ("Sound effects", 0);
    }
    if matches!(key, EDIT_CURSOR_TRAIL_COLOR | EDIT_CURSOR_TRAIL_ACCENT) {
        return ("Trail color", 4);
    }
    if key == EDIT_CURSOR_TRAIL_STYLE {
        return ("Cursor style", 0);
    }
    if key == EDIT_CURSOR_PET {
        return ("Pet", 1);
    }
    if key == EDIT_CURSOR_NYAN_SPRITE {
        return ("Kitty art", 2);
    }
    if COLOR_KEYS.contains(&key) {
        return ("Colors", 1);
    }
    if SECURITY_BOOL_KEYS.contains(&key) {
        return ("Permissions", 0);
    }
    if key == EDIT_MACHINE_UNIVERSAL_CONTROL {
        return ("This Mac", 1);
    }
    if key.starts_with("update.") {
        return ("Automatic updates", 3);
    }
    if key.starts_with("harness.") {
        return ("Harness supervision", 0);
    }
    if key.starts_with("ai.") {
        return ("AI CLIs", 1);
    }
    if key.starts_with("sparkle_words.") {
        return ("Sparkle words", 4);
    }
    if key.starts_with("matrix_rain.") || key.starts_with("output_streak.") {
        return ("Effects", 5);
    }
    match key {
        EDIT_THEME | EDIT_WINDOW_THEME | EDIT_WINDOW_COLORSPACE | EDIT_PALETTE_GENERATE => {
            ("Theme", 0)
        }
        EDIT_PALETTE => ("Colors", 1),
        EDIT_MINIMUM_CONTRAST
        | EDIT_SELECTION_INACTIVE
        | EDIT_BOLD_IS_BRIGHT
        | EDIT_FAINT_OPACITY => ("Text & Contrast", 2),
        EDIT_BACKGROUND_OPACITY | EDIT_BACKGROUND_MATERIAL => ("Background", 3),
        EDIT_WALLPAPER | EDIT_WALLPAPER_DIM | EDIT_WALLPAPER_TEXT_TINT => ("Wallpaper", 4),
        EDIT_SERIOUS_MODE => ("Effect policy", 0),
        EDIT_CURSOR_STYLE | EDIT_CURSOR_BLINK | EDIT_CURSOR_MOMENTUM_GLOW => ("Cursor", 0),
        EDIT_CURSOR_TRAIL_MS
        | EDIT_CURSOR_TRAIL_LENGTH
        | EDIT_CURSOR_TRAIL_INTENSITY
        | EDIT_CURSOR_TRAIL_RADIUS
        | EDIT_CURSOR_TRAIL_RING => ("Trail effect", 1),
        EDIT_MOTION | EDIT_LOAD_ADAPTIVE_MOTION => ("Motion", 3),
        EDIT_CURSOR_TRAIL_BLOOM
        | EDIT_CURSOR_TRAIL_BLOOM_STRENGTH
        | EDIT_CURSOR_TRAIL_BLOOM_RADIUS
        | EDIT_CURSOR_FIRE_SHIMMER
        | EDIT_HDR_GLOW
        | EDIT_CURSOR_GLOW_SDR_BOOST => ("Bloom & glow", 5),
        EDIT_FONT_FAMILY
        | EDIT_FONT_PX
        | EDIT_DISPLAY_FONT
        | EDIT_LINE_HEIGHT
        | EDIT_FONT_SIZE_SCALE
        | EDIT_ADJUST_BASELINE
        | EDIT_ADJUST_UNDERLINE_POSITION
        | EDIT_ADJUST_UNDERLINE_THICKNESS
        | EDIT_UNDERLINE_SKIP_DESCENDERS
        | EDIT_FONT_WEIGHT
        | EDIT_FONT_VARIATION
        | EDIT_FONT_WEIGHT_DARK_NUDGE => ("Font", 0),
        EDIT_LIGATURES
        | EDIT_CURSOR_BREAK_LIGATURES
        | EDIT_MERGED_LIGATURES
        | EDIT_FONT_FEATURES
        | EDIT_TEXT_BLENDING
        | EDIT_FONT_THICKEN
        | EDIT_STEM_GAMMA => ("Rendering", 1),
        EDIT_FONT_FAMILY_BOLD
        | EDIT_FONT_FAMILY_ITALIC
        | EDIT_FONT_FAMILY_BOLD_ITALIC
        | EDIT_FONT_SYNTHETIC_STYLE
        | EDIT_FALLBACK_FONTS
        | EDIT_SYMBOL_FONT
        | EDIT_EMOJI_FONT => ("Styles & fallbacks", 2),
        EDIT_COLUMNS | EDIT_LINES | EDIT_TAB_STRIP_ROWS => ("Size", 0),
        EDIT_TAB_STATUS
        | EDIT_TAB_STATUS_QUIET_AFTER_MS
        | EDIT_TAB_STATUS_DWELL_MS
        | EDIT_TAB_STATUS_BADGE
        | EDIT_TAB_CONNECTION_BADGE => ("Tab Status", 2),
        EDIT_WINDOW_PADDING | EDIT_WINDOW_PADDING_TOP => ("Padding", 2),
        EDIT_ACTIVE_TAB_COLOR
        | EDIT_ACCENT_COLOR
        | EDIT_BORDER_COLOR
        | EDIT_SEARCH_MATCH_COLOR
        | EDIT_SPLIT_FOCUS_MARK => ("Tabs & panes", 3),
        EDIT_SHOW_BUILD_BADGE => ("Build badge", 5),
        EDIT_RESTORE_SESSION
        | EDIT_KEEPER_ENABLED
        | EDIT_SESSION_LOG_JOURNAL
        | EDIT_SESSION_LOG_CLAUSE => ("Session", 4),
        EDIT_COPY_ON_SELECT | EDIT_CONFIRM_MULTILINE_PASTE => ("Clipboard", 0),
        EDIT_OPTION_AS_META | EDIT_PREDICTIVE_ECHO | EDIT_PREDICTIVE_ECHO_EDITING => {
            ("Keyboard", 1)
        }
        EDIT_FOCUS_BOOST | EDIT_GPU | EDIT_TEMPORAL_RECORDING => ("System", 3),
        EDIT_PACKAGES_ENABLED | EDIT_PACKAGES_AUTO_INSTALL | EDIT_REROUTE_ANNOUNCE => {
            ("ALab Tools", 0)
        }
        EDIT_SCROLLBACK | EDIT_SEARCH_HISTORY_LINES => ("Scrollback", 0),
        EDIT_BIDI | EDIT_AMBIGUOUS_WIDTH => ("Text direction & width", 1),
        EDIT_SHELL | EDIT_SHELL_ARGS | EDIT_AMACS_DEFAULT_EDITOR => ("Shell", 2),
        _ => ("General", u8::MAX),
    }
}

/// The Sound page's master switch and volume notes.
pub(crate) const TRAIL_SOUNDS_NOTE: &str = "Off silences every aterm sound but the bell.";
/// [`TRAIL_SOUNDS_NOTE`]'s companion, for the volume's own row.
pub(crate) const TRAIL_SOUND_VOLUME_NOTE: &str = "How loud every aterm sound but the bell plays.";

fn sound_voice_note(key: &str) -> Option<&'static str> {
    Some(match key {
        EDIT_TRAIL_SOUND_STYLE => {
            "Follow the trail plays your cursor style\u{2019}s own sound; any other choice \
             plays that one sound whatever the style."
        }
        EDIT_TONE_MELODY => {
            "The typing melody shifts a little with the mood of what you type, worked out on \
             this computer."
        }
        EDIT_TRAIL_SOUND_BED => "A soft background sound that swells while you type fast.",
        EDIT_TRAIL_SOUND_RIFF => {
            "The song that plays while you hold down one key with a companion cursor style, \
             such as Rainbow kitty; the dance still shows when this is off."
        }
        EDIT_SPARKLE_BONK => "A cartoon bonk as you type a swear word, in any program.",
        EDIT_SPARKLE_BONK_DETONATION => {
            "A bonk when a swear word in a program\u{2019}s output bursts into sparkles."
        }
        EDIT_OUTPUT_STREAK_SOUND => {
            "One soft note with the small comet that greets new output from a program."
        }
        _ => return None,
    })
}
fn permission_note(key: &str) -> Option<&'static str> {
    Some(match key {
        EDIT_ALLOW_OSC52_QUERY => "Off, a program that asks to read the clipboard gets no answer.",
        EDIT_ALLOW_WINDOW_OPS => {
            if cfg!(target_os = "linux") {
                "Off, a program cannot resize the window or learn its title and size."
            } else {
                "Off, a program that asks for the window title or size gets no answer."
            }
        }
        EDIT_ALLOW_NOTIFICATIONS => {
            if cfg!(target_os = "macos") {
                "Off, a program's notifications are dropped. Turning this on asks macOS to allow them."
            } else {
                "Off, a program's notifications are dropped."
            }
        }
        EDIT_ALLOW_PALETTE_RECONFIGURE => {
            "A program's colors last for its session and are never saved; off refuses them."
        }
        EDIT_ALLOW_KITTY_FILE_TRANSFER => {
            "Off, a Kitty image must arrive inline: a program cannot have aterm read a file here."
        }
        _ => return None,
    })
}

const BELL_SOUND_NOTE: &str =
    "The bell is your system's alert sound; Music effects and Sound volume do not change it.";

/// The consequence note for one setting.
pub(crate) fn footnote_of(key: &str) -> Option<&'static str> {
    if key == EDIT_BELL_SOUND {
        return Some(BELL_SOUND_NOTE);
    }
    if let Some(note) = sound_voice_note(key) {
        return Some(note);
    }
    if key == EDIT_TRAIL_SOUNDS {
        return Some(TRAIL_SOUNDS_NOTE);
    }
    if key == EDIT_TRAIL_SOUND_VOLUME {
        return Some(TRAIL_SOUND_VOLUME_NOTE);
    }
    if key == EDIT_SECURE_KEYBOARD_ENTRY {
        return Some("Secure Keyboard Entry hides what you type from other apps.");
    }
    if let Some(note) = permission_note(key) {
        return Some(note);
    }
    Some(match key {
        EDIT_AMACS_DEFAULT_EDITOR => {
            "New tabs set EDITOR to amacs, aterm's own editor, so git commit and an agent's \
             Ctrl-G open their file in an aterm tab. An EDITOR or VISUAL you set yourself always \
             wins."
        }
        EDIT_KEEPER_ENABLED => {
            "Each launch starts the PTY keeper if none runs. Turning this off takes effect next launch; `aterm keeper stop` ends the one running. Reopening a saved layout alone starts new shells."
        }
        EDIT_SESSION_LOG_JOURNAL => {
            "Saves session activity, including command metadata, on disk while session restore is enabled. Removes eligible inactive files toward a 64 MiB / 2,048-file target; live or locked journals can keep it above target. Turning this off takes effect next launch and keeps existing files."
        }
        EDIT_SESSION_LOG_CLAUSE => {
            "Adds current activity to tab tooltips and context menus without saving it to disk."
        }
        "sparkle_words.profanity.enabled" => {
            "Sparkle words: swear words in output burst into sparkles."
        }
        "sparkle_words.feline.enabled" => {
            "Keyword kitties: a little cat peeks out by words like \u{201c}cat\u{201d} and \
             \u{201c}kitty\u{201d}."
        }
        EDIT_MINIMUM_CONTRAST => "1 leaves colors as the theme draws them.",
        EDIT_BACKGROUND_OPACITY => {
            "Translucency needs GPU rendering (the default). Text over a translucent \
             background keeps at least 4.5:1 contrast."
        }
        EDIT_MATRIX_RAIN_ENABLED => {
            "Rain follows activity and drains when idle. View ▸ Matrix Rain overrides it for \
             one session. Serious Mode and Reduce Motion turn it off."
        }
        EDIT_OUTPUT_STREAK_ENABLED => {
            "A small comet greets new output from a program. Serious Mode and Reduce Motion \
             turn it off."
        }
        EDIT_MOTION => motion_auto_footnote(std::env::consts::OS),
        EDIT_WINDOW_THEME => window_theme_auto(std::env::consts::OS).note,
        EDIT_LOAD_ADAPTIVE_MOTION => {
            "Drops decorative effects while drawing falls behind. Scrolling stays smooth."
        }
        EDIT_CURSOR_FIRE_SHIMMER => {
            "The preview can\u{2019}t show the heat shimmer; the terminal draws it."
        }
        EDIT_CURSOR_TRAIL_BLOOM
        | EDIT_CURSOR_TRAIL_BLOOM_STRENGTH
        | EDIT_CURSOR_TRAIL_BLOOM_RADIUS
        | EDIT_CURSOR_GLOW_SDR_BOOST => {
            "Bloom, shimmer, and HDR fall back quietly when the display cannot show them."
        }
        EDIT_FONT_WEIGHT => {
            "Only variable fonts use Variable font weight; other fonts, and a font named at \
             one weight (SF Mono Light), ignore it."
        }
        EDIT_WINDOW_PADDING_TOP => "The top cannot be more than Sides and bottom.",
        EDIT_CONFIRM_MULTILINE_PASTE => {
            "Asks before pasting text with a line break into a program that does not use \
             bracketed paste."
        }
        EDIT_PREDICTIVE_ECHO => {
            "Adaptive shows what you type before a slow connection echoes it, never at a \
             password prompt."
        }
        EDIT_SCROLLBACK => "0 keeps every line.",
        EDIT_SEARCH_HISTORY_LINES => {
            "Find searches this many recent lines; 0 searches only the screen."
        }
        EDIT_BIDI => {
            "Auto shows Arabic and Hebrew lines in reading order; If asked leaves the order \
             to the program."
        }
        EDIT_AMBIGUOUS_WIDTH => "Narrow gives characters like ① or ※ one cell, Wide two.",
        EDIT_MACHINE_UNIVERSAL_CONTROL => {
            "Off keeps the pointer on this Mac: it won\u{2019}t move to a nearby Mac or iPad."
        }
        EDIT_PACKAGES_ENABLED => {
            "Keeps ALab tools and the coding agents aterm manages up to date, starting at once."
        }
        EDIT_PACKAGES_AUTO_INSTALL => {
            "Lets aterm download ALab tools (several GB) at the next check, and new ones as \
             they join. Off installs nothing you did not ask for."
        }
        EDIT_REROUTE_ANNOUNCE => {
            "Prints one line before cargo or rustc runs through the ALab toolchain."
        }
        EDIT_HARNESS_ENABLED => HARNESS_NOTE,
        k if agent_switch(k).is_some() => HARNESS_AGENT_NOTE,
        EDIT_HARNESS_AUTOPILOT => HARNESS_AUTOPILOT_NOTE.as_str(),
        EDIT_HARNESS_LAND_AT => HARNESS_LAND_AT_NOTE,
        EDIT_HARNESS_APPROVE => HARNESS_APPROVE_NOTE,
        EDIT_HARNESS_ANSWER_QUESTIONS | EDIT_HARNESS_QUESTION_PRIORITY => HARNESS_QUESTIONS_NOTE,
        EDIT_AI_PREFERRED => AI_PREFERRED_NOTE,
        EDIT_AI_ENABLED => AI_ENABLED_NOTE,
        EDIT_AI_REVIEWS => AI_REVIEWS_NOTE,
        EDIT_AI_REVIEW_PERMISSIONS => AI_REVIEW_PERMISSIONS_NOTE,
        EDIT_AI_MACHINE => AI_MACHINE_NOTE,
        EDIT_AI_FLEET => AI_FLEET_NOTE,
        EDIT_AI_FLEET_KEY => AI_FLEET_KEY_NOTE,
        k if ai_use_switch(k).is_some() => AI_USE_NOTE,
        _ => return None,
    })
}

/// What the preferred AI CLI is for.
pub(crate) const AI_PREFERRED_NOTE: &str = "The AI CLI that asks when a review names none, \
     and the first one aterm ai doctor checks.";
/// What the look's switch does.
pub(crate) const AI_ENABLED_NOTE: &str = "aterm checks each AI CLI's login and says in \
     Messages when one needs you to sign in.";
/// What reviews are.
pub(crate) const AI_REVIEWS_NOTE: &str = "On, an AI agent here can ask every other AI CLI \
     used here to review its change (aterm ai review); each reviewer has all its tools.";
/// What the reviewers' permission level is.
pub(crate) const AI_REVIEW_PERMISSIONS_NOTE: &str = "Inherit runs each reviewer with the \
     asking agent's own permissions, yolo when you ask; the others pin one level.";
/// What this machine's fleet name is.
pub(crate) const AI_MACHINE_NOTE: &str = "The [machine.<name>] row of the fleet document \
     that applies to this machine.";
/// What the fleet document is.
pub(crate) const AI_FLEET_NOTE: &str = "One document for every machine: a file, or an \
     https:// URL fetched every six hours and checked against the signer's key.";
/// What the fleet signer's key is.
pub(crate) const AI_FLEET_KEY_NOTE: &str = "The key the fleet document must be signed with \
     (aterm ai fleet keygen <secret-key-file> prints one); a URL's document needs it.";
/// What a CLI's own switch does, after the last of them.
pub(crate) const AI_USE_NOTE: &str = "Off, aterm never signs that CLI in here, never asks \
     it for a review and says nothing about it.";

/// What supervision does, under the SUPERVISION card's master switch.
pub(crate) const HARNESS_NOTE: &str = "aterm keeps your coding agents working by answering \
     their permission boxes and questions for you.";
/// What an agent's own switch does, after the last of them.
pub(crate) const HARNESS_AGENT_NOTE: &str = "Off, aterm leaves that agent's sessions alone; \
     Supervise coding agents turns supervision off for all of them.";
/// What the default autopilot's modes mean, for a session with none of its own.
pub(crate) static HARNESS_AUTOPILOT_NOTE: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| {
        let mut note = String::from("For sessions with no autopilot of their own.");
        for words in &crate::autopilot::MODE_WORDS {
            let mut word = words.mode.word().chars();
            note.push_str(" Under ");
            note.extend(word.next().into_iter().flat_map(char::to_uppercase));
            note.push_str(word.as_str());
            note.push_str(", ");
            note.push_str(words.record);
            note.push('.');
        }
        note
    });
/// What the Land at time does: offered, never armed.
pub(crate) const HARNESS_LAND_AT_NOTE: &str = "A time, HH:MM, that each session's Autopilot \
     menu offers: cruise until then, then land. Nothing changes until you pick it.";
/// What the approval segments All, Safe and None mean.
pub(crate) const HARNESS_APPROVE_NOTE: &str = "All says yes to every permission box, for that \
     request only; Safe says yes only to requests aterm can tell are safe, such as reading a \
     file; None leaves every box to you. aterm answers no Codex box: codex-alab answers its own \
     under these same words.";
/// What the question priorities Recommended, Long-term and Short-term mean.
pub(crate) const HARNESS_QUESTIONS_NOTE: &str = "With Answer questions on, Recommended (the \
     default) picks the answer the question marks as recommended; Long-term and Short-term let \
     the agent choose, for lasting quality or for the quickest finish.";

/// A row's note as its box draws it ([`row_notes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowNote<'a> {
    Own(&'a str),
    Shared(&'a str),
}

impl<'a> RowNote<'a> {
    /// The note when it is the row's own.
    pub(crate) fn own(note: Option<Self>) -> Option<&'a str> {
        match note? {
            Self::Own(text) => Some(text),
            Self::Shared(_) => None,
        }
    }

    /// The note when the row closes a run that shares it.
    pub(crate) fn shared(note: Option<Self>) -> Option<&'a str> {
        match note? {
            Self::Shared(text) => Some(text),
            Self::Own(_) => None,
        }
    }
}

/// The note each of `keys` (a box's rows, top to bottom) draws.
pub(crate) fn row_notes(keys: &[&str]) -> Vec<Option<RowNote<'static>>> {
    shared_row_notes(keys.iter().map(|key| footnote_of(key)))
}

/// [`row_notes`] for a box whose rows' notes are `notes` (each row's, top to bottom).
pub(crate) fn shared_row_notes<'a>(
    notes: impl IntoIterator<Item = Option<&'a str>>,
) -> Vec<Option<RowNote<'a>>> {
    let notes: Vec<Option<&'a str>> = notes.into_iter().collect();
    notes
        .iter()
        .enumerate()
        .map(|(position, note)| {
            let note = (*note)?;
            let shares = |other: &Option<&str>| *other == Some(note);
            if notes[position + 1..].iter().any(shares) {
                None
            } else if notes[..position].iter().any(shares) {
                Some(RowNote::Shared(note))
            } else {
                Some(RowNote::Own(note))
            }
        })
        .collect()
}

/// When a saved change takes effect, where that is not at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ApplicationTiming {
    AfterRestart,
    NewSessions,
    WhenAtermClosesOrRestarts,
    NextToolsCheck,
    DailyLaunchApply,
    NextCargoRun,
    NewTextOnly,
    EnablingNeedsNewWindow,
    DevelopmentBuildsOnly,
}

impl ApplicationTiming {
    /// The one plain phrase every surface says for this timing.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::AfterRestart => "Applies after restart",
            Self::NewSessions => "Applies to new sessions",
            Self::WhenAtermClosesOrRestarts => "Applies when aterm closes or restarts",
            Self::NextToolsCheck => "Applies at the next ALab tools check",
            // A start claims the day's apply (aterm's own, or an `aterm` terminal session's);
            // a window a running aterm opens claims nothing.
            Self::DailyLaunchApply => "Applies once a day, when aterm starts",
            Self::NextCargoRun => "Applies to the next cargo or rustc run",
            Self::NewTextOnly => "Applies to new text only",
            Self::EnablingNeedsNewWindow => "Turning it on may need a new window",
            Self::DevelopmentBuildsOnly => "Development builds only",
        }
    }

    /// [`Self::words`] as the clause after a verb ("aterm turns it off at the next ALab tools check").
    pub(crate) fn clause(self) -> &'static str {
        let words = self.words();
        words.strip_prefix("Applies ").unwrap_or(words)
    }

    /// Whether saving under this timing still has an effect at once.
    pub(crate) const fn has_live_effect(self) -> bool {
        matches!(self, Self::NewTextOnly | Self::EnablingNeedsNewWindow)
    }
}

/// When an authored value becomes effective ([`ApplicationTiming`]), where that is not when it is saved.
pub(crate) fn application_timing(key: &str) -> Option<ApplicationTiming> {
    use ApplicationTiming as T;
    Some(match key {
        EDIT_COLUMNS
        | EDIT_LINES
        | EDIT_GPU
        | EDIT_UPDATE_ENABLED
        | EDIT_KEEPER_ENABLED
        | EDIT_SESSION_LOG_JOURNAL => T::AfterRestart,
        EDIT_ALLOW_KITTY_FILE_TRANSFER
        | EDIT_TEMPORAL_RECORDING
        | EDIT_SHELL
        | EDIT_SHELL_ARGS
        | EDIT_AMACS_DEFAULT_EDITOR => T::NewSessions,
        EDIT_HDR_GLOW => T::EnablingNeedsNewWindow,
        EDIT_RESTORE_SESSION => T::WhenAtermClosesOrRestarts,
        EDIT_REROUTE_ANNOUNCE => T::NextCargoRun,
        EDIT_PACKAGES_AUTO_INSTALL
        | "packages.account"
        | "packages.exclude"
        | "packages.links"
        | EDIT_MACHINE_UNIVERSAL_CONTROL => T::NextToolsCheck,
        "update.owner" | "update.repo" => T::DevelopmentBuildsOnly,
        EDIT_AMBIGUOUS_WIDTH => T::NewTextOnly,
        _ => return None,
    })
}

/// When a saved Universal Control `policy` takes effect.
pub(crate) const fn universal_control_timing(
    policy: atpkg::config::UniversalControlPolicy,
    packages_enabled: bool,
) -> Option<ApplicationTiming> {
    match policy {
        atpkg::config::UniversalControlPolicy::Leave => None,
        atpkg::config::UniversalControlPolicy::Off if packages_enabled => {
            Some(ApplicationTiming::NextToolsCheck)
        }
        atpkg::config::UniversalControlPolicy::Off => Some(ApplicationTiming::DailyLaunchApply),
    }
}

/// Whether saving this key has any immediate effect in the running app.
pub(crate) fn application_has_live_effect(key: &str) -> bool {
    application_timing(key).is_none_or(ApplicationTiming::has_live_effect)
}

/// Ambient precedence documented beside the schema.
pub(crate) fn environment_precedence(key: &str) -> Option<&'static str> {
    Some(match key {
        EDIT_COLUMNS => "a launch --columns overrides this value",
        EDIT_LINES => "a launch --lines overrides this value",
        EDIT_GPU => "a launch --cpu or --gpu overrides this value",
        EDIT_FONT_PX => "a launch --font-px overrides this value",
        EDIT_FONT_FAMILY => "a launch --font overrides this value",
        EDIT_SHELL => "a launch --shell overrides this value; -e / --command bypasses the shell",
        EDIT_SHELL_ARGS => "a launch -e / --command bypasses shell_args",
        EDIT_AMACS_DEFAULT_EDITOR => {
            "an EDITOR or VISUAL set in your shell startup files or aterm's environment wins; \
             a shell aterm has no integration for gets no default"
        }
        _ => return None,
    })
}

/// Human label for a security opt-in row (keyed by its `SECURITY_BOOL_KEYS` entry). A name too
/// long for the lines a split row's caption has over its control at the least view ends in the
/// words in parentheses that explain it, so the caption says the name before them whole
/// (`settings_scroll::named_row_parts`): Linux's window-operations name, said as one sentence,
/// took four lines at 2x in the least window (278x264) in Cantarell, Noto Sans and DejaVu Sans,
/// where three fit over its switch, and its caption read "Allow programs to / resize the window /
/// and read its title…".
pub(crate) fn security_label(key: &str) -> &'static str {
    match key {
        EDIT_ALLOW_OSC52_QUERY => "Allow programs to read the clipboard",
        EDIT_SECURE_KEYBOARD_ENTRY => "Secure Keyboard Entry (block keystroke snooping)",
        EDIT_ALLOW_WINDOW_OPS => {
            if cfg!(target_os = "linux") {
                "Allow programs to resize the window (and read its title and size)"
            } else {
                "Allow programs to read the window title and size"
            }
        }
        EDIT_ALLOW_NOTIFICATIONS => "Allow programs to show desktop notifications",
        EDIT_ALLOW_PALETTE_RECONFIGURE => "Allow programs to change the color palette",
        EDIT_ALLOW_KITTY_FILE_TRANSFER => "Allow local files for Kitty graphics",
        _ => "Security option",
    }
}

/// Truthful copy for `motion = "auto"` differs with the shipping platform seam.
pub(crate) fn motion_auto_copy(target_os: &str) -> (&'static str, &'static str) {
    match target_os {
        "macos" => (
            "Automatic follows macOS Reduce Motion as you change it.",
            " · motion=auto follows macOS Reduce Motion live",
        ),
        "windows" => (
            "Automatic follows the Windows animation setting each time a window opens.",
            " · motion=auto samples Show animations in Windows when a window attaches; preference changes are not observed live",
        ),
        "linux" => (
            "Automatic follows the desktop's animation setting each time a window opens.",
            " · motion=auto samples the desktop's animation setting (GNOME's enable-animations, through the Settings portal) when a window attaches; preference changes are not observed live",
        ),
        _ => (
            "Automatic animates fully here: this system has no Reduce Motion setting to follow.",
            " · motion=auto cannot query OS Reduce Motion on this platform and does not reduce motion for an OS preference; choose reduced explicitly",
        ),
    }
}

/// What Automatic window appearance (`window_theme = "auto"`) follows, in the words each place
/// that says it uses ([`window_theme_auto`]).
pub(crate) struct WindowThemeAuto {
    /// What it takes its light or dark from.
    pub(crate) follows: &'static str,
    /// The choice's words in a picker.
    pub(crate) choice: &'static str,
    /// The note under the Window appearance segments, whose first is "Auto".
    pub(crate) note: &'static str,
    /// What follows the system instead, said after the side the window sample paints, or nothing.
    pub(crate) system: &'static str,
    /// Top Settings' sentence over its colors card.
    pub(crate) card: &'static str,
}

/// WHAT AUTOMATIC WINDOW APPEARANCE FOLLOWS ON `target_os`, said in one place for every surface
/// that says it (the picker's choice, the note under the segments, the window sample's spoken
/// value, Top Settings' colors card). On every platform the window takes the terminal theme's
/// side, the side the window sample paints (`settings_preview::window_sample_light`): macOS
/// draws its transparent title bar over the theme-colored window with the toolbar pinned to the
/// theme's side and leaves the window's own appearance unset, so only menus, sheets and alerts
/// follow macOS; Windows resolves its caption, Linux its chrome, from the terminal theme. (The
/// note said "Auto matches the macOS appearance" over a sample drawing Aterm Day's light title
/// bar on a dark Mac.)
pub(crate) fn window_theme_auto(target_os: &str) -> WindowThemeAuto {
    let auto = WindowThemeAuto {
        follows: "the terminal theme",
        choice: "Match terminal",
        note: "Auto matches the terminal theme, light or dark.",
        system: "",
        card: "Choose colors; the window can match the terminal, Light, or Dark.",
    };
    match target_os {
        "macos" => WindowThemeAuto {
            note: "Auto matches the terminal theme, light or dark; menus and sheets follow macOS.",
            system: "; its menus and sheets follow macOS",
            ..auto
        },
        _ => auto,
    }
}

fn motion_auto_footnote(target_os: &str) -> &'static str {
    motion_auto_copy(target_os).0
}

pub(crate) fn motion_auto_help() -> &'static str {
    motion_auto_copy(std::env::consts::OS).1
}

/// Inclusive numeric bounds and step for a bounded numeric control.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Range {
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) step: f64,
}

/// The unit a numeric setting is counted in, as the suffix its number takes (" px", " ms", "%", "×", " cells").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NumericUnit {
    pub(crate) one: &'static str,
    pub(crate) many: &'static str,
}

impl NumericUnit {
    const fn symbol(suffix: &'static str) -> Self {
        Self {
            one: suffix,
            many: suffix,
        }
    }

    /// The suffix `value` takes.
    pub(crate) fn after(self, value: f64) -> &'static str {
        if value == 1.0 { self.one } else { self.many }
    }
}

/// [`NumericUnit`] of `key`, from the one table.
pub(crate) fn numeric_unit(key: &str) -> Option<NumericUnit> {
    Some(match key {
        EDIT_CURSOR_TRAIL_LENGTH | EDIT_CURSOR_TRAIL_RADIUS => NumericUnit {
            one: " cell",
            many: " cells",
        },
        _ => NumericUnit::symbol(numeric_symbol(key)?),
    })
}

fn numeric_symbol(key: &str) -> Option<&'static str> {
    Some(match key {
        EDIT_FONT_PX
        | EDIT_WINDOW_PADDING
        | EDIT_WINDOW_PADDING_TOP
        | EDIT_ADJUST_BASELINE
        | EDIT_ADJUST_UNDERLINE_POSITION
        | EDIT_ADJUST_UNDERLINE_THICKNESS
        | "sparkle_words.profanity.jitter" => " px",
        EDIT_CURSOR_TRAIL_MS
        | EDIT_TAB_STATUS_QUIET_AFTER_MS
        | EDIT_TAB_STATUS_DWELL_MS
        | "sparkle_words.profanity.anim_ms"
        | "sparkle_words.ink.sweep_ms"
        | "matrix_rain.mutation_ms"
        | "privacy.warmup_hold_ms"
        | "privacy.probe_interval_ms" => " ms",
        "matrix_rain.idle_secs" | "output_streak.idle_secs" | "idle_rest_secs" => " s",
        "sparkle_words.profanity.supernova_chance" | "sparkle_words.custom.burst.chance" => "%",
        EDIT_LINE_HEIGHT | EDIT_CURSOR_TRAIL_BLOOM_RADIUS => "\u{00d7}",
        _ => return None,
    })
}

/// Formatted display string for a numeric setting value.
pub(crate) fn numeric_reading(key: &str, value: f64, shown: &str) -> Option<String> {
    if key == EDIT_CURSOR_TRAIL_BLOOM_STRENGTH {
        return Some(format!("{:.0}%", value * 100.0));
    }
    if let Some(unit) = numeric_unit(key) {
        return Some(format!("{shown}{}", unit.after(value)));
    }
    range_of(key)
        .is_some_and(|range| range.min == 0.0 && range.max == 1.0)
        .then(|| format!("{:.0}%", value * 100.0))
}

/// [`numeric_reading`] of a whole number, or the number itself.
pub(crate) fn integer_reading(key: &str, value: i64) -> String {
    let shown = value.to_string();
    #[allow(clippy::cast_precision_loss)] // a count far below 2^52
    numeric_reading(key, value as f64, &shown).unwrap_or(shown)
}

/// [`numeric_reading`] of a stored float, written as the field takes it back, or the number itself.
pub(crate) fn float_reading(key: &str, value: f32) -> String {
    let shown = value.to_string();
    numeric_reading(key, f64::from(value), &shown).unwrap_or(shown)
}

pub(crate) fn range_of(key: &str) -> Option<Range> {
    let r = |min, max, step| Some(Range { min, max, step });
    match key {
        EDIT_FONT_PX => r(6.0, 200.0, 1.0),
        EDIT_CURSOR_TRAIL_MS => r(30.0, 2000.0, 10.0),
        EDIT_CURSOR_TRAIL_LENGTH => r(1.0, 512.0, 1.0),
        EDIT_CURSOR_TRAIL_INTENSITY => r(0.0, 1.0, 0.05),
        EDIT_CURSOR_TRAIL_RADIUS => r(0.0, 2.0, 0.05),
        EDIT_CURSOR_TRAIL_BLOOM_STRENGTH => r(0.0, 3.0, 0.05),
        EDIT_CURSOR_TRAIL_BLOOM_RADIUS => r(0.5, 8.0, 0.1),
        EDIT_CURSOR_GLOW_SDR_BOOST => r(0.0, 1.0, 0.05),
        EDIT_TAB_STRIP_ROWS => r(0.0, 4.0, 1.0),
        EDIT_TAB_STATUS_QUIET_AFTER_MS => r(
            MIN_TAB_STATUS_QUIET_AFTER_MS as f64,
            MAX_TAB_STATUS_QUIET_AFTER_MS as f64,
            50.0,
        ),
        EDIT_TAB_STATUS_DWELL_MS => r(
            MIN_TAB_STATUS_DWELL_MS as f64,
            MAX_TAB_STATUS_DWELL_MS as f64,
            50.0,
        ),
        EDIT_LINE_HEIGHT => r(0.8, 2.0, 0.05),
        EDIT_FONT_SIZE_SCALE => r(0.8, 1.25, 0.01),
        EDIT_MINIMUM_CONTRAST => r(1.0, 21.0, 0.5),
        EDIT_FAINT_OPACITY => r(0.0, 1.0, 0.05),
        EDIT_ADJUST_BASELINE => r(-32.0, 32.0, 1.0),
        EDIT_ADJUST_UNDERLINE_POSITION | EDIT_ADJUST_UNDERLINE_THICKNESS => r(-32.0, 32.0, 1.0),
        EDIT_STEM_GAMMA => r(0.30, 3.0, 0.05),
        EDIT_FONT_WEIGHT => r(1.0, 1000.0, 1.0),
        EDIT_TRAIL_SOUND_VOLUME => r(0.0, 1.0, 0.05),
        EDIT_FONT_WEIGHT_DARK_NUDGE => r(0.0, 300.0, 10.0),
        EDIT_BACKGROUND_OPACITY => r(0.0, 1.0, 0.05),
        EDIT_WALLPAPER_DIM => r(0.0, 1.0, 0.05),
        EDIT_WINDOW_PADDING | EDIT_WINDOW_PADDING_TOP => r(0.0, 64.0, 1.0),
        "sparkle_words.profanity.supernova_chance" => r(0.0, 100.0, 5.0),
        "sparkle_words.profanity.density" => r(1.0, 12.0, 1.0),
        "sparkle_words.profanity.anim_ms" => r(350.0, 10000.0, 50.0),
        "sparkle_words.profanity.jitter" => r(0.0, 6.0, 1.0),
        "sparkle_words.profanity.intensity" | "sparkle_words.feline.intensity" => r(0.0, 1.0, 0.05),
        "sparkle_words.ink.strength" => r(0.0, 1.0, 0.05),
        "sparkle_words.ink.sweep_ms" => r(350.0, 6000.0, 50.0),
        "matrix_rain.fps" => r(12.0, 60.0, 1.0),
        "matrix_rain.density" => r(1.0, 12.0, 1.0),
        "matrix_rain.speed" | "matrix_rain.trail" => r(1.0, 10.0, 1.0),
        "matrix_rain.alpha" | "matrix_rain.head_alpha" => r(16.0, 135.0, 1.0),
        "matrix_rain.mutation_ms" => r(80.0, 2000.0, 10.0),
        "matrix_rain.idle_secs" => r(2.0, 120.0, 1.0),
        _ => None,
    }
}

#[cfg(windows)]
const SHELL_KEYWORDS: &[&str] = &[
    "shell",
    "powershell",
    "pwsh",
    "cmd",
    "wsl",
    "bash",
    "git-bash",
    "nu",
    "nushell",
    "command",
    "login",
    "argv",
];
#[cfg(not(windows))]
const SHELL_KEYWORDS: &[&str] = &["shell", "bash", "zsh", "command", "login", "argv"];

/// The shells it takes ride in an aside, so a narrow field says the short
/// form, `Platform (default)`, instead of cutting the list.
#[cfg(windows)]
const SHELL_PLACEHOLDER: &str =
    "Platform (default) (pwsh, powershell, cmd, bash, wsl, nu, or a path)";
#[cfg(not(windows))]
const SHELL_PLACEHOLDER: &str = "Platform (default)";

fn packages_keywords() -> &'static [&'static str] {
    static WORDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    WORDS.get_or_init(|| {
        let mut words = vec![
            "packages",
            "toolchain",
            "atpkg",
            "tools",
            "automatic",
            "updates",
            "maintenance",
            "master",
            "alab",
            "auto_update",
            "auto-update",
        ];
        words.extend(
            Program::ALL
                .into_iter()
                .filter(|agent| agent.harness_table())
                .map(Program::name),
        );
        words
    })
}

/// Extra search keywords so a fuzzy query finds a control by intent ("dark mode" → `window_theme`, "history" → `scrollback_lines`).
pub(crate) fn keywords_of(key: &str) -> &'static [&'static str] {
    match key {
        EDIT_WINDOW_THEME => &[
            "dark",
            "light",
            "mode",
            "dark mode",
            "light mode",
            "appearance",
            "follow system",
            "system",
        ],
        EDIT_THEME => &[
            "colors",
            "colour",
            "scheme",
            "palette",
            "follow system",
            "follow",
            "system",
            "light",
            "dark",
            "mode",
            "light mode",
            "dark mode",
            "light/dark",
            "pair",
        ],
        EDIT_SCROLLBACK => &["history", "buffer", "lines", "scroll"],
        EDIT_COPY_ON_SELECT => &["clipboard", "selection", "mouse"],
        EDIT_LIGATURES => &["font", "programming", "arrows"],
        EDIT_CURSOR_MOMENTUM_GLOW => &["blink", "glow", "typing", "speed", "momentum", "effect"],
        EDIT_CURSOR_STYLE => &["block", "bar", "beam", "caret", "shape"],
        EDIT_CURSOR_TRAIL_MS
        | EDIT_CURSOR_TRAIL_LENGTH
        | EDIT_CURSOR_TRAIL_INTENSITY
        | EDIT_CURSOR_TRAIL_RADIUS
        | EDIT_CURSOR_TRAIL_RING => &["effect", "motion", "comet", "trail"],
        EDIT_CURSOR_TRAIL_STYLE => &[
            "effect",
            "motion",
            "comet",
            "trail",
            "kitty",
            "rainbow",
            "underline",
        ],
        EDIT_CURSOR_PET => &[
            "pet",
            "cat",
            "dog",
            "kitty",
            "animal",
            "companion",
            "flying",
            "fly",
        ],
        EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF => &["trail", "previous", "last", "off", "restore"],
        EDIT_CURSOR_TRAIL_COLOR | EDIT_CURSOR_TRAIL_ACCENT => {
            &["effect", "trail", "color", "colour", "aurora", "accent"]
        }
        EDIT_TRAIL_SOUNDS | EDIT_TRAIL_SOUND_VOLUME => {
            &["sound", "audio", "sfx", "mute", "volume", "effects"]
        }
        EDIT_TONE_MELODY => &["tone", "mood", "melody", "music", "classifier", "sound"],
        EDIT_TRAIL_SOUND_BED => &["bed", "ambient", "drone", "texture", "background", "sound"],
        EDIT_TRAIL_SOUND_RIFF => &[
            "riff",
            "sing",
            "song",
            "celebration",
            "music",
            "loud",
            "sound",
            "sfx",
        ],
        EDIT_OUTPUT_STREAK_SOUND => &[
            "streak", "output", "prism", "comet", "rainbow", "pip", "chime", "sound", "sfx",
        ],
        EDIT_BELL_SOUND => &[
            "bell",
            "beep",
            "alert",
            "bel",
            "audible",
            "sound",
            "sfx",
            "notification",
        ],
        EDIT_TRAIL_SOUND_STYLE => &[
            "mechanical",
            "keyboard",
            "thock",
            "click",
            "typing",
            "sound",
            "audio",
            "bell",
            "glass",
            "typewriter",
            "marimba",
            "felt",
            "piano",
            "wood",
            "droplet",
            "voice",
            "instrument",
        ],
        EDIT_CURSOR_NYAN_SPRITE => &[
            "kitty", "rainbow", "nyan", "cat", "sprite", "image", "png", "trail",
        ],
        EDIT_CURSOR_TRAIL_BLOOM
        | EDIT_CURSOR_TRAIL_BLOOM_STRENGTH
        | EDIT_CURSOR_TRAIL_BLOOM_RADIUS => &["bloom", "halo", "glow", "gpu", "blur", "trail"],
        EDIT_CURSOR_FIRE_SHIMMER => &["fire", "shimmer", "heat", "haze", "gpu", "refraction"],
        EDIT_HDR_GLOW => &["hdr", "edr", "display", "glow", "bright", "luminous"],
        EDIT_CURSOR_GLOW_SDR_BOOST => &["sdr", "glow", "boost", "crown", "brightness"],
        EDIT_GPU => &["gpu", "renderer", "metal", "vulkan", "cpu", "restart"],
        EDIT_PALETTE => &["palette", "ansi", "colors", "colours", "indexed"],
        EDIT_SHELL | EDIT_SHELL_ARGS => SHELL_KEYWORDS,
        EDIT_AMACS_DEFAULT_EDITOR => &[
            "editor", "amacs", "visual", "git", "commit", "ctrl-g", "emacs", "vim",
        ],
        EDIT_WINDOW_COLORSPACE => &["colorspace", "srgb", "p3", "gamut", "color"],
        EDIT_RESTORE_SESSION => &["restore", "session", "reopen", "launch", "windows"],
        EDIT_KEEPER_ENABLED => &["keeper", "recovery", "crash", "shell", "session"],
        EDIT_SESSION_LOG_JOURNAL => &["history", "journal", "activity", "privacy", "session"],
        EDIT_SESSION_LOG_CLAUSE => &["activity", "tooltip", "tab", "session"],
        EDIT_FONT_FEATURES => &["opentype", "features", "ss01", "zero", "stylistic"],
        EDIT_FONT_WEIGHT_DARK_NUDGE => &["weight", "dark", "nudge", "wght", "theme"],
        EDIT_BACKGROUND_OPACITY | EDIT_BACKGROUND_MATERIAL => &[
            "opacity",
            "transparent",
            "transparency",
            "translucent",
            "translucency",
            "see-through",
            "glass",
            "vibrancy",
            "blur",
            "material",
        ],
        EDIT_WALLPAPER | EDIT_WALLPAPER_DIM | EDIT_WALLPAPER_TEXT_TINT => &[
            "wallpaper",
            "background",
            "image",
            "backdrop",
            "picture",
            "photo",
        ],
        EDIT_TEMPORAL_RECORDING => &[
            "rewind",
            "temporal",
            "recording",
            "replay",
            "history",
            "time",
        ],
        EDIT_WINDOW_PADDING | EDIT_WINDOW_PADDING_TOP => &[
            "padding", "margin", "border", "spacing", "inset", "edge", "sides", "bottom", "top",
        ],
        EDIT_CONFIRM_MULTILINE_PASTE => &["paste", "safety", "multiline"],
        EDIT_OPTION_AS_META => &["alt", "meta", "keyboard"],
        EDIT_BIDI => &[
            "rtl",
            "right-to-left",
            "direction",
            "arabic",
            "hebrew",
            "bidirectional",
        ],
        EDIT_AMBIGUOUS_WIDTH => &[
            "east",
            "asian",
            "cjk",
            "width",
            "unicode",
            "emoji width",
            "double width",
        ],
        EDIT_PREDICTIVE_ECHO => &[
            "typing",
            "latency",
            "prediction",
            "local echo",
            "echo",
            "ssh",
            "remote",
        ],
        EDIT_PREDICTIVE_ECHO_EDITING => &[
            "editing",
            "arrow",
            "cursor",
            "backspace",
            "prediction",
            "latency",
            "ssh",
        ],
        EDIT_FOCUS_BOOST => &["priority", "qos", "latency", "windows", "starvation"],
        EDIT_MATRIX_RAIN_ENABLED => &[
            "matrix",
            "rain",
            "phosphor",
            "effect",
            "screensaver",
            "green",
        ],
        EDIT_OUTPUT_STREAK_ENABLED => &["streak", "prism", "wake", "comet", "output", "effects"],
        EDIT_PACKAGES_ENABLED => packages_keywords(),
        EDIT_UPDATE_ENABLED => &[
            "update",
            "updates",
            "check",
            "auto-check",
            "automatic",
            "automatically",
            "background",
            "software",
            "release",
        ],
        "update.auto_apply" => &[
            "update",
            "updates",
            "install",
            "auto-install",
            "automatic",
            "automatically",
            "apply",
            "software",
            "release",
        ],
        EDIT_REROUTE_ANNOUNCE => &[
            "reroute",
            "cargo",
            "rustc",
            "signpost",
            "announce",
            "quiet",
            "toolchain",
            "trust",
        ],
        EDIT_MACHINE_UNIVERSAL_CONTROL => &[
            "machine",
            "universal control",
            "cursor",
            "ipad",
            "mac",
            "host",
            "roam",
        ],
        EDIT_PACKAGES_AUTO_INSTALL => &[
            "packages",
            "toolchain",
            "atpkg",
            "install",
            "bootstrap",
            "seed",
            "batteries",
            "first run",
            "alab",
            "seed_install",
        ],
        EDIT_FONT_FAMILY | EDIT_FONT_PX => &["typeface", "size", "text"],
        EDIT_DISPLAY_FONT => &[
            "display",
            "display faces",
            "faces",
            "chunky",
            "pixel",
            "engraved",
            "bubble",
            "minecraft",
            "retro",
            "game",
            "fun",
        ],
        EDIT_FONT_FAMILY_BOLD | EDIT_FONT_FAMILY_ITALIC | EDIT_FONT_FAMILY_BOLD_ITALIC => {
            &["bold", "italic", "weight", "style", "typeface"]
        }
        EDIT_FONT_SYNTHETIC_STYLE => &["synthetic", "bold", "italic", "fake", "dilate"],
        EDIT_FALLBACK_FONTS => &["fallback", "unicode", "cjk", "chain", "coverage"],
        EDIT_SYMBOL_FONT => &["symbols", "math", "glyphs", "fallback"],
        EDIT_EMOJI_FONT => &["emoji", "color", "colour", "fallback"],
        EDIT_LINE_HEIGHT => &["spacing", "leading", "rows", "height"],
        EDIT_FONT_SIZE_SCALE => &["scale", "zoom", "size", "bigger", "smaller"],
        EDIT_ADJUST_BASELINE => &["baseline", "vertical", "shift"],
        EDIT_ADJUST_UNDERLINE_POSITION | EDIT_ADJUST_UNDERLINE_THICKNESS => {
            &["underline", "position", "thickness", "decoration"]
        }
        EDIT_UNDERLINE_SKIP_DESCENDERS => &["underline", "descender", "skip", "ink", "gap"],
        EDIT_MINIMUM_CONTRAST => &["contrast", "legibility", "wcag", "accessibility"],
        EDIT_SELECTION_INACTIVE => &["selection", "focus", "dim", "unfocused"],
        EDIT_SPLIT_FOCUS_MARK => &["split", "pane", "focus", "active", "border", "divider"],
        EDIT_CURSOR_BREAK_LIGATURES => &["ligature", "cursor", "break"],
        EDIT_MERGED_LIGATURES => &["ligature", "merged", "cascadia", "collapse", "n:1"],
        EDIT_BOLD_IS_BRIGHT => &["bold", "bright", "ansi", "promote"],
        EDIT_PALETTE_GENERATE => &["256", "palette", "cube", "xterm", "grey", "gray", "ramp"],
        EDIT_FAINT_OPACITY => &["dim", "faint", "sgr2", "opacity"],
        EDIT_TEXT_BLENDING => &[
            "blending",
            "antialias",
            "aa",
            "weight",
            "gamma",
            "smoothing",
        ],
        EDIT_FONT_THICKEN => &["thicken", "smoothing", "weight", "coretext", "bold"],
        EDIT_STEM_GAMMA => &["stem", "gamma", "weight", "thickness", "thin", "thick"],
        EDIT_FONT_VARIATION => &["variable", "axes", "wght", "opsz", "variation"],
        EDIT_FONT_WEIGHT => &["weight", "wght", "variable", "light", "bold", "thin"],
        EDIT_MOTION => &["motion", "reduce", "animation", "accessibility", "a11y"],
        EDIT_SECURE_KEYBOARD_ENTRY => &[
            "secure",
            "keyboard",
            "keylogger",
            "snooping",
            "password",
            "privacy",
            "security",
        ],
        EDIT_SERIOUS_MODE => &[
            "serious",
            "focus",
            "professional",
            "mute",
            "sound",
            "effects",
            "fun",
        ],
        EDIT_LOAD_ADAPTIVE_MOTION => &[
            "motion",
            "performance",
            "shedding",
            "adaptive",
            "load",
            "effects",
        ],
        EDIT_FOREGROUND
        | EDIT_BACKGROUND
        | EDIT_CURSOR_COLOR
        | EDIT_SELECTION_COLOR
        | EDIT_SELECTION_FOREGROUND => &["color", "colour"],
        EDIT_ACCENT_COLOR => &["color", "colour", "accent", "highlight", "chrome"],
        EDIT_BORDER_COLOR => &["color", "colour", "border", "divider", "seam", "split"],
        EDIT_SEARCH_MATCH_COLOR => &["color", "colour", "search", "find", "match", "highlight"],
        EDIT_ALLOW_OSC52_QUERY => &[
            "security",
            "permission",
            "allow",
            "clipboard",
            "paste",
            "osc 52",
            "osc52",
        ],
        EDIT_ALLOW_WINDOW_OPS => &[
            "security",
            "permission",
            "allow",
            "window",
            "title",
            "size",
            "xtwinops",
            "csi t",
        ],
        EDIT_ALLOW_PALETTE_RECONFIGURE => &[
            "security",
            "permission",
            "allow",
            "palette",
            "colors",
            "colours",
            "indexed colors",
            "osc 4",
            "osc 21",
        ],
        k if SECURITY_BOOL_KEYS.contains(&k) => &["security", "permission", "allow"],
        EDIT_SHOW_BUILD_BADGE => &["version", "build", "badge", "chrome"],
        EDIT_TAB_STATUS | EDIT_TAB_STATUS_BADGE => &[
            "tab status",
            "status",
            "badge",
            "indicator",
            "busy",
            "attention",
            "running",
            "failed",
            "session",
        ],
        EDIT_TAB_CONNECTION_BADGE => &[
            "connection",
            "session connection",
            "badge",
            "mark",
            "outbound",
            "inbound",
            "peer",
            "tab",
        ],
        EDIT_TAB_STATUS_QUIET_AFTER_MS => &[
            "tab status",
            "quiet",
            "stall",
            "idle",
            "timeout",
            "milliseconds",
            "busy",
        ],
        EDIT_TAB_STATUS_DWELL_MS => &[
            "tab status",
            "dwell",
            "hysteresis",
            "flap",
            "debounce",
            "settle",
            "milliseconds",
        ],
        k if k.starts_with("update.") => &["update", "channel", "github", "release"],
        EDIT_HARNESS_AUTOPILOT => &[
            "harness",
            "claude",
            "autopilot",
            "supervisor",
            "cruise",
            "land",
            "wrap",
            "wrap up",
            "forever",
            "infinity",
            "off",
        ],
        EDIT_HARNESS_LAND_AT => &[
            "harness",
            "autopilot",
            "land",
            "land at",
            "landing",
            "time",
            "timer",
            "until",
            "cruise until",
            "wrap up",
            "end of day",
            "schedule",
        ],
        EDIT_HARNESS_APPROVE => &[
            "harness",
            "claude",
            "approve",
            "approval",
            "permission",
            "prompt",
            "dialog",
            "yes",
            "auto approve",
            "proceed",
            "supervisor",
        ],
        EDIT_HARNESS_ANSWER_QUESTIONS => &[
            "harness",
            "claude",
            "question",
            "questions",
            "answer",
            "askuserquestion",
            "recommended",
            "recommendation",
            "dialog",
            "choose",
            "auto answer",
        ],
        EDIT_HARNESS_QUESTION_PRIORITY => &[
            "harness",
            "claude",
            "question",
            "questions",
            "priority",
            "question priority",
            "long-term",
            "short-term",
            "long term",
            "short term",
            "askuserquestion",
            "recommended",
            "decide",
        ],
        k if agent_switch(k).is_some() => &["harness", "agent", "supervise", "disable", "off"],
        EDIT_AI_PREFERRED => &["ai", "cli", "preferred", "main", "default", "review"],
        EDIT_AI_ENABLED => &["ai", "cli", "login", "sign in", "doctor", "messages"],
        EDIT_AI_REVIEWS => &["ai", "cli", "review", "reviewer", "second opinion"],
        EDIT_AI_REVIEW_PERMISSIONS => {
            &["ai", "review", "permissions", "yolo", "inherit", "sandbox"]
        }
        EDIT_AI_MACHINE => &["ai", "fleet", "machine", "name", "host"],
        EDIT_AI_FLEET => &["ai", "fleet", "document", "url", "path", "central"],
        EDIT_AI_FLEET_KEY => &["ai", "fleet", "key", "signer", "signature"],
        k if ai_use_switch(k).is_some() => &["ai", "cli", "use", "login", "review", "off"],
        k if k.starts_with("harness.") => &[
            "harness",
            "wrapper",
            "claude",
            "agent",
            "disable",
            "off",
            "kill switch",
        ],
        EDIT_SPARKLE_BONK | EDIT_SPARKLE_BONK_DETONATION => &[
            "bonk",
            "curse",
            "profanity",
            "swear",
            "sound",
            "sfx",
            "sparkle",
            "words",
        ],
        "sparkle_words.profanity.enabled" => &[
            "sparkle",
            "words",
            "swear",
            "swearing",
            "profanity",
            "curse",
            "cursing",
            "effects",
        ],
        "sparkle_words.feline.enabled" => &[
            "kitty", "kitties", "cat", "cats", "feline", "keyword", "words", "effects",
        ],
        k if k.starts_with("sparkle_words.") => &["sparkle", "words", "decorations", "effects"],
        k if k.starts_with("matrix_rain.") => &["matrix", "rain", "phosphor", "effects"],
        _ => &[],
    }
}

/// Build the editable field specs (label, key, kind, seed, placeholder) the Settings surfaces render, grouped by [`Section`].
pub(crate) fn editable_fields_with(
    cfg: &Config,
    typo: &crate::resolved_typography::ResolvedTypography,
    theme_label: &str,
) -> Vec<EditField> {
    let mut fields = editable_fields(cfg);
    apply_theme_typography(&mut fields, cfg, typo, theme_label);
    fields
}

/// The value a theme's typography gives `key` where `cfg` leaves it unset, as the theme file writes it.
pub(crate) fn theme_value_for(
    cfg: &Config,
    typo: &crate::resolved_typography::ResolvedTypography,
    key: &str,
) -> Option<String> {
    let spec = aterm_types::scheme::THEME_KEYS.iter().find(|spec| {
        spec.group == aterm_types::scheme::ThemeKeyGroup::Typography && spec.name == key
    })?;
    if crate::resolved_typography::config_sets(cfg, spec.key) {
        return None;
    }
    typo.applied.value_text(spec.key)
}

fn apply_theme_typography(
    fields: &mut [EditField],
    cfg: &Config,
    typo: &crate::resolved_typography::ResolvedTypography,
    theme_label: &str,
) {
    for field in fields {
        let Some(value) = theme_value_for(cfg, typo, field.key) else {
            continue;
        };
        if field.key == EDIT_FONT_FAMILY
            && aterm_render::display_face_ids_for_family(&value).is_some()
        {
            continue;
        }
        if matches!(field.kind, EditKind::Bool) {
            field.seed = Some(value);
        } else {
            field.placeholder = format!("{value} (from theme {theme_label})");
        }
    }
}

pub(crate) fn editable_fields(cfg: &Config) -> Vec<EditField> {
    let font_family = configured_str(cfg.font_family.as_deref());
    let display_font = configured_str(cfg.display_font.as_deref());
    let active_tab_color = configured_str(cfg.active_tab_color.as_deref());
    let accent_color = configured_str(cfg.accent_color.as_deref());
    let border_color = configured_str(cfg.border_color.as_deref());
    let search_match_color = configured_str(cfg.search_match_color.as_deref());
    let font_family_bold = configured_str(cfg.font_family_bold.as_deref());
    let font_family_italic = configured_str(cfg.font_family_italic.as_deref());
    let font_family_bold_italic = configured_str(cfg.font_family_bold_italic.as_deref());
    let fallback_fonts = cfg
        .fallback_fonts
        .as_ref()
        .filter(|l| !l.0.is_empty())
        .map(|l| l.0.join(", "));
    let symbol_font = configured_str(cfg.symbol_font.as_deref());
    let emoji_font = configured_str(cfg.emoji_font.as_deref());
    let font_variation = cfg
        .font_variation
        .as_ref()
        .filter(|l| !l.0.is_empty())
        .map(|l| l.0.join(", "));
    let theme = configured_str(cfg.theme.as_deref());
    let cursor_style = configured_str(cfg.cursor_style.as_deref());
    let cursor_trail_style = configured_str(cfg.cursor_trail_style.as_deref());
    let cursor_pet = configured_str(cfg.cursor_pet.as_deref());
    let cursor_trail_color = configured_str(cfg.cursor_trail_color.as_deref());
    let cursor_trail_accent = configured_str(cfg.cursor_trail_accent.as_deref());
    let classic_in_cursor_color = GlowStyle::style_names_classic_mono(cfg.cursor_trail_style_raw());
    let cursor_nyan_sprite = configured_str(cfg.cursor_nyan_sprite.as_deref());
    let foreground = configured_str(cfg.foreground.as_deref());
    let background = configured_str(cfg.background.as_deref());
    let cursor_color = configured_str(cfg.cursor_color.as_deref());
    let selection_color = configured_str(cfg.selection_color.as_deref());
    let selection_foreground = configured_str(cfg.selection_foreground.as_deref());
    let predictive_echo = configured_str(cfg.predictive_echo.as_deref());
    let mut fields = vec![
        EditField {
            label: "Font family",
            key: EDIT_FONT_FAMILY,
            kind: EditKind::Text,
            placeholder: font_family
                .clone()
                .unwrap_or_else(|| "Automatic".to_string()),
            seed: font_family,
        },
        EditField {
            label: "Font size",
            key: EDIT_FONT_PX,
            kind: EditKind::Float,
            seed: cfg.font_px.map(|px| format!("{px}")),
            // What the field reads unset, whatever the file holds now: a write in flight that
            // removes the key (a step onto the automatic size) shows the automatic size. Before,
            // a set size's own "14 px" stood in, the slider read the old size while the removal
            // flew, and the next step wrote the automatic size again instead of moving past it.
            placeholder: format!("{} (automatic)", crate::FONT_PX),
        },
        EditField {
            label: "Display face",
            key: EDIT_DISPLAY_FONT,
            kind: EditKind::Enum {
                options: DISPLAY_FONT_OPTIONS,
            },
            placeholder: display_font
                .clone()
                .unwrap_or_else(|| "off (default font)".to_string()),
            seed: display_font,
        },
        EditField {
            label: "Bold font family",
            key: EDIT_FONT_FAMILY_BOLD,
            kind: EditKind::Text,
            placeholder: font_family_bold
                .clone()
                .unwrap_or_else(|| "Automatic".to_string()),
            seed: font_family_bold,
        },
        EditField {
            label: "Italic font family",
            key: EDIT_FONT_FAMILY_ITALIC,
            kind: EditKind::Text,
            placeholder: font_family_italic
                .clone()
                .unwrap_or_else(|| "Automatic".to_string()),
            seed: font_family_italic,
        },
        EditField {
            label: "Bold italic font family",
            key: EDIT_FONT_FAMILY_BOLD_ITALIC,
            kind: EditKind::Text,
            placeholder: font_family_bold_italic
                .clone()
                .unwrap_or_else(|| "Automatic".to_string()),
            seed: font_family_bold_italic,
        },
        EditField {
            label: "Synthetic bold/italic",
            key: EDIT_FONT_SYNTHETIC_STYLE,
            kind: EditKind::Bool,
            seed: Some(cfg.font_synthetic_style.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Fallback fonts",
            key: EDIT_FALLBACK_FONTS,
            kind: EditKind::Text,
            placeholder: fallback_fonts
                .clone()
                .unwrap_or_else(|| "Automatic".to_string()),
            seed: fallback_fonts,
        },
        EditField {
            label: "Symbol font",
            key: EDIT_SYMBOL_FONT,
            kind: EditKind::Text,
            placeholder: symbol_font
                .clone()
                .unwrap_or_else(|| "Automatic".to_string()),
            seed: symbol_font,
        },
        EditField {
            label: "Emoji font",
            key: EDIT_EMOJI_FONT,
            kind: EditKind::Text,
            placeholder: emoji_font
                .clone()
                .unwrap_or_else(|| "Automatic".to_string()),
            seed: emoji_font,
        },
        EditField {
            label: "Ligatures",
            key: EDIT_LIGATURES,
            kind: EditKind::Bool,
            seed: Some(cfg.ligatures.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Line height",
            key: EDIT_LINE_HEIGHT,
            kind: EditKind::Float,
            seed: cfg.line_height.map(|v| format!("{v}")),
            placeholder: match cfg.line_height {
                Some(v) => format!("{v}×"),
                None => format!("{:.1} (default)", crate::app_config::DEFAULT_LINE_HEIGHT),
            },
        },
        EditField {
            label: "Text size scale",
            key: EDIT_FONT_SIZE_SCALE,
            kind: EditKind::Float,
            seed: cfg.font_size_scale.map(|v| format!("{v}")),
            placeholder: match cfg.font_size_scale {
                Some(v) => format!("{v}×"),
                None => "1 (default)".to_string(),
            },
        },
        EditField {
            label: "Adjust baseline",
            key: EDIT_ADJUST_BASELINE,
            kind: EditKind::Integer,
            seed: cfg.adjust_baseline.map(|v| v.to_string()),
            placeholder: cfg
                .adjust_baseline
                .map_or_else(|| "0 (default)".to_string(), |v| v.to_string()),
        },
        EditField {
            label: "Adjust underline position",
            key: EDIT_ADJUST_UNDERLINE_POSITION,
            kind: EditKind::Integer,
            seed: cfg.adjust_underline_position.map(|v| v.to_string()),
            placeholder: cfg
                .adjust_underline_position
                .map_or_else(|| "0 (default)".to_string(), |v| v.to_string()),
        },
        EditField {
            label: "Adjust underline thickness",
            key: EDIT_ADJUST_UNDERLINE_THICKNESS,
            kind: EditKind::Integer,
            seed: cfg.adjust_underline_thickness.map(|v| v.to_string()),
            placeholder: cfg
                .adjust_underline_thickness
                .map_or_else(|| "0 (default)".to_string(), |v| v.to_string()),
        },
        EditField {
            label: "Skip underline over descenders",
            key: EDIT_UNDERLINE_SKIP_DESCENDERS,
            kind: EditKind::Bool,
            seed: Some(cfg.underline_skip_descenders_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Break ligatures at cursor",
            key: EDIT_CURSOR_BREAK_LIGATURES,
            kind: EditKind::Bool,
            seed: Some(cfg.cursor_break_ligatures.unwrap_or(false).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Merged (Cascadia N:1) ligatures",
            key: EDIT_MERGED_LIGATURES,
            kind: EditKind::Bool,
            seed: Some(cfg.merged_ligatures.unwrap_or(false).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Minimum contrast",
            key: EDIT_MINIMUM_CONTRAST,
            kind: EditKind::Float,
            seed: cfg.minimum_contrast.map(|v| format!("{v}")),
            placeholder: match cfg.minimum_contrast {
                Some(v) => format!("{v}:1"),
                None if cfg!(windows) => "auto (4.5:1 ANSI)".to_string(),
                None => "1 (off)".to_string(),
            },
        },
        EditField {
            label: "Dim selection when unfocused",
            key: EDIT_SELECTION_INACTIVE,
            kind: EditKind::Bool,
            seed: Some(cfg.selection_inactive_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Mark the active split pane",
            key: EDIT_SPLIT_FOCUS_MARK,
            kind: EditKind::Bool,
            seed: Some(cfg.split_focus_mark_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Bold uses bright colors",
            key: EDIT_BOLD_IS_BRIGHT,
            kind: EditKind::Bool,
            seed: Some(cfg.bold_is_bright.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Faint text opacity",
            key: EDIT_FAINT_OPACITY,
            kind: EditKind::Float,
            seed: cfg.faint_opacity.map(|v| format!("{v}")),
            placeholder: match cfg.faint_opacity {
                Some(v) => format!("{v}"),
                None => "0.5 (default)".to_string(),
            },
        },
        EditField {
            label: "Text blending",
            key: EDIT_TEXT_BLENDING,
            kind: EditKind::Enum {
                options: TEXT_BLENDINGS,
            },
            placeholder: configured_str(cfg.text_blending.as_deref())
                .unwrap_or_else(|| "linear-corrected (default)".to_string()),
            seed: configured_str(cfg.text_blending.as_deref()),
        },
        EditField {
            label: "Thicken font strokes",
            key: EDIT_FONT_THICKEN,
            kind: EditKind::Bool,
            seed: Some(cfg.font_thicken_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Stem weight (gamma)",
            key: EDIT_STEM_GAMMA,
            kind: EditKind::Float,
            seed: cfg.stem_gamma.map(|v| format!("{v}")),
            placeholder: match cfg.stem_gamma {
                Some(v) => format!("{v}"),
                None => "1 (default)".to_string(),
            },
        },
        EditField {
            label: "Variable font weight",
            key: EDIT_FONT_WEIGHT,
            kind: EditKind::Integer,
            seed: cfg.font_weight.map(|n| n.to_string()),
            placeholder: cfg
                .font_weight
                .map_or_else(|| "400 (default)".to_string(), |n| n.to_string()),
        },
        EditField {
            label: "Font variations",
            key: EDIT_FONT_VARIATION,
            kind: EditKind::Text,
            placeholder: font_variation
                .clone()
                .unwrap_or_else(|| "Regular (wght=400)".to_string()),
            seed: font_variation,
        },
        EditField {
            label: "Color theme",
            key: EDIT_THEME,
            kind: EditKind::Theme,
            placeholder: theme.clone().unwrap_or_else(|| "Default".to_string()),
            seed: theme,
        },
        EditField {
            label: "Cursor shape",
            key: EDIT_CURSOR_STYLE,
            kind: EditKind::Enum {
                options: CURSOR_STYLES,
            },
            placeholder: cursor_style
                .clone()
                .unwrap_or_else(|| format!("{DEFAULT_CURSOR_STYLE} (default)")),
            seed: cursor_style,
        },
        EditField {
            label: "Cursor blink",
            key: EDIT_CURSOR_BLINK,
            kind: EditKind::Bool,
            seed: Some(cfg.cursor_blink.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Typing momentum glow",
            key: EDIT_CURSOR_MOMENTUM_GLOW,
            kind: EditKind::Bool,
            seed: Some(cfg.cursor_momentum_glow_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Cursor style",
            key: EDIT_CURSOR_TRAIL_STYLE,
            kind: EditKind::Enum {
                options: CURSOR_TRAIL_STYLES,
            },
            placeholder: cursor_trail_style.clone().unwrap_or_else(|| {
                format!(
                    "{} (default)",
                    crate::app_config::default_cursor_trail_style()
                )
            }),
            seed: (cursor_trail_style.is_some() || cfg.cursor_trail.is_some())
                .then(|| cfg.cursor_trail_style_raw().to_string()),
        },
        EditField {
            label: "Pet",
            key: EDIT_CURSOR_PET,
            kind: EditKind::Enum {
                options: CURSOR_PETS,
            },
            placeholder: cursor_pet.clone().unwrap_or_else(|| {
                format!("{} (default)", crate::app_config::default_cursor_pet())
            }),
            seed: (cursor_pet.is_some() || cfg.cursor_trail.is_some())
                .then(|| cfg.cursor_pet_raw().to_string()),
        },
        EditField {
            label: "Cursor style before Off",
            key: EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF,
            kind: EditKind::Enum {
                options: CURSOR_TRAIL_STYLES,
            },
            seed: None,
            placeholder: String::new(),
        },
        EditField {
            label: "Music effects",
            key: EDIT_TRAIL_SOUNDS,
            kind: EditKind::Bool,
            seed: Some(cfg.trail_sounds_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Sound volume",
            key: EDIT_TRAIL_SOUND_VOLUME,
            kind: EditKind::Float,
            seed: cfg.trail_sound_volume.map(|v| format!("{v}")),
            placeholder: match cfg.trail_sound_volume {
                Some(v) => format!("{v}"),
                None => "0.4 (default)".to_string(),
            },
        },
        EditField {
            label: "Tone-adaptive melody",
            key: EDIT_TONE_MELODY,
            kind: EditKind::Bool,
            seed: Some(cfg.tone_melody_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Ambient sound bed",
            key: EDIT_TRAIL_SOUND_BED,
            kind: EditKind::Bool,
            seed: Some(cfg.trail_sound_bed_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Typing sound",
            key: EDIT_TRAIL_SOUND_STYLE,
            kind: EditKind::Enum {
                options: TRAIL_SOUND_STYLES,
            },
            seed: cfg.trail_sound_style.clone(),
            placeholder: cfg
                .trail_sound_style
                .clone()
                .unwrap_or_else(|| format!("{DEFAULT_TRAIL_SOUND_STYLE} (default)")),
        },
        EditField {
            label: "Sing-along riff",
            key: EDIT_TRAIL_SOUND_RIFF,
            kind: EditKind::Bool,
            seed: Some(cfg.trail_sound_riff_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Terminal bell sound",
            key: EDIT_BELL_SOUND,
            kind: EditKind::Bool,
            seed: Some(cfg.bell_sound_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Trail duration",
            key: EDIT_CURSOR_TRAIL_MS,
            kind: EditKind::Integer,
            seed: cfg.cursor_trail_ms.map(|n| n.to_string()),
            placeholder: match cfg.cursor_trail_ms {
                Some(n) => {
                    integer_reading(EDIT_CURSOR_TRAIL_MS, i64::try_from(n).unwrap_or(i64::MAX))
                }
                None => format!("{} (default)", integer_reading(EDIT_CURSOR_TRAIL_MS, 260)),
            },
        },
        EditField {
            label: "Trail length",
            key: EDIT_CURSOR_TRAIL_LENGTH,
            kind: EditKind::Integer,
            seed: cfg.cursor_trail_length.map(|n| n.to_string()),
            placeholder: match cfg.cursor_trail_length {
                Some(n) => integer_reading(
                    EDIT_CURSOR_TRAIL_LENGTH,
                    i64::try_from(n).unwrap_or(i64::MAX),
                ),
                None => format!(
                    "{} (default)",
                    integer_reading(EDIT_CURSOR_TRAIL_LENGTH, 24)
                ),
            },
        },
        EditField {
            label: "Trail intensity",
            key: EDIT_CURSOR_TRAIL_INTENSITY,
            kind: EditKind::Float,
            seed: cfg.cursor_trail_intensity.map(|v| v.to_string()),
            placeholder: match cfg.cursor_trail_intensity {
                Some(v) => v.to_string(),
                None => format!(
                    "{:.1} (default)",
                    cfg.cursor_trail_intensity_for(crate::app_config::resolve_trail_presentation(
                        cfg.cursor_trail_style_raw(),
                        cfg.cursor_pet_raw()
                    ))
                ),
            },
        },
        EditField {
            label: "Glow size",
            key: EDIT_CURSOR_TRAIL_RADIUS,
            kind: EditKind::Float,
            seed: cfg.cursor_trail_radius.map(|v| v.to_string()),
            placeholder: match cfg.cursor_trail_radius {
                Some(v) => float_reading(EDIT_CURSOR_TRAIL_RADIUS, v),
                None => format!("{} (default)", float_reading(EDIT_CURSOR_TRAIL_RADIUS, 0.6)),
            },
        },
        EditField {
            label: "Ring on jumps",
            key: EDIT_CURSOR_TRAIL_RING,
            kind: EditKind::Bool,
            seed: Some(cfg.cursor_trail_ring_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Trail color",
            key: EDIT_CURSOR_TRAIL_COLOR,
            kind: EditKind::Color,
            placeholder: match cursor_trail_color.as_deref() {
                Some(word) if word.eq_ignore_ascii_case(TRAIL_COLOR_CURSOR) => {
                    TRAIL_COLOR_CURSOR_LABEL.to_string()
                }
                Some(color) => color.to_string(),
                None if classic_in_cursor_color => TRAIL_COLOR_CURSOR_LABEL.to_string(),
                None => format!(
                    "{} (default)",
                    trail_color_default_words(
                        EDIT_CURSOR_TRAIL_COLOR,
                        cfg.cursor_trail_style_raw(),
                        cfg.trail_color(),
                        cfg.cursor_color.is_some(),
                    )
                ),
            },
            seed: cursor_trail_color,
        },
        EditField {
            label: "Trail accent color",
            key: EDIT_CURSOR_TRAIL_ACCENT,
            kind: EditKind::Color,
            placeholder: cursor_trail_accent.clone().unwrap_or_else(|| {
                format!(
                    "{} (default)",
                    trail_color_default_words(
                        EDIT_CURSOR_TRAIL_ACCENT,
                        cfg.cursor_trail_style_raw(),
                        cfg.trail_color(),
                        cfg.cursor_color.is_some(),
                    )
                )
            }),
            seed: cursor_trail_accent,
        },
        EditField {
            label: "Flying cat sprite",
            key: EDIT_CURSOR_NYAN_SPRITE,
            kind: EditKind::Text,
            placeholder: cursor_nyan_sprite
                .clone()
                .unwrap_or_else(|| "Built-in sprite (default)".to_string()),
            seed: cursor_nyan_sprite,
        },
        EditField {
            label: "Bloom",
            key: EDIT_CURSOR_TRAIL_BLOOM,
            kind: EditKind::Bool,
            seed: Some(cfg.cursor_trail_bloom_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Bloom strength",
            key: EDIT_CURSOR_TRAIL_BLOOM_STRENGTH,
            kind: EditKind::Float,
            seed: cfg.cursor_trail_bloom_strength.map(|v| v.to_string()),
            placeholder: match cfg.cursor_trail_bloom_strength {
                Some(v) => v.to_string(),
                None => "0.85 (default)".to_string(),
            },
        },
        EditField {
            label: "Bloom size",
            key: EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
            kind: EditKind::Float,
            seed: cfg.cursor_trail_bloom_radius.map(|v| v.to_string()),
            placeholder: match cfg.cursor_trail_bloom_radius {
                Some(v) => v.to_string(),
                None => format!(
                    "{} (default)",
                    Config::bloom_radius_style_default(crate::app_config::resolve_trail_style(
                        cfg.cursor_trail_style_raw(),
                    ))
                ),
            },
        },
        EditField {
            label: "Heat shimmer",
            key: EDIT_CURSOR_FIRE_SHIMMER,
            kind: EditKind::Bool,
            seed: Some(cfg.cursor_fire_shimmer_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "HDR / EDR glow",
            key: EDIT_HDR_GLOW,
            kind: EditKind::Bool,
            seed: Some(cfg.hdr_glow_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "SDR glow boost",
            key: EDIT_CURSOR_GLOW_SDR_BOOST,
            kind: EditKind::Float,
            seed: cfg.cursor_glow_sdr_boost.map(|v| v.to_string()),
            placeholder: match cfg.cursor_glow_sdr_boost {
                Some(v) => v.to_string(),
                None => "0.25 (default)".to_string(),
            },
        },
        EditField {
            label: "Serious Mode (no sounds or effects)",
            key: EDIT_SERIOUS_MODE,
            kind: EditKind::Bool,
            seed: Some(cfg.serious_mode_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Motion",
            key: EDIT_MOTION,
            kind: EditKind::Enum {
                options: MOTION_MODES,
            },
            placeholder: configured_str(cfg.motion.as_deref())
                .unwrap_or_else(|| "auto (default)".to_string()),
            seed: configured_str(cfg.motion.as_deref()),
        },
        EditField {
            label: "Reduce motion under heavy load",
            key: EDIT_LOAD_ADAPTIVE_MOTION,
            kind: EditKind::Bool,
            seed: Some(cfg.load_adaptive_motion_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Scrollback limit",
            key: EDIT_SCROLLBACK,
            kind: EditKind::Integer,
            seed: cfg.scrollback_lines.map(|n| n.to_string()),
            placeholder: match cfg.scrollback_lines {
                Some(0) => "unlimited".to_string(),
                Some(n) => n.to_string(),
                None => format!("{DEFAULT_SCROLLBACK_LINES} (default)"),
            },
        },
        EditField {
            label: "Copy on select",
            key: EDIT_COPY_ON_SELECT,
            kind: EditKind::Bool,
            seed: Some(cfg.copy_on_select_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Foreground",
            key: EDIT_FOREGROUND,
            kind: EditKind::Color,
            placeholder: foreground
                .clone()
                .unwrap_or_else(|| "Theme (default)".to_string()),
            seed: foreground,
        },
        EditField {
            label: "Background",
            key: EDIT_BACKGROUND,
            kind: EditKind::Color,
            placeholder: background
                .clone()
                .unwrap_or_else(|| "Theme (default)".to_string()),
            seed: background,
        },
        EditField {
            label: "Cursor color",
            key: EDIT_CURSOR_COLOR,
            kind: EditKind::Color,
            placeholder: cursor_color
                .clone()
                .unwrap_or_else(|| "Theme (default)".to_string()),
            seed: cursor_color,
        },
        EditField {
            label: "Selection color",
            key: EDIT_SELECTION_COLOR,
            kind: EditKind::Color,
            placeholder: selection_color
                .clone()
                .unwrap_or_else(|| "Theme (default)".to_string()),
            seed: selection_color,
        },
        EditField {
            label: "Selection foreground",
            key: EDIT_SELECTION_FOREGROUND,
            kind: EditKind::Color,
            placeholder: selection_foreground
                .clone()
                .unwrap_or_else(|| "Automatic".to_string()),
            seed: selection_foreground,
        },
        EditField {
            label: "Window appearance",
            key: EDIT_WINDOW_THEME,
            kind: EditKind::Enum {
                options: WINDOW_THEMES,
            },
            placeholder: configured_str(cfg.window_theme.as_deref())
                .unwrap_or_else(|| "auto (default)".to_string()),
            seed: configured_str(cfg.window_theme.as_deref()),
        },
        EditField {
            label: "256-colour palette",
            key: EDIT_PALETTE_GENERATE,
            kind: EditKind::Enum {
                options: PALETTE_GENERATE_MODES,
            },
            placeholder: configured_str(cfg.palette_generate.as_deref())
                .unwrap_or_else(|| "theme (default)".to_string()),
            seed: configured_str(cfg.palette_generate.as_deref()),
        },
        EditField {
            label: "Selected tab color",
            key: EDIT_ACTIVE_TAB_COLOR,
            kind: EditKind::Color,
            placeholder: active_tab_color
                .clone()
                .unwrap_or_else(|| "Automatic (default)".to_string()),
            seed: active_tab_color,
        },
        EditField {
            label: "Accent color",
            key: EDIT_ACCENT_COLOR,
            kind: EditKind::Color,
            placeholder: accent_color
                .clone()
                .unwrap_or_else(|| "Theme (default)".to_string()),
            seed: accent_color,
        },
        EditField {
            label: "Divider color",
            key: EDIT_BORDER_COLOR,
            kind: EditKind::Color,
            placeholder: border_color
                .clone()
                .unwrap_or_else(|| "Theme (default)".to_string()),
            seed: border_color,
        },
        EditField {
            label: "Search highlight color",
            key: EDIT_SEARCH_MATCH_COLOR,
            kind: EditKind::Color,
            placeholder: search_match_color
                .clone()
                .unwrap_or_else(|| "Theme (default)".to_string()),
            seed: search_match_color,
        },
        EditField {
            label: "Show floating version pill",
            key: EDIT_SHOW_BUILD_BADGE,
            kind: EditKind::Bool,
            seed: Some(cfg.show_build_badge_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Bidirectional text",
            key: EDIT_BIDI,
            kind: EditKind::Enum {
                options: BIDI_MODES,
            },
            placeholder: configured_str(cfg.bidi.as_deref())
                .unwrap_or_else(|| "implicit (default)".to_string()),
            seed: configured_str(cfg.bidi.as_deref()),
        },
        EditField {
            label: "Ambiguous-character width",
            key: EDIT_AMBIGUOUS_WIDTH,
            kind: EditKind::Enum {
                options: AMBIGUOUS_WIDTHS,
            },
            placeholder: configured_str(cfg.ambiguous_width.as_deref())
                .unwrap_or_else(|| "narrow (default)".to_string()),
            seed: configured_str(cfg.ambiguous_width.as_deref()),
        },
        EditField {
            label: "Confirm multiline paste",
            key: EDIT_CONFIRM_MULTILINE_PASTE,
            kind: EditKind::Bool,
            seed: Some(cfg.confirm_multiline_paste_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Alt/Option as Meta",
            key: EDIT_OPTION_AS_META,
            kind: EditKind::Bool,
            seed: Some(cfg.option_as_meta_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Initial columns",
            key: EDIT_COLUMNS,
            kind: EditKind::Integer,
            seed: cfg.columns.map(|n| n.to_string()),
            placeholder: cfg
                .columns
                .map_or_else(|| "80 (default)".to_string(), |n| n.to_string()),
        },
        EditField {
            label: "Initial lines",
            key: EDIT_LINES,
            kind: EditKind::Integer,
            seed: cfg.lines.map(|n| n.to_string()),
            placeholder: cfg
                .lines
                .map_or_else(|| "24 (default)".to_string(), |n| n.to_string()),
        },
        EditField {
            label: "Tab strip rows",
            key: EDIT_TAB_STRIP_ROWS,
            kind: EditKind::Integer,
            seed: cfg.tab_strip_rows.map(|n| n.to_string()),
            placeholder: cfg.tab_strip_rows.map_or_else(
                || format!("{} (default)", crate::app_config::DEFAULT_TAB_STRIP_ROWS),
                |n| n.to_string(),
            ),
        },
        EditField {
            label: "Classify session status",
            key: EDIT_TAB_STATUS,
            kind: EditKind::Bool,
            seed: Some(cfg.tab_status_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Quiet after",
            key: EDIT_TAB_STATUS_QUIET_AFTER_MS,
            kind: EditKind::Integer,
            seed: cfg.tab_status_quiet_after_ms.map(|ms| ms.to_string()),
            placeholder: cfg.tab_status_quiet_after_ms.map_or_else(
                || format!("{DEFAULT_TAB_STATUS_QUIET_AFTER_MS} (default)"),
                |ms| ms.to_string(),
            ),
        },
        EditField {
            label: "Status dwell",
            key: EDIT_TAB_STATUS_DWELL_MS,
            kind: EditKind::Integer,
            seed: cfg.tab_status_dwell_ms.map(|ms| ms.to_string()),
            placeholder: cfg.tab_status_dwell_ms.map_or_else(
                || format!("{DEFAULT_TAB_STATUS_DWELL_MS} (default)"),
                |ms| ms.to_string(),
            ),
        },
        EditField {
            label: "Show status on tabs",
            key: EDIT_TAB_STATUS_BADGE,
            kind: EditKind::Bool,
            seed: Some(cfg.tab_status_badge_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Mark connected sessions on tabs",
            key: EDIT_TAB_CONNECTION_BADGE,
            kind: EditKind::Bool,
            seed: Some(cfg.tab_connection_badge_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Searchable scrollback lines",
            key: EDIT_SEARCH_HISTORY_LINES,
            kind: EditKind::Integer,
            seed: cfg.search_history_lines.map(|n| n.to_string()),
            placeholder: cfg.search_history_lines.map_or_else(
                || format!("{} (default)", aterm_core::search::DEFAULT_MAX_CACHED_LINES),
                |n| n.to_string(),
            ),
        },
        EditField {
            label: "Predictive echo",
            key: EDIT_PREDICTIVE_ECHO,
            kind: EditKind::Enum {
                options: PREDICTIVE_ECHO_MODES,
            },
            placeholder: predictive_echo
                .clone()
                .unwrap_or_else(|| "adaptive (default)".to_string()),
            seed: predictive_echo,
        },
        EditField {
            label: "Predictive editing",
            key: EDIT_PREDICTIVE_ECHO_EDITING,
            kind: EditKind::Bool,
            seed: Some(cfg.predictive_echo_editing_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Focus priority boost (Windows)",
            key: EDIT_FOCUS_BOOST,
            kind: EditKind::Bool,
            seed: Some(cfg.focus_boost.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: atpkg::adoption::UPDATES_SWITCH,
            key: EDIT_PACKAGES_ENABLED,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_enabled().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: atpkg::adoption::INSTALLS_SWITCH,
            key: EDIT_PACKAGES_AUTO_INSTALL,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_auto_install().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Universal Control",
            key: EDIT_MACHINE_UNIVERSAL_CONTROL,
            kind: EditKind::Enum {
                options: UNIVERSAL_CONTROL_POLICIES,
            },
            seed: configured_str(
                cfg.machine
                    .as_ref()
                    .and_then(|m| m.universal_control.as_deref()),
            ),
            placeholder: "off (default)".to_string(),
        },
    ];
    let join_list = |v: Option<&Vec<String>>| v.filter(|l| !l.is_empty()).map(|l| l.join(", "));
    let palette = join_list(cfg.palette.as_ref());
    let shell_args = join_list(cfg.shell_args.as_ref());
    let font_features = join_list(cfg.font_features.as_ref());
    let shell = configured_str(cfg.shell.as_deref());
    let window_colorspace = configured_str(cfg.window_colorspace.as_deref());
    let background_material = configured_str(cfg.background_material.as_deref());
    fields.extend([
        EditField {
            label: "GPU rendering",
            key: EDIT_GPU,
            kind: EditKind::Bool,
            seed: Some(cfg.gpu.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "ANSI palette",
            key: EDIT_PALETTE,
            kind: EditKind::Text,
            placeholder: palette
                .clone()
                .unwrap_or_else(|| "theme palette — #RRGGBB by index, comma-separated".to_string()),
            seed: palette,
        },
        EditField {
            label: "Shell",
            key: EDIT_SHELL,
            kind: EditKind::Text,
            placeholder: shell
                .clone()
                .unwrap_or_else(|| SHELL_PLACEHOLDER.to_string()),
            seed: shell,
        },
        EditField {
            label: "Shell arguments",
            key: EDIT_SHELL_ARGS,
            kind: EditKind::Text,
            placeholder: shell_args
                .clone()
                .unwrap_or_else(|| "none — e.g. -l, -i (comma-separated)".to_string()),
            seed: shell_args,
        },
        EditField {
            label: "Window colorspace",
            key: EDIT_WINDOW_COLORSPACE,
            kind: EditKind::Enum {
                options: WINDOW_COLORSPACES,
            },
            placeholder: window_colorspace
                .clone()
                .unwrap_or_else(|| "srgb (default)".to_string()),
            seed: window_colorspace,
        },
        EditField {
            label: "Restore session at launch",
            key: EDIT_RESTORE_SESSION,
            kind: EditKind::Bool,
            seed: Some(cfg.restore_session_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Font features",
            key: EDIT_FONT_FEATURES,
            kind: EditKind::Text,
            placeholder: font_features
                .clone()
                .unwrap_or_else(|| "none — e.g. ss01, zero, -calt (comma-separated)".to_string()),
            seed: font_features,
        },
        EditField {
            label: "Dark-theme weight nudge (wght)",
            key: EDIT_FONT_WEIGHT_DARK_NUDGE,
            kind: EditKind::Float,
            seed: cfg.font_weight_dark_nudge.map(|v| format!("{v}")),
            placeholder: match cfg.font_weight_dark_nudge {
                Some(v) => format!("{v}"),
                None => "0 (off)".to_string(),
            },
        },
        EditField {
            label: "Background opacity",
            key: EDIT_BACKGROUND_OPACITY,
            kind: EditKind::Float,
            seed: cfg.background_opacity.map(|v| format!("{v}")),
            placeholder: match cfg.background_opacity {
                Some(v) => format!("{v}"),
                None => "1 (solid)".to_string(),
            },
        },
        EditField {
            label: "Wallpaper image",
            key: EDIT_WALLPAPER,
            kind: EditKind::Text,
            seed: cfg.wallpaper.clone(),
            placeholder: cfg
                .wallpaper
                .clone()
                .unwrap_or_else(|| "No image (default)".to_string()),
        },
        EditField {
            label: "Wallpaper dim",
            key: EDIT_WALLPAPER_DIM,
            kind: EditKind::Float,
            seed: cfg.wallpaper_dim.map(|v| format!("{v}")),
            placeholder: match cfg.wallpaper_dim {
                Some(v) => format!("{v}"),
                None => "0.3 (default)".to_string(),
            },
        },
        EditField {
            label: "Wallpaper text tint",
            key: EDIT_WALLPAPER_TEXT_TINT,
            kind: EditKind::Bool,
            seed: Some(cfg.wallpaper_text_tint_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Background material",
            key: EDIT_BACKGROUND_MATERIAL,
            kind: EditKind::Enum {
                options: BACKGROUND_MATERIALS,
            },
            placeholder: background_material
                .clone()
                .unwrap_or_else(|| "none (default)".to_string()),
            seed: background_material,
        },
        EditField {
            label: "Rewind (keep recent screen history)",
            key: EDIT_TEMPORAL_RECORDING,
            kind: EditKind::Bool,
            seed: Some(cfg.temporal_recording_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Sides and bottom",
            key: EDIT_WINDOW_PADDING,
            kind: EditKind::Float,
            seed: cfg.window_padding.map(|v| format!("{v}")),
            placeholder: match cfg.window_padding {
                Some(v) => format!("{v}"),
                None => "12 (default)".to_string(),
            },
        },
        EditField {
            label: "Top",
            key: EDIT_WINDOW_PADDING_TOP,
            kind: EditKind::Float,
            seed: cfg.window_padding_top.map(|v| format!("{v}")),
            placeholder: match cfg.window_padding_top {
                Some(v) => format!("{v}"),
                None => "2 (default)".to_string(),
            },
        },
    ]);
    for leaf in NESTED_LEAVES {
        let (seed, placeholder) = nested_seed_placeholder(cfg, leaf.key);
        fields.push(EditField {
            label: leaf.label,
            key: leaf.key,
            kind: leaf.kind,
            seed,
            placeholder,
        });
    }
    for &key in SECURITY_BOOL_KEYS {
        let on = match key {
            EDIT_ALLOW_OSC52_QUERY => cfg.allow_osc52_query.unwrap_or(false),
            EDIT_ALLOW_WINDOW_OPS => cfg.allow_window_ops.unwrap_or(false),
            EDIT_ALLOW_NOTIFICATIONS => cfg.allow_notifications.unwrap_or(false),
            EDIT_ALLOW_PALETTE_RECONFIGURE => cfg.allow_palette_reconfigure_or_default(),
            EDIT_ALLOW_KITTY_FILE_TRANSFER => cfg.allow_kitty_file_transfer.unwrap_or(false),
            EDIT_SECURE_KEYBOARD_ENTRY => cfg.secure_keyboard_entry.unwrap_or(false),
            _ => false,
        };
        fields.push(EditField {
            label: security_label(key),
            key,
            kind: EditKind::Bool,
            seed: Some(on.to_string()),
            placeholder: String::new(),
        });
    }
    fields.sort_by_cached_key(|f| section_of(f.key).order_index());
    fields
}

/// The result of a Preferences save, for the window's status feedback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveOutcome {
    Saved,
    Unchanged,
    Conflict {
        expected: crate::native_document_io::ContentFingerprint,
        observed: crate::native_document_io::ObservedFileVersion,
        message: String,
    },
    PublishedUnverified {
        stage: crate::native_document_io::AtomicSaveStage,
        observed: Option<crate::native_document_io::ObservedFileVersion>,
        message: String,
    },
    Error(String),
}

/// Settings-worker persistence outcome paired with the generation proof required to clear dirty state.
#[derive(Clone, Debug)]
pub(crate) struct ConfigSnapshotSaveResult {
    pub(crate) outcome: SaveOutcome,
    pub(crate) observed: Option<crate::native_document_host::AtomicFileBaseline>,
}

/// Atomically persist a complete snapshot already produced by the versioned native-config service.
#[cfg(test)]
pub(crate) fn save_prefs_snapshot(
    plan: &crate::native_config_service::ConfigPersistencePlan,
) -> SaveOutcome {
    save_prefs_snapshot_observed(plan).outcome
}

/// Worker-facing form of `save_prefs_snapshot` that retains the committed proof.
pub(crate) fn save_prefs_snapshot_observed(
    plan: &crate::native_config_service::ConfigPersistencePlan,
) -> ConfigSnapshotSaveResult {
    if plan.expected_text == plan.snapshot.text {
        return ConfigSnapshotSaveResult {
            outcome: SaveOutcome::Unchanged,
            observed: plan.baseline.clone(),
        };
    }
    let (path, baseline) = if let Some(baseline) = plan.baseline.clone() {
        (baseline.target.logical_path().to_path_buf(), baseline)
    } else {
        let Some(path) = plan
            .logical_path
            .clone()
            .or_else(crate::app_config::config_path)
        else {
            return ConfigSnapshotSaveResult {
                outcome: SaveOutcome::Error(crate::save_words::words(
                    crate::save_words::Speaker::Aterm,
                    std::path::Path::new("aterm.toml"),
                    &crate::save_words::Failure::NoPath,
                )),
                observed: None,
            };
        };
        let contents = match crate::native_document_host::read_config_atomic_file(
            &path,
            crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
            true,
        ) {
            Ok(contents) => contents,
            Err(error) => {
                crate::logging::stderr_line!(
                    "aterm-gui: prefs save: {}; config unchanged",
                    crate::native_config_service::config_read_error(&path, error.clone())
                );
                return ConfigSnapshotSaveResult {
                    outcome: SaveOutcome::Error(crate::save_words::words(
                        crate::save_words::Speaker::Aterm,
                        &path,
                        &crate::save_words::Failure::Read(&error),
                    )),
                    observed: None,
                };
            }
        };
        if contents.bytes.as_slice() != plan.expected_text.as_bytes() {
            return ConfigSnapshotSaveResult {
                outcome: SaveOutcome::Conflict {
                    expected: crate::native_document_io::ContentFingerprint::of(
                        plan.expected_text.as_bytes(),
                    ),
                    observed: contents.baseline.observed,
                    message: "config changed on disk before the Settings transaction began"
                        .to_string(),
                },
                observed: None,
            };
        }
        (path, contents.baseline)
    };
    match crate::native_document_host::commit_atomic_bytes(&baseline, plan.snapshot.text.as_bytes())
    {
        crate::native_document_host::AtomicCommitResult::Committed(proof) => {
            ConfigSnapshotSaveResult {
                outcome: SaveOutcome::Saved,
                observed: Some(crate::native_document_host::AtomicFileBaseline {
                    target: baseline.target,
                    observed: proof.observed,
                }),
            }
        }
        crate::native_document_host::AtomicCommitResult::Conflict { observed, message } => {
            let message = format!(
                "{} changed while saving ({message}); review the latest file and retry",
                path.display()
            );
            crate::logging::stderr_line!("aterm-gui: prefs save: {message}; config unchanged");
            ConfigSnapshotSaveResult {
                outcome: SaveOutcome::Conflict {
                    expected: baseline.observed.content,
                    observed,
                    message,
                },
                observed: None,
            }
        }
        crate::native_document_host::AtomicCommitResult::Failed {
            stage,
            message,
            cause,
        } => {
            crate::logging::stderr_line!(
                "aterm-gui: prefs save: {} failed at {stage:?} ({message}); config unchanged",
                path.display()
            );
            ConfigSnapshotSaveResult {
                outcome: SaveOutcome::Error(crate::save_words::words(
                    crate::save_words::Speaker::Aterm,
                    &path,
                    &crate::save_words::Failure::Write { stage, cause },
                )),
                observed: None,
            }
        }
        crate::native_document_host::AtomicCommitResult::PublishedUnverified {
            stage,
            observed,
            message,
            cause,
        } => {
            crate::logging::stderr_line!(
                "aterm-gui: prefs save: {} may already contain the requested bytes ({stage:?}: \
                 {message})",
                path.display()
            );
            ConfigSnapshotSaveResult {
                outcome: SaveOutcome::PublishedUnverified {
                    stage,
                    observed,
                    message: crate::save_words::words(
                        crate::save_words::Speaker::Aterm,
                        &path,
                        &crate::save_words::Failure::Unconfirmed { stage, cause },
                    ),
                },
                observed: None,
            }
        }
    }
}

#[cfg(all(test, unix))]
#[path = "prefs_save_words_tests.rs"]
mod save_words_tests;

#[cfg(test)]
#[path = "prefs_trail_style_tests.rs"]
mod trail_style_tests;

#[cfg(test)]
#[path = "prefs_cursor_names_tests.rs"]
mod cursor_names_tests;

#[cfg(test)]
#[path = "prefs_edit_tests.rs"]
mod edit_tests;

#[cfg(test)]
#[path = "prefs_registry_conformance_tests.rs"]
mod registry_conformance_tests;
