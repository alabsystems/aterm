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
/// The RETIRED cursor-effects master (`Config::cursor_trail`).
pub(crate) const RETIRED_CURSOR_TRAIL: &str = "cursor_trail";
/// The two settings [`RETIRED_CURSOR_TRAIL`] reads as.
pub(crate) const RETIRED_CURSOR_TRAIL_SUCCESSORS: [&str; 2] =
    [EDIT_CURSOR_TRAIL_STYLE, EDIT_CURSOR_MOMENTUM_GLOW];
pub(crate) const EDIT_CURSOR_TRAIL_STYLE: &str = "cursor_trail_style";
/// THE STYLE OFF REPLACED.
pub(crate) const EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF: &str = "cursor_trail_style_before_off";
pub(crate) const EDIT_TRAIL_SOUNDS: &str = "trail_sounds";
/// Tone-melody toggle (`Config::tone_melody`, default ON).
pub(crate) const EDIT_TONE_MELODY: &str = "tone_melody";
/// Ambient-bed toggle (`Config::trail_sound_bed`, default ON).
pub(crate) const EDIT_TRAIL_SOUND_BED: &str = "trail_sound_bed";
/// Typing-sound picker (`Config::trail_sound_style`, default `auto`).
pub(crate) const EDIT_TRAIL_SOUND_STYLE: &str = "trail_sound_style";
/// The held-key sing-along riff's own switch (`Config::trail_sound_riff`, default ON).
pub(crate) const EDIT_TRAIL_SOUND_RIFF: &str = "trail_sound_riff";
/// Audible terminal bell (`Config::bell_sound`, default ON).
pub(crate) const EDIT_BELL_SOUND: &str = "bell_sound";
/// The curse bonk's two gates.
pub(crate) const EDIT_SPARKLE_BONK: &str = "sparkle_words.profanity.bonk";
pub(crate) const EDIT_SPARKLE_BONK_DETONATION: &str = "sparkle_words.profanity.bonk_detonation";
/// PRISM WAKE's pip (`[output_streak] sound`, default ON; `docs/DESIGN-output-streak-2026-08-30.md` §7).
pub(crate) const EDIT_OUTPUT_STREAK_SOUND: &str = "output_streak.sound";
/// PRISM WAKE's switch (`[output_streak] enabled`).
pub(crate) const EDIT_OUTPUT_STREAK_ENABLED: &str = "output_streak.enabled";

/// PRESENCE: the `[presence]` leaf the View menu's needs switch writes.
pub(crate) const EDIT_PRESENCE_RIM: &str = "presence.rim";

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

/// WHAT THE TRAIL COLOUR ROWS DO for the cursor style aterm.toml spells `style_raw`.
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
            format!(
                "Blank: {name}\u{2019}s rod keeps its own color on a bar cursor; any color, \
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
        || (key == EDIT_CURSOR_TRAIL_COLOR && raw.eq_ignore_ascii_case(TRAIL_COLOR_CURSOR))
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
/// Preference key controlling whether `aterm` posts its own notices as desktop notifications.
pub(crate) const EDIT_DESKTOP_ALERTS: &str = "desktop_alerts";
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
/// The ONE name of [`EDIT_UPDATE_ENABLED`]'s switch.
pub(crate) const UPDATE_CHECK_LABEL: &str = aterm_update_core::settings::CHECK_LABEL;
/// The ONE name of `[update] auto_apply`'s switch, as [`UPDATE_CHECK_LABEL`] is the check switch's.
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
/// `[harness.codex] enabled` — [`EDIT_HARNESS_CLAUDE_ENABLED`] for Codex.
pub(crate) const EDIT_HARNESS_CODEX_ENABLED: &str = "harness.codex.enabled";

/// The label of Codex's own switch ([`EDIT_HARNESS_CODEX_ENABLED`]).
pub(crate) const CODEX_SWITCH_LABEL: &str = "Watch codex-alab sessions";

/// The label of the switch of the agent named `product` (`Program::product`).
#[must_use]
pub(crate) fn agent_switch_label(product: &str) -> String {
    use aterm_phase::Program;
    match Program::ALL.into_iter().find(|p| p.product() == product) {
        Some(Program::Codex) => CODEX_SWITCH_LABEL.to_string(),
        Some(
            Program::Claude | Program::Agy | Program::Muse | Program::Gemini | Program::Generic,
        )
        | None => format!("Supervise {product} sessions"),
    }
}
/// `[harness.agy] enabled` — [`EDIT_HARNESS_CLAUDE_ENABLED`] for agy.
pub(crate) const EDIT_HARNESS_AGY_ENABLED: &str = "harness.agy.enabled";
/// `[harness.muse] enabled` — [`EDIT_HARNESS_CLAUDE_ENABLED`] for Muse Code.
pub(crate) const EDIT_HARNESS_MUSE_ENABLED: &str = "harness.muse.enabled";
/// `[harness.gemini] enabled` — [`EDIT_HARNESS_CLAUDE_ENABLED`] for Gemini CLI.
pub(crate) const EDIT_HARNESS_GEMINI_ENABLED: &str = "harness.gemini.enabled";

/// The agent a `harness.<agent>.enabled` row switches, read from its key.
pub(crate) fn agent_switch(key: &str) -> Option<Program> {
    let word = key.strip_prefix("harness.")?.strip_suffix(".enabled")?;
    aterm_phase::program_of(word).filter(|p| p.harness_table() && p.name() == word)
}

/// The names a row is searched by as its OWN, beside its label and key (`native_config_language::query_match_rank`).
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
/// GPU-present colour-space tag (`window_colorspace`: srgb | display-p3).
pub(crate) const EDIT_WINDOW_COLORSPACE: &str = "window_colorspace";
/// Session restore at launch (`restore_session`, default ON).
pub(crate) const EDIT_RESTORE_SESSION: &str = "restore_session";
/// Opt-in shell custody, read at launch.
pub(crate) const EDIT_KEEPER_ENABLED: &str = "keeper.enabled";
/// Session activity on disk (on by default, D3), read at launch.
pub(crate) const EDIT_SESSION_LOG_JOURNAL: &str = "session_log.journal";
/// Live activity in terminal tab tooltips and menus (on by default, D6).
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
/// Per-session temporal recorder opt-in (`temporal_recording`, default OFF).
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

/// One SCALAR leaf of a nested config table (`[update]` / `[sparkle_words]` (+ sub-tables) / `[matrix_rain]`).
pub(crate) struct NestedLeaf {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) kind: EditKind,
}

/// Every REGISTERED nested scalar leaf.
pub(crate) const NESTED_LEAVES: &[NestedLeaf] = &[
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
        label: "Show a note when cargo or rustc is rerouted",
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
        key: EDIT_HARNESS_GEMINI_ENABLED,
        label: "Supervise Gemini CLI sessions",
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
            options: &aterm_agent::supervise::QuestionPriority::WORDS,
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
    NestedLeaf {
        key: EDIT_PRESENCE_RIM,
        label: "Show needs in windows",
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
pub(crate) const DEFAULT_CURSOR_TRAIL_STYLE: &str = "rainbow kitty pet";

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

/// Documented aliases accepted by the runtime.
pub(crate) const CURSOR_TRAIL_STYLE_ALIASES: &[(&str, &str)] = spellings::TRAIL_STYLE_ALIASES;

/// Resolve a `cursor_trail_style` spelling (a canonical row or a documented alias; trimmed, case-insensitive) to its canonical name.
pub(crate) fn cursor_trail_style_canonical(token: &str) -> Option<&'static str> {
    spellings::canonical(token)
}

/// How a Preferences key should be TYPED in the written TOML.
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
        | EDIT_DESKTOP_ALERTS
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
    match raw
        .filter(|_| !theme_is_unset(raw))
        .map(ThemeSelector::parse)
    {
        Some((ThemeSelector::Split { .. }, _)) => ThemeMode::FollowSystem,
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
    if let Some(kept) = kept
        && raw.is_some_and(|raw| raw.trim().eq_ignore_ascii_case(kept.always.trim()))
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

/// Whether a configured `theme` value draws what an unset one does (Default, on every appearance).
pub(crate) fn theme_is_unset(raw: Option<&str>) -> bool {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return true;
    };
    let (selector, _) = aterm_types::scheme::ThemeSelector::parse(raw);
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
mod theme_pair_tests {
    use super::{
        ThemeFollowKept, ThemeMode, theme_always_value, theme_choice_value,
        theme_follow_system_value, theme_is_unset, theme_mode, theme_sideless_choice_value,
        theme_slots, theme_with_side,
    };
    use aterm_types::Appearance::{Dark, Light};

    fn builtins() -> std::sync::Arc<crate::app_config::ThemeCatalog> {
        crate::app_config::ThemeCatalog::empty()
    }

    #[test]
    fn the_mode_is_read_off_the_value() {
        assert_eq!(theme_mode(None), ThemeMode::Always);
        assert_eq!(theme_mode(Some("")), ThemeMode::Always);
        assert_eq!(theme_mode(Some("  ")), ThemeMode::Always);
        assert_eq!(theme_mode(Some("dark:,light:")), ThemeMode::Always);
        assert_eq!(theme_mode(Some("Nord")), ThemeMode::Always);
        assert_eq!(
            theme_mode(Some("dark:Nord,light:Nord Light")),
            ThemeMode::FollowSystem
        );
        assert_eq!(
            theme_mode(Some("light:Nord Light")),
            ThemeMode::FollowSystem
        );
    }

    #[test]
    fn turning_follow_system_on_seeds_the_other_side_from_the_pair() {
        let themes = builtins();
        assert_eq!(
            theme_follow_system_value(Some("Gruvbox Dark"), None, &themes),
            "dark:Gruvbox Dark,light:Gruvbox Light"
        );
        assert_eq!(
            theme_follow_system_value(Some("Gruvbox Light"), None, &themes),
            "dark:Gruvbox Dark,light:Gruvbox Light"
        );
        for raw in [
            Some("Default"),
            Some("Default Light"),
            None,
            Some(""),
            Some("dark:,light:"),
        ] {
            assert_eq!(
                theme_follow_system_value(raw, None, &themes),
                "dark:Default,light:Default Light",
                "{raw:?}"
            );
        }
        let unpaired = aterm_types::scheme::builtin_theme_list()
            .into_iter()
            .filter(|theme| theme.pair.is_none())
            .collect::<Vec<_>>();
        for appearance in [Dark, Light] {
            let theme = unpaired
                .iter()
                .find(|theme| theme.appearance == appearance)
                .expect("a built-in without a pair on each side");
            let expected = match appearance {
                Dark => format!("dark:{},light:Default Light", theme.name),
                Light => format!("dark:Default,light:{}", theme.name),
            };
            assert_eq!(
                theme_follow_system_value(Some(theme.name), None, &themes),
                expected
            );
        }
        assert_eq!(
            theme_follow_system_value(Some("Nord"), None, &themes),
            "dark:Nord,light:Default Light"
        );
        assert_eq!(
            theme_follow_system_value(Some("GitHub Light"), None, &themes),
            "dark:Default,light:GitHub Light"
        );
        assert_eq!(
            theme_follow_system_value(Some("Office Missing"), None, &themes),
            "dark:Office Missing,light:Default Light"
        );
        assert_eq!(
            theme_follow_system_value(Some("dark:Dracula,light:Nord"), None, &themes),
            "dark:Dracula,light:Nord"
        );
    }

    #[test]
    fn a_choice_keeps_a_pair_and_writes_a_name_otherwise() {
        let themes = builtins();
        let pair = Some("dark:Gruvbox Dark,light:Gruvbox Light");
        assert_eq!(
            theme_choice_value(pair, Light, "Nord", &themes),
            ("dark:Gruvbox Dark,light:Nord".to_string(), Some(Light))
        );
        assert_eq!(
            theme_choice_value(pair, Dark, "Default", &themes),
            ("dark:Default,light:Gruvbox Light".to_string(), Some(Dark))
        );
        let unset = [None, Some(""), Some("  "), Some("dark:,light:")];
        for raw in unset {
            assert!(theme_is_unset(raw), "{raw:?}");
        }
        for raw in [Some("Dracula"), Some("dark:Dracula")] {
            assert!(!theme_is_unset(raw), "{raw:?}");
        }
        for raw in unset.into_iter().chain([Some("Dracula")]) {
            for theme in aterm_types::scheme::builtin_theme_list() {
                for side in [Dark, Light] {
                    assert_eq!(
                        theme_choice_value(raw, side, theme.name, &themes),
                        (theme.name.to_string(), None),
                        "{raw:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_one_theme_pair_takes_the_rules_every_pair_takes() {
        let themes = builtins();
        let same = Some("dark:Aterm Day,light:Aterm Day");
        assert_eq!(theme_mode(same), ThemeMode::FollowSystem);
        assert_eq!(
            theme_choice_value(same, Dark, "Linen", &themes),
            ("dark:Linen,light:Aterm Day".to_string(), Some(Dark))
        );
        assert_eq!(
            theme_sideless_choice_value(same, Dark, "Linen", &themes),
            ("Linen".to_string(), None)
        );
        assert_eq!(
            theme_sideless_choice_value(same, Dark, "Hearth", &themes),
            ("dark:Hearth,light:Aterm Day".to_string(), Some(Dark))
        );
        assert_eq!(
            theme_sideless_choice_value(same, Light, "Harbor Day", &themes).0,
            "dark:Aterm Day,light:Harbor Day"
        );
        let two = Some("dark:Nord,light:Aterm Day");
        assert_eq!(
            theme_choice_value(two, Dark, "Linen", &themes),
            ("dark:Linen,light:Aterm Day".to_string(), Some(Dark))
        );
        assert_eq!(
            theme_sideless_choice_value(two, Dark, "Linen", &themes),
            ("Linen".to_string(), None)
        );
    }

    #[test]
    fn a_side_less_choice_never_files_a_theme_under_the_other_appearance() {
        let themes = builtins();
        let raws = [
            None,
            Some("Nord"),
            Some("dark:Default,light:Default Light"),
            Some("dark:Nord,light:Default Light"),
            Some("dark:Nord,light:Solarized Light"),
            Some("dark:Aterm Day,light:Aterm Day"),
        ];
        for raw in raws {
            for theme in aterm_types::scheme::builtin_theme_list() {
                for shown in [Dark, Light] {
                    let (value, side) =
                        theme_sideless_choice_value(raw, shown, theme.name, &themes);
                    let (dark, light) = theme_slots(Some(&value), &themes);
                    let drawn = match (theme_mode(Some(&value)), shown) {
                        (ThemeMode::Always, _) => value.clone(),
                        (ThemeMode::FollowSystem, Dark) => dark.clone(),
                        (ThemeMode::FollowSystem, Light) => light.clone(),
                    };
                    assert_eq!(drawn, theme.name, "{raw:?} {shown:?} -> {value}");
                    if theme_mode(Some(&value)) == ThemeMode::FollowSystem {
                        assert_eq!(theme.appearance, shown, "{raw:?} -> {value}");
                        assert_eq!(side, Some(shown));
                        let (was_dark, was_light) = theme_slots(raw, &themes);
                        match shown {
                            Dark => assert_eq!(light, was_light, "{raw:?} -> {value}"),
                            Light => assert_eq!(dark, was_dark, "{raw:?} -> {value}"),
                        }
                    } else {
                        assert_eq!(side, None);
                        assert_eq!(value, theme.name);
                    }
                }
            }
        }
        assert_eq!(
            theme_sideless_choice_value(
                Some("dark:Nord,light:Default Light"),
                Dark,
                "Aterm Day",
                &themes
            ),
            ("Aterm Day".to_string(), None)
        );
        assert_eq!(
            theme_sideless_choice_value(
                Some("dark:Nord,light:Solarized Light"),
                Light,
                "Hearth",
                &themes
            ),
            ("Hearth".to_string(), None)
        );
        assert_eq!(
            theme_choice_value(
                Some("dark:Nord,light:Default Light"),
                Dark,
                "Aterm Day",
                &themes
            ),
            ("dark:Aterm Day,light:Default Light".to_string(), Some(Dark))
        );
    }

    #[test]
    fn turning_follow_system_off_then_on_restores_the_pair_it_replaced() {
        let themes = builtins();
        let pair = "dark:Nord,light:Solarized Light";
        for (side, name) in [(Dark, "Nord"), (Light, "Solarized Light")] {
            let always = theme_always_value(Some(pair), side, &themes);
            assert_eq!(always, name);
            let kept = ThemeFollowKept {
                always: always.clone(),
                pair: pair.to_string(),
            };
            assert_eq!(
                theme_follow_system_value(Some(&always), Some(&kept), &themes),
                pair,
                "{side:?}"
            );
            assert_ne!(
                theme_follow_system_value(Some(&always), None, &themes),
                pair
            );
        }
        let kept = ThemeFollowKept {
            always: "Nord".to_string(),
            pair: pair.to_string(),
        };
        assert_eq!(
            theme_follow_system_value(Some("Dracula"), Some(&kept), &themes),
            theme_follow_system_value(Some("Dracula"), None, &themes)
        );
    }

    #[test]
    fn a_slot_edit_keeps_the_other_side() {
        let themes = builtins();
        let raw = Some("dark:Gruvbox Dark,light:Gruvbox Light");
        assert_eq!(
            theme_with_side(raw, Light, "Solarized Light", &themes),
            "dark:Gruvbox Dark,light:Solarized Light"
        );
        assert_eq!(
            theme_with_side(raw, Dark, "Nord", &themes),
            "dark:Nord,light:Gruvbox Light"
        );
        assert_eq!(
            theme_with_side(
                Some("dark:Nord,light:Default Light"),
                Dark,
                "Default",
                &themes
            ),
            "dark:Default,light:Default Light"
        );
        assert_eq!(
            theme_with_side(None, Light, "GitHub Light", &themes),
            "dark:Default,light:GitHub Light"
        );
        let implicit = Some("dark:Gruvbox Dark");
        assert_eq!(
            theme_slots(implicit, &themes),
            ("Gruvbox Dark".to_string(), "Gruvbox Light".to_string())
        );
        assert_eq!(
            theme_with_side(implicit, Dark, "Nord", &themes),
            "dark:Nord,light:Gruvbox Light"
        );
        for theme in aterm_types::scheme::builtin_theme_list() {
            for side in [Dark, Light] {
                let written = theme_with_side(raw, side, theme.name, &themes);
                let (dark, light) = theme_slots(Some(&written), &themes);
                let (edited, kept, kept_was) = match side {
                    Dark => (dark, light, "Gruvbox Light"),
                    Light => (light, dark, "Gruvbox Dark"),
                };
                assert_eq!(edited, theme.name, "{written:?}");
                assert_eq!(kept, kept_was, "{written:?}");
            }
        }
    }

    #[test]
    fn turning_follow_system_off_writes_the_visible_side() {
        let themes = builtins();
        let raw = Some("dark:Dracula,light:GitHub Light");
        assert_eq!(theme_always_value(raw, Dark, &themes), "Dracula");
        assert_eq!(theme_always_value(raw, Light, &themes), "GitHub Light");
        let on = theme_follow_system_value(Some("Gruvbox Dark"), None, &themes);
        assert_eq!(theme_always_value(Some(&on), Dark, &themes), "Gruvbox Dark");
    }
}

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
    keep_cursor_style_before_off(&cursor_before, &mut doc, edits);
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

fn keep_cursor_style_before_off(
    before: &CursorStyleFacts,
    doc: &mut aterm_toml::edit::DocumentMut,
    edits: &[(&str, Option<String>)],
) {
    if edits
        .iter()
        .any(|(key, _)| *key == EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF)
    {
        return;
    }
    let after = CursorStyleFacts::of(doc);
    if after == *before {
        return;
    }
    if style_draws(after.drawn()) {
        doc.remove(EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF);
        return;
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
        // Not true or false: it never read, so nothing to carry over.
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

/// THE VALUE THAT MEANS UNSET.
pub(crate) fn unset_literal(field: &EditField) -> Option<String> {
    if field.key == "ai" || field.key.starts_with("ai.") {
        return None;
    }
    let placeholder = field.placeholder.trim();
    let marked = placeholder.strip_suffix(" (default)").map(str::trim);
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

/// Whether `value` is `field`'s [`unset_literal`].
pub(crate) fn is_unset_literal(field: &EditField, value: &str) -> bool {
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

/// THE ONE "A VALUE EQUAL TO ITS DEFAULT REMOVES THE KEY" RULE, for every writer.
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

/// THE SETTINGS A RETIRED SPELLING STILL AUTHORS in `text`.
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
    let Ok(with) = Config::parse(text) else {
        return authored;
    };
    let fields_with = editable_fields(&with);
    for (retired, successor) in retired_successor_pairs() {
        if !holds(&doc, retired) {
            continue;
        }
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

/// Would the writer take `raw` for `key`? Its own check, for a caller that refuses a value before it reaches the file.
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

/// THE ONE SPELLING OF A NUMBER A FRACTIONAL ([`EditKind::Float`]) SETTING WRITES.
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
    } else if LIST_KEYS.contains(&key) && trimmed.starts_with('[') {
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
                // A case-variant id is AUTHORED as its canonical spelling.
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
            if trimmed.eq_ignore_ascii_case(TRAIL_COLOR_CURSOR) {
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
        EDIT_KEEPER_ENABLED => boolean(Some(cfg.keeper_enabled()), false),
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
                (word != aterm_agent::supervise::QuestionPriority::Recommended)
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
        EDIT_PRESENCE_RIM => boolean(Some(cfg.presence_rim_enabled()), true),
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
    if key.starts_with("presence.") {
        return Section::Window;
    }
    if matches!(key, EDIT_CURSOR_TRAIL_STYLE | EDIT_CURSOR_NYAN_SPRITE)
        || CURSOR_STYLE_TUNING_KEYS.contains(&key)
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
        EDIT_DESKTOP_ALERTS => Section::Security,
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
        EDIT_SHELL | EDIT_SHELL_ARGS => Section::Terminal,
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
    if key.starts_with("presence.") {
        return ("Presence", 2);
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
        EDIT_DESKTOP_ALERTS => ("Notifications", 2),
        EDIT_PACKAGES_ENABLED | EDIT_PACKAGES_AUTO_INSTALL | EDIT_REROUTE_ANNOUNCE => {
            ("ALab Tools", 0)
        }
        EDIT_SCROLLBACK | EDIT_SEARCH_HISTORY_LINES => ("Scrollback", 0),
        EDIT_BIDI | EDIT_AMBIGUOUS_WIDTH => ("Text direction & width", 1),
        EDIT_SHELL | EDIT_SHELL_ARGS => ("Shell", 2),
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
            "Off, a program's notifications are dropped; aterm's own alerts still show."
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
        EDIT_KEEPER_ENABLED => {
            "Requires the PTY keeper service (`aterm keeper start`); this switch alone does not start it. Reopening a saved layout alone starts new shells."
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
        EDIT_WINDOW_THEME => {
            if cfg!(target_os = "macos") {
                "Auto matches the macOS appearance, light or dark."
            } else {
                "Auto matches the terminal theme, light or dark."
            }
        }
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
        EDIT_FONT_WEIGHT => "Only variable fonts use Variable font weight; other fonts ignore it.",
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
        EDIT_DESKTOP_ALERTS => {
            "When off, aterm's alerts stay in the window, the menu bar and Messages."
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
     (aterm ai fleet keygen prints one); a URL's document needs it.";
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

/// WHEN A SAVED CHANGE TAKES EFFECT, where that is not at once.
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
            Self::DailyLaunchApply => "Applies once a day, at a new window or session",
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
        EDIT_ALLOW_KITTY_FILE_TRANSFER | EDIT_TEMPORAL_RECORDING | EDIT_SHELL | EDIT_SHELL_ARGS => {
            T::NewSessions
        }
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
        _ => return None,
    })
}

/// Human label for a security opt-in row (keyed by its `SECURITY_BOOL_KEYS` entry).
pub(crate) fn security_label(key: &str) -> &'static str {
    match key {
        EDIT_ALLOW_OSC52_QUERY => "Allow programs to read the clipboard",
        EDIT_SECURE_KEYBOARD_ENTRY => "Secure Keyboard Entry (block keystroke snooping)",
        EDIT_ALLOW_WINDOW_OPS => {
            if cfg!(target_os = "linux") {
                // The manipulation half is wired on Linux.
                "Allow programs to resize the window and read its title and size"
            } else {
                // No host callback off Linux: reports only, which include the
                // engine's pixel answers (CSI 14 t, CSI 16 t), hence "size".
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

/// THE UNIT A NUMERIC SETTING IS COUNTED IN, as the suffix its number takes (" px", " ms", "%", "×", " cells").
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
        "matrix_rain.idle_secs" | "output_streak.idle_secs" => " s",
        "sparkle_words.profanity.supernova_chance" | "sparkle_words.custom.burst.chance" => "%",
        EDIT_LINE_HEIGHT | EDIT_CURSOR_TRAIL_BLOOM_RADIUS => "\u{00d7}",
        _ => return None,
    })
}

/// HOW A NUMBER READS.
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

#[cfg(windows)]
const SHELL_PLACEHOLDER: &str =
    "Platform (default) — pwsh, powershell, cmd, bash, wsl, nu, or a path";
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
            "cat",
            "rainbow",
            "nyan",
            "pet",
            "companion",
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
        EDIT_TEMPORAL_RECORDING => &["temporal", "recording", "replay", "history", "time"],
        EDIT_DESKTOP_ALERTS => &[
            "notification",
            "notifications",
            "alert",
            "alerts",
            "banner",
            "desktop",
            "messages",
            "script",
            "editor",
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
        EDIT_PRESENCE_RIM => &[
            "presence",
            "needs",
            "border",
            "rim",
            "pill",
            "stuck",
            "decide",
            "attention",
            "window",
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
            // A checkbox shows the state in force.
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
            placeholder: match cfg.font_px {
                Some(px) => format!("{px} px"),
                None => format!("{} (automatic)", crate::FONT_PX),
            },
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
                        cfg.cursor_trail_style_raw()
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
            label: "Rainbow kitty sprite",
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
            label: "Keep ALab tools up to date",
            key: EDIT_PACKAGES_ENABLED,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_enabled().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Install ALab tools",
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
            label: "Desktop notifications from aterm",
            key: EDIT_DESKTOP_ALERTS,
            kind: EditKind::Bool,
            seed: Some(cfg.desktop_alerts_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Record sessions for replay",
            key: EDIT_TEMPORAL_RECORDING,
            kind: EditKind::Bool,
            seed: Some(cfg.temporal_recording.unwrap_or(false).to_string()),
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

/// Worker-facing form of [`save_prefs_snapshot`] that retains the committed proof.
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
mod save_words_tests {
    use crate::native_document_io::{AtomicSaveStage, SaveCause, SaveTouch};
    use crate::save_words::{Failure, Speaker, words};
    use std::io::ErrorKind;
    use std::path::Path;

    fn save_with(
        path: &Path,
        before: impl FnOnce(),
        after: impl FnOnce(),
    ) -> (crate::prefs::SaveOutcome, String) {
        use crate::native_config_service::{
            ConfigKeyEdit, ConfigPatchRequest, ConfigPatchResult, ExpectedValue,
            VersionedConfigService,
        };
        std::fs::write(path, "theme = \"Default\"\n").unwrap();
        let mut service = VersionedConfigService::load_path(path).unwrap();
        let ConfigPatchResult::Applied { snapshot, .. } = service.patch(ConfigPatchRequest {
            base_revision: service.snapshot().revision,
            edits: vec![ConfigKeyEdit {
                key: "theme".into(),
                expected: ExpectedValue::Exact(Some("Default".to_string())),
                value: Some("Nord".to_string()),
            }],
        }) else {
            panic!("the patch reduces before it is written");
        };
        let plan = service.persistence_plan(snapshot);
        before();
        let outcome = crate::prefs::save_prefs_snapshot(&plan);
        after();
        (outcome, std::fs::read_to_string(path).unwrap())
    }

    #[test]
    fn a_refused_save_says_where_and_why() {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let dir =
            std::env::temp_dir().join(format!("aterm-save-words-{}-refused", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let lock = dir.join(".aterm.toml.aterm-write.lock");
        assert_eq!(
            crate::native_document_host::save_lock_of(&path).as_deref(),
            Some(lock.as_path())
        );
        let expect = |stage, touch| {
            words(
                Speaker::Aterm,
                &path,
                &Failure::Write {
                    stage,
                    cause: Some(SaveCause {
                        kind: ErrorKind::PermissionDenied,
                        touch,
                    }),
                },
            )
        };
        let (outcome, after) = save_with(&path, || mode(&dir, 0o500), || mode(&dir, 0o700));
        let folder = expect(AtomicSaveStage::Preflight, SaveTouch::Folder);
        assert!(folder.contains("isn\u{2019}t writable"), "{folder}");
        assert_eq!(outcome, crate::prefs::SaveOutcome::Error(folder.clone()));
        assert_eq!(after, "theme = \"Default\"\n");
        std::fs::write(&lock, b"").unwrap();
        let (outcome, after) = save_with(&path, || mode(&dir, 0o500), || mode(&dir, 0o700));
        assert_eq!(
            outcome,
            crate::prefs::SaveOutcome::Error(expect(
                AtomicSaveStage::CreateTemporary,
                SaveTouch::Folder
            ))
        );
        assert_eq!(after, "theme = \"Default\"\n");
        let (outcome, after) = save_with(&path, || mode(&path, 0o200), || mode(&path, 0o600));
        let unreadable = expect(AtomicSaveStage::Preflight, SaveTouch::Read);
        assert!(unreadable.contains("isn\u{2019}t readable"), "{unreadable}");
        assert_eq!(
            outcome,
            crate::prefs::SaveOutcome::Error(unreadable.clone())
        );
        assert_eq!(after, "theme = \"Default\"\n");
        let (outcome, after) = save_with(&path, || mode(&lock, 0o400), || mode(&lock, 0o600));
        let locked = expect(AtomicSaveStage::Preflight, SaveTouch::Lock);
        assert!(locked.contains("delete "), "{locked}");
        assert_eq!(outcome, crate::prefs::SaveOutcome::Error(locked.clone()));
        assert_eq!(after, "theme = \"Default\"\n");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod trail_style_tests {
    use super::{CURSOR_TRAIL_STYLES, Config, apply_prefs_edits};
    use crate::cursor_glow::GlowStyle;
    use aterm_effects::cursor_fx::GlowSpelling;
    use aterm_effects::kitty_pet::PetSpecies;

    fn set(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn a_word_unit_is_singular_after_exactly_one() {
        use super::{
            EDIT_CURSOR_TRAIL_LENGTH, EDIT_CURSOR_TRAIL_MS, EDIT_CURSOR_TRAIL_RADIUS,
            editable_fields, integer_reading, numeric_reading,
        };
        assert_eq!(integer_reading(EDIT_CURSOR_TRAIL_LENGTH, 1), "1 cell");
        assert_eq!(integer_reading(EDIT_CURSOR_TRAIL_LENGTH, 24), "24 cells");
        assert_eq!(
            numeric_reading(EDIT_CURSOR_TRAIL_RADIUS, 1.0, "1").as_deref(),
            Some("1 cell")
        );
        assert_eq!(
            numeric_reading(EDIT_CURSOR_TRAIL_RADIUS, 0.6, "0.6").as_deref(),
            Some("0.6 cells")
        );
        assert_eq!(integer_reading(EDIT_CURSOR_TRAIL_MS, 1), "1 ms");
        let doc = "cursor_trail_length = 1\ncursor_trail_radius = 1.0\n";
        let fields = editable_fields(&Config::parse(doc).expect("parses"));
        let placeholder = |key: &str| {
            fields
                .iter()
                .find(|field| field.key == key)
                .map(|field| field.placeholder.clone())
        };
        assert_eq!(
            placeholder(EDIT_CURSOR_TRAIL_LENGTH).as_deref(),
            Some("1 cell")
        );
        assert_eq!(
            placeholder(EDIT_CURSOR_TRAIL_RADIUS).as_deref(),
            Some("1 cell")
        );
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Look {
        style: Option<GlowStyle>,
        pet: Option<PetSpecies>,
        ribbon_tall: bool,
        ribbon_flat: bool,
        classic_mono: bool,
        comet: bool,
        beam: bool,
    }

    const OFF: Look = Look {
        style: None,
        pet: None,
        ribbon_tall: true,
        ribbon_flat: false,
        classic_mono: false,
        comet: false,
        beam: true,
    };

    const fn styled(style: GlowStyle) -> Look {
        Look {
            style: Some(style),
            ..OFF
        }
    }

    const fn ribbon(pet: Option<PetSpecies>) -> Look {
        Look {
            style: Some(GlowStyle::RainbowKitty),
            pet,
            beam: false,
            ..OFF
        }
    }

    const BEFORE: &[(&str, Look)] = &[
        ("phaser", styled(GlowStyle::Phaser)),
        ("rainbow kitty pet", ribbon(Some(PetSpecies::Cat))),
        ("rainbow dog pet", ribbon(Some(PetSpecies::Dog))),
        ("rainbow kitty flying", ribbon(None)),
        (
            "rainbow kitty underline",
            Look {
                ribbon_tall: false,
                ..ribbon(Some(PetSpecies::Cat))
            },
        ),
        (
            "rainbow kitty flat",
            Look {
                ribbon_flat: true,
                ..ribbon(Some(PetSpecies::Cat))
            },
        ),
        (
            "comet",
            Look {
                comet: true,
                ..styled(GlowStyle::Comet)
            },
        ),
        ("lumen", styled(GlowStyle::Lumen)),
        ("sparkle", styled(GlowStyle::Sparkle)),
        ("fire", styled(GlowStyle::Fire)),
        ("laser", styled(GlowStyle::Laser)),
        (
            "water",
            Look {
                beam: false,
                ..styled(GlowStyle::Water)
            },
        ),
        ("beam", styled(GlowStyle::Beam)),
        ("classic", styled(GlowStyle::Classic)),
        (
            "classic mono",
            Look {
                classic_mono: true,
                ..styled(GlowStyle::Classic)
            },
        ),
        ("off", OFF),
    ];

    const BEFORE_ALIASES: &[(&str, &str)] = &[
        ("classic wake", "classic"),
        ("classic lumen", "classic mono"),
        ("mono classic", "classic mono"),
        ("nyan rainbow", "rainbow kitty flying"),
        ("nyan", "rainbow kitty flying"),
        ("rainbow", "rainbow kitty flying"),
        ("flying kitty", "rainbow kitty flying"),
        ("kitty flying", "rainbow kitty flying"),
        ("kitty", "rainbow kitty pet"),
        ("kitty pet", "rainbow kitty pet"),
        ("pet kitty", "rainbow kitty pet"),
        ("dog pet", "rainbow dog pet"),
        ("pet dog", "rainbow dog pet"),
        ("rainbow puppy pet", "rainbow dog pet"),
        ("rainbow underline", "rainbow kitty underline"),
        ("underline rainbow", "rainbow kitty underline"),
        ("nyan underline", "rainbow kitty underline"),
        ("rainbow kitty", "rainbow kitty pet"),
        ("rainbow kitty tall", "rainbow kitty pet"),
        ("rainbow tall", "rainbow kitty pet"),
        ("tall rainbow", "rainbow kitty pet"),
        ("nyan tall", "rainbow kitty pet"),
        ("rainbow flat", "rainbow kitty flat"),
        ("flat rainbow", "rainbow kitty flat"),
        ("nyan flat", "rainbow kitty flat"),
        ("sparkles", "sparkle"),
        ("phaser-sparkle", "sparkle"),
        ("rainbow-sparkle", "sparkle"),
        ("ember", "fire"),
        ("embers", "fire"),
        ("ocean", "water"),
        ("wave", "water"),
        ("lightbeam", "beam"),
        ("light-beam", "beam"),
    ];

    fn native_look(raw: &str) -> (Look, bool) {
        let p = crate::app_config::resolve_trail_presentation(raw);
        let look = Look {
            style: p.style.style,
            pet: p.pet_species,
            ribbon_tall: p.ribbon_tall,
            ribbon_flat: p.ribbon_flat,
            classic_mono: p.classic_mono,
            comet: p.comet,
            beam: p.beam,
        };
        (look, p.style.unknown)
    }

    fn web_look(raw: &str) -> Look {
        let s = GlowSpelling::parse(raw);
        Look {
            style: s.style,
            pet: GlowSpelling::pet_of(raw),
            ribbon_tall: s.ribbon_tall,
            ribbon_flat: s.ribbon_flat,
            classic_mono: s.classic_mono,
            comet: s.style == Some(GlowStyle::Comet),
            beam: s.beam,
        }
    }

    #[test]
    fn no_spelling_that_loaded_before_changes_what_it_draws() {
        let before = |name: &str| {
            BEFORE
                .iter()
                .find(|(n, _)| *n == name)
                .map(|&(_, look)| look)
                .unwrap_or_else(|| panic!("{name:?} is not a 5e5f7cb17f look"))
        };
        let spellings = BEFORE
            .iter()
            .map(|&(name, _)| (name, name))
            .chain(BEFORE_ALIASES.iter().copied());
        for (raw, then) in spellings {
            let want = before(then);
            for spelt in [
                raw.to_string(),
                raw.to_ascii_uppercase(),
                format!("  {raw} "),
            ] {
                let (native, unknown) = native_look(&spelt);
                assert!(!unknown, "{spelt:?} loads without a warning");
                assert_eq!(native, want, "native {spelt:?} draws what it drew");
                assert_eq!(web_look(&spelt), native, "web {spelt:?} agrees with native");
            }
        }
        for (new, row) in [("lightning", "laser"), ("rainbow dog", "rainbow dog pet")] {
            assert_eq!(native_look(new), (before(row), false), "{new:?}");
            assert_eq!(web_look(new), before(row), "{new:?}");
        }
        assert_ne!(before("rainbow kitty flying"), before("rainbow kitty pet"));
        assert_ne!(before("rainbow kitty flat"), before("rainbow kitty pet"));
        let (typo, unknown) = native_look("rainbow kity");
        assert!(unknown);
        assert_eq!(typo, before(super::DEFAULT_CURSOR_TRAIL_STYLE));
        assert_eq!(web_look("rainbow kity").style, Some(GlowStyle::Lumen));
    }

    #[test]
    fn validate_config_accepts_aliased_trail_styles() {
        let baseline = crate::diagnostics::validate_config_text("").expect("empty config");
        for style in [
            "classic mono",
            "rainbow kitty tall",
            "rainbow kitty",
            "nyan rainbow",
            "rainbow dog",
            "lightning",
        ] {
            let warnings = crate::diagnostics::validate_config_text(&format!(
                "cursor_trail_style = {style:?}\n"
            ))
            .expect("parses");
            assert_eq!(warnings, baseline, "{style:?}");
        }
        for unknown in ["lightnin", "kitty v2"] {
            let warnings = crate::diagnostics::validate_config_text(&format!(
                "cursor_trail_style = {unknown:?}\n"
            ))
            .expect("parses");
            assert!(
                warnings.iter().any(|w| w.contains("cursor_trail_style")),
                "{unknown:?} is still flagged: {warnings:?}"
            );
        }
    }

    #[test]
    fn the_unset_default_is_the_full_body_pet_all_the_way_to_the_draw_seam() {
        let unset = crate::app_config::Config::default();
        let raw = unset.cursor_trail_style_raw();
        assert_eq!(raw, super::DEFAULT_CURSOR_TRAIL_STYLE);
        assert!(
            CURSOR_TRAIL_STYLES.contains(&raw),
            "the default must be a selectable option, got {raw:?}"
        );
        assert!(
            GlowStyle::style_names_kitty_pet(raw),
            "an unset config must draw the WALKING pet, not the flying head"
        );
        assert_eq!(
            GlowStyle::parse(raw),
            GlowStyle::RainbowKitty,
            "the pet still rides the rainbow ribbon"
        );
        assert!(GlowStyle::style_names_kitty_pet("rainbow kitty"));
        assert!(GlowStyle::style_names_kitty_pet("kitty"));
        for flying in ["rainbow kitty flying", "flying kitty", "kitty flying"] {
            let canonical =
                super::cursor_trail_style_canonical(flying).expect("documented flying spelling");
            assert!(
                CURSOR_TRAIL_STYLES.contains(&canonical),
                "the flying head must stay a real picker option, got {canonical:?}"
            );
            assert!(
                !GlowStyle::style_names_any_pet(flying)
                    && !GlowStyle::style_names_any_pet(canonical),
                "{flying:?} must keep drawing the flying head"
            );
            assert_eq!(GlowStyle::parse(flying), GlowStyle::RainbowKitty);
        }
        for legacy in ["nyan", "nyan rainbow", "rainbow"] {
            assert!(
                !GlowStyle::style_names_kitty_pet(legacy),
                "{legacy:?} must keep resolving to the flying head"
            );
            assert!(
                !GlowStyle::style_names_kitty_pet(
                    super::cursor_trail_style_canonical(legacy).expect("documented alias")
                ),
                "{legacy:?}'s canonical name must draw the same animal it does"
            );
        }
    }

    #[test]
    fn trail_sound_style_canonical_resolves_aliases_and_the_loader_agrees() {
        use aterm_effects::cursor_glow::GlowStyle;
        use aterm_effects::trail_sound::SoundVoice;
        for &s in super::TRAIL_SOUND_STYLES {
            assert_eq!(super::trail_sound_style_canonical(s), Some(s));
            assert_eq!(
                super::trail_sound_style_canonical(&format!(" {} ", s.to_ascii_uppercase())),
                Some(s)
            );
        }
        for &(alias, voice) in SoundVoice::ALIASES {
            assert_eq!(
                super::trail_sound_style_canonical(alias),
                Some(voice.name()),
                "{alias}"
            );
            let cfg = Config {
                trail_sound_style: Some(alias.to_string()),
                ..Config::default()
            };
            assert_eq!(cfg.trail_sound_voice(), voice, "loader: {alias}");
            if !super::TRAIL_SOUND_STYLES.contains(&alias) {
                assert!(
                    apply_prefs_edits("", &[(super::EDIT_TRAIL_SOUND_STYLE, set(alias))]).is_err(),
                    "writer must refuse alias {alias:?}"
                );
            }
        }
        for (raw, voice) in [
            ("mech", SoundVoice::Mech),
            ("mechanical", SoundVoice::Mech),
            ("water", SoundVoice::Of(GlowStyle::Water)),
            ("music box", SoundVoice::RainbowKittyV2),
            ("Glass Bell", SoundVoice::RainbowKittyV2),
            ("  felt ", SoundVoice::Felt),
            ("auto", SoundVoice::Style),
            ("garbage", SoundVoice::Style),
            ("", SoundVoice::Style),
        ] {
            let cfg = Config {
                trail_sound_style: Some(raw.to_string()),
                ..Config::default()
            };
            assert_eq!(cfg.trail_sound_voice(), voice, "{raw:?}");
        }
        assert_eq!(Config::default().trail_sound_voice(), SoundVoice::Style);
        assert_eq!(super::trail_sound_style_canonical("kazoo"), None);
        assert_eq!(super::trail_sound_style_canonical(""), None);
        for &o in super::TRAIL_SOUND_STYLES {
            let out = apply_prefs_edits("", &[(super::EDIT_TRAIL_SOUND_STYLE, set(o))]).unwrap();
            assert!(
                out.contains(&format!("trail_sound_style = \"{o}\"")),
                "{out}"
            );
            let up = apply_prefs_edits(
                "",
                &[(super::EDIT_TRAIL_SOUND_STYLE, set(&o.to_ascii_uppercase()))],
            )
            .unwrap();
            assert!(up.contains(&format!("trail_sound_style = \"{o}\"")), "{up}");
        }
    }
}

#[cfg(test)]
mod edit_tests {
    use super::{
        Config, EDIT_BACKGROUND, EDIT_BACKGROUND_OPACITY, EDIT_COLUMNS, EDIT_COPY_ON_SELECT,
        EDIT_CURSOR_COLOR, EDIT_CURSOR_STYLE, EDIT_CURSOR_TRAIL_STYLE, EDIT_FONT_FAMILY,
        EDIT_FONT_PX, EDIT_FONT_WEIGHT, EDIT_FONT_WEIGHT_DARK_NUDGE, EDIT_FOREGROUND,
        EDIT_LIGATURES, EDIT_LINE_HEIGHT, EDIT_LINES, EDIT_SCROLLBACK, EDIT_SEARCH_HISTORY_LINES,
        EDIT_SELECTION_COLOR, EDIT_THEME, EDIT_WINDOW_PADDING, EDIT_WINDOW_PADDING_TOP, EditKind,
        PrefsEditError, Range, apply_prefs_edits, editable_fields, keywords_of, range_of,
    };

    fn set(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn dotted_matrix_rain_key_types_as_bool() {
        assert_eq!(
            super::edit_kind(super::EDIT_MATRIX_RAIN_ENABLED),
            EditKind::Bool
        );
        assert!(matches!(
            apply_prefs_edits("", &[(super::EDIT_MATRIX_RAIN_ENABLED, set("yes"))]),
            Err(PrefsEditError::BadValue { .. })
        ));
    }

    #[test]
    fn dotted_remove_reverts_and_prunes_only_a_bare_table() {
        let keep_sibling = "[matrix_rain]\nenabled = true\nfps = 24\n";
        let out = apply_prefs_edits(keep_sibling, &[(super::EDIT_MATRIX_RAIN_ENABLED, None)])
            .expect("dotted remove");
        assert!(!out.contains("enabled"), "child removed");
        assert!(
            out.contains("[matrix_rain]") && out.contains("fps = 24"),
            "table + sibling survive: {out:?}"
        );

        let lone = "font_px = 13.0\n[matrix_rain]\n# mine\nenabled = true\n";
        let out = apply_prefs_edits(lone, &[(super::EDIT_MATRIX_RAIN_ENABLED, None)])
            .expect("dotted remove of the last child");
        assert_eq!(
            out, "font_px = 13.0\n[matrix_rain]\n# mine\n",
            "an emptied table is retained, with the comment above its last key"
        );
        let bare = "font_px = 13.0\n\n[matrix_rain]\nenabled = true\n";
        let out = apply_prefs_edits(bare, &[(super::EDIT_MATRIX_RAIN_ENABLED, None)])
            .expect("dotted remove of the last uncommented child");
        assert_eq!(
            out, "font_px = 13.0\n",
            "a bare table goes with its last key"
        );
        let cfg: crate::app_config::Config = aterm_toml::from_str(&out).expect("round-trip");
        assert!(
            !cfg.matrix_rain_enabled(),
            "an emptied [matrix_rain] resolves to the same default as an absent one"
        );

        let out = apply_prefs_edits(
            "font_px = 13.0\n",
            &[(super::EDIT_MATRIX_RAIN_ENABLED, None)],
        )
        .expect("remove-from-absent is a no-op");
        assert_eq!(out, "font_px = 13.0\n");
    }

    #[test]
    fn matrix_rain_row_surfaces_in_the_settings_model() {
        let key = super::EDIT_MATRIX_RAIN_ENABLED;
        assert_eq!(super::section_of(key), super::Section::Appearance);
        assert_eq!(super::group_of(key), ("Effects", 5));
        assert!(super::footnote_of(key).is_some());
        assert!(!keywords_of(key).is_empty());
        assert!(
            super::nested_leaf(key).is_some(),
            "the row is registered ONCE, via NESTED_LEAVES (no duplicate EditField)"
        );

        let unset = editable_fields(&Config::default());
        let row = unset.iter().find(|f| f.key == key).expect("row exists");
        assert_eq!(row.kind, EditKind::Bool);
        assert_eq!(row.seed.as_deref(), Some("false"), "resolved default OFF");

        let on: Config = aterm_toml::from_str("[matrix_rain]\nenabled = true").unwrap();
        let row_on = editable_fields(&on)
            .into_iter()
            .find(|f| f.key == key)
            .unwrap();
        assert_eq!(row_on.seed.as_deref(), Some("true"), "resolved ON");
    }

    #[test]
    fn the_output_streak_switch_is_a_row_seeded_where_the_engine_starts() {
        let key = super::EDIT_OUTPUT_STREAK_ENABLED;
        let row = editable_fields(&Config::default())
            .into_iter()
            .find(|f| f.key == key)
            .expect("the row exists on an unconfigured install");
        assert_eq!(row.label, "Output streak");
        assert_eq!(row.kind, EditKind::Bool);
        assert_eq!(super::section_of(key), super::Section::Appearance);
        assert_eq!(super::group_of(key), ("Effects", 5));
        assert_eq!(
            row.seed.as_deref(),
            Some(if crate::app_config::DEFAULT_DECORATIVE_EFFECTS {
                "true"
            } else {
                "false"
            }),
            "the resolved platform default"
        );
        let off: Config = aterm_toml::from_str("[output_streak]\nenabled = false").unwrap();
        let row_off = editable_fields(&off)
            .into_iter()
            .find(|f| f.key == key)
            .unwrap();
        assert_eq!(row_off.seed.as_deref(), Some("false"), "resolved OFF");
        let written = apply_prefs_edits("", &[(key, set("false"))])
            .expect("the nested writer creates [output_streak]");
        assert!(written.contains("[output_streak]"), "{written}");
        let reparsed: Config = aterm_toml::from_str(&written).expect("re-parses as Config");
        assert!(!reparsed.output_streak_enabled_or_default());
    }

    #[test]
    fn the_harness_rows_round_trip_through_set_and_unset() {
        use aterm_agent::supervise::config::Approve;
        use aterm_agent::supervise::{Autopilot, QuestionPriority, SupervisorConfig};
        let seed = |cfg: &Config, key: &str| {
            editable_fields(cfg)
                .into_iter()
                .find(|f| f.key == key)
                .expect("the row exists")
                .seed
        };
        let full = SupervisorConfig::default();
        type Row = (
            &'static str,
            Option<&'static str>,
            &'static str,
            &'static str,
            fn(&SupervisorConfig) -> bool,
        );
        let rows: [Row; 5] = [
            (
                super::EDIT_HARNESS_ENABLED,
                Some("true"),
                "false",
                "false",
                |p| !p.enabled,
            ),
            (super::EDIT_HARNESS_AUTOPILOT, None, "Land", "land", |p| {
                p.autopilot == Autopilot::Land && p.enabled
            }),
            (super::EDIT_HARNESS_APPROVE, None, "safe", "safe", |p| {
                p.approve == Approve::Safe && p.enabled
            }),
            (
                super::EDIT_HARNESS_ANSWER_QUESTIONS,
                Some("true"),
                "false",
                "false",
                |p| !p.answer_questions && p.approve == Approve::All,
            ),
            (
                super::EDIT_HARNESS_QUESTION_PRIORITY,
                None,
                "Short-Term",
                "short-term",
                |p| p.question_priority == QuestionPriority::ShortTerm && p.answer_questions,
            ),
        ];
        for (key, absent, typed, stored, took) in rows {
            assert_eq!(super::section_of(key), super::Section::Harness, "{key}");
            assert_eq!(super::environment_precedence(key), None, "{key}");
            assert_eq!(super::application_timing(key), None, "{key}");
            assert!(super::application_has_live_effect(key), "{key}");
            assert!(super::footnote_of(key).is_some(), "{key}");
            assert_eq!(seed(&Config::default(), key).as_deref(), absent, "{key}");

            let written = apply_prefs_edits("font_px = 14\n", &[(key, set(typed))]).expect("set");
            assert!(
                written.contains("font_px = 14") && written.contains("[harness]"),
                "{written}"
            );
            let cfg = Config::parse(&written).expect("re-parses as Config");
            assert!(cfg.harness_notices().is_empty(), "{written}");
            assert!(took(&cfg.harness.policy), "{written}");
            assert_eq!(seed(&cfg, key).as_deref(), Some(stored), "{written}");

            let cleared = apply_prefs_edits(&written, &[(key, None)]).expect("unset");
            let back = Config::parse(&cleared).expect("re-parses as Config");
            assert_eq!(back.harness.policy, full, "{cleared}");
        }
        for (key, word) in [
            (super::EDIT_HARNESS_QUESTION_PRIORITY, "long_term"),
            (super::EDIT_HARNESS_AUTOPILOT, "hover"),
        ] {
            assert!(
                apply_prefs_edits("", &[(key, set(word))]).is_err(),
                "{key}: a word outside the three is refused at Save"
            );
        }

        let (autopilot, approve, answer, priority) = (
            super::EDIT_HARNESS_AUTOPILOT,
            super::EDIT_HARNESS_APPROVE,
            super::EDIT_HARNESS_ANSWER_QUESTIONS,
            super::EDIT_HARNESS_QUESTION_PRIORITY,
        );
        for (line, key, seeded, limits) in [
            ("autopilot = \"no\"", autopilot, Some("off"), true),
            ("autopilot = \"sometimes\"", autopilot, None, false),
            ("approve = \"no\"", approve, Some("none"), true),
            ("approve = \"most\"", approve, None, false),
            ("answer_questions = 0", answer, Some("false"), true),
            (
                "answer_questions = \"sometimes\"",
                answer,
                Some("true"),
                false,
            ),
            ("question_priority = \"sometimes\"", priority, None, false),
        ] {
            let cfg = Config::parse(&format!("[harness]\n{line}\n")).expect("parses");
            assert_eq!(seed(&cfg, key).as_deref(), seeded, "{line}");
            assert_eq!(cfg.harness.policy != full, limits, "{line}");
        }
    }

    #[test]
    fn each_ai_use_switch_round_trips_through_set_and_unset() {
        use crate::app_config::AiPolicy;
        let field = |cfg: &Config, key: &str| {
            editable_fields(cfg)
                .into_iter()
                .find(|f| f.key == key)
                .unwrap_or_else(|| panic!("{key}: no row"))
        };
        let seed = |cfg: &Config, key: &str| field(cfg, key).seed;
        let ids = aterm_agent::ai::connector::ids();
        let key_of = |id: &str| format!("ai.{id}.use");
        for id in ids.iter().copied() {
            let key = key_of(id);
            let key = key.as_str();
            let cli = aterm_agent::ai::connector::connector(id).expect("a connection");
            let row = super::NESTED_LEAVES
                .iter()
                .find(|leaf| leaf.key == key)
                .unwrap_or_else(|| panic!("{key}: no Settings row for {id}"));
            assert_eq!(
                row.label,
                format!("Use {} on this machine", cli.product),
                "{key}"
            );
            assert_eq!(super::ai_use_switch(key).map(|c| c.id), Some(id), "{key}");
            assert_eq!(super::section_of(key), super::Section::Harness, "{key}");
            assert_eq!(super::group_of(key), ("AI CLIs", 1), "{key}");
            assert_eq!(super::application_timing(key), None, "{key}: live");
            assert!(super::application_has_live_effect(key), "{key}");
            assert_eq!(super::footnote_of(key), Some(super::AI_USE_NOTE), "{key}");
            assert!(!keywords_of(key).is_empty(), "{key}");
            assert_eq!(super::edit_kind(key), EditKind::Bool, "{key}");
            assert_eq!(seed(&Config::default(), key).as_deref(), Some("true"));

            let written = apply_prefs_edits("font_px = 14\n", &[(key, set("false"))]).expect("set");
            assert!(
                written.contains("font_px = 14")
                    && written.contains(&format!("[ai.{id}]\nuse = false")),
                "{written}"
            );
            let cfg = Config::parse(&written).expect("re-parses as Config");
            assert!(cfg.ai.notes.is_empty(), "{:?}", cfg.ai.notes);
            assert!(!cfg.ai.settings().wants(id), "{written}");
            assert_eq!(seed(&cfg, key).as_deref(), Some("false"), "{written}");
            for other in ids.iter().copied().filter(|other| *other != id) {
                assert!(cfg.ai.settings().wants(other), "{other}: {written}");
                assert_eq!(
                    seed(&cfg, &key_of(other)).as_deref(),
                    Some("true"),
                    "{other}"
                );
            }

            let cleared = apply_prefs_edits(&written, &[(key, None)]).expect("unset");
            assert!(!cleared.contains("[ai"), "no empty table left: {cleared}");
            let back = Config::parse(&cleared).expect("re-parses as Config");
            assert!(back.ai.settings().wants(id), "{cleared}");
            assert_eq!(seed(&back, key).as_deref(), Some("true"), "{cleared}");
        }
        for key in [
            "ai.nonesuch.use",
            "ai.codex.enabled",
            "ai.use",
            "harness.codex.enabled",
            super::EDIT_AI_ENABLED,
            super::EDIT_AI_PREFERRED,
        ] {
            assert!(super::ai_use_switch(key).is_none(), "{key}");
        }

        let preferred = super::EDIT_AI_PREFERRED;
        assert_eq!(
            super::edit_kind(preferred),
            EditKind::Enum {
                options: &aterm_agent::ai::connector::IDS
            }
        );
        let default = aterm_agent::ai::config::default_preferred().id;
        let bare = field(&Config::default(), preferred);
        assert_eq!(bare.seed, None);
        assert_eq!(bare.placeholder, format!("{default} (default)"));
        let written = apply_prefs_edits("", &[(preferred, set("codex"))]).expect("set");
        let cfg = Config::parse(&written).expect("re-parses as Config");
        assert_eq!(
            cfg.ai.settings().preferred.1,
            aterm_agent::ai::config::From::Local
        );
        assert_eq!(seed(&cfg, preferred).as_deref(), Some("codex"));
        let written =
            apply_prefs_edits("", &[(super::EDIT_AI_ENABLED, set("false"))]).expect("set");
        let cfg = Config::parse(&written).expect("re-parses as Config");
        assert!(!cfg.ai.enabled());
        assert_eq!(seed(&cfg, super::EDIT_AI_ENABLED).as_deref(), Some("false"));
        assert_eq!(
            seed(&Config::default(), super::EDIT_AI_ENABLED).as_deref(),
            Some("true")
        );

        let scratch = aterm_tempfile::tempdir().expect("scratch folder");
        let dir = scratch.path();
        std::fs::write(
            dir.join(aterm_agent::ai::config::FLEET_FILE),
            "schema = 1\npreferred = \"codex\"\n[muse]\nuse = false\n",
        )
        .expect("the fleet document");
        let over = |text: &str| {
            let mut cfg = Config::parse(text).expect("parses");
            cfg.ai = AiPolicy::read_beside(text, Some(dir));
            cfg
        };
        let fleet = over("");
        assert_eq!(
            fleet.ai.settings().preferred.1,
            aterm_agent::ai::config::From::Fleet
        );
        assert_eq!(seed(&fleet, preferred).as_deref(), Some("codex"));
        assert_eq!(seed(&fleet, &key_of("muse")).as_deref(), Some("false"));
        assert_eq!(
            fleet.path_feed_paths(),
            vec![dir.join(aterm_agent::ai::config::FLEET_FILE)]
        );
        let local = over("[ai]\npreferred = \"claude\"\n[ai.muse]\nuse = true\n");
        assert_eq!(seed(&local, preferred).as_deref(), Some("claude"));
        assert_eq!(seed(&local, &key_of("muse")).as_deref(), Some("true"));
        let reset = apply_prefs_edits("[ai]\npreferred = \"claude\"\n", &[(preferred, None)])
            .expect("unset");
        assert_eq!(
            seed(&over(&reset), preferred).as_deref(),
            Some("codex"),
            "{reset}"
        );
        #[cfg(unix)]
        {
            let path = dir.join(aterm_agent::ai::config::FLEET_FILE);
            std::fs::remove_file(&path).expect("remove own fleet fixture");
            std::os::unix::fs::symlink(dir.join("missing-fleet"), &path)
                .expect("dangling fleet entry");
            let unknown = over("[ai]\nreviewers = [\"codex\"]\n");
            assert!(matches!(
                unknown.ai.settings().fleet,
                aterm_agent::ai::config::FleetState::Refused { .. }
            ));
            assert_eq!(
                unknown.ai.settings().reviewers.1,
                aterm_agent::ai::config::From::Local,
                "a refused document applies nothing: this machine's list stands"
            );
            assert!(unknown.path_feed_paths().contains(&path));
        }
    }

    #[test]
    fn each_agent_switch_round_trips_through_set_and_unset() {
        use aterm_agent::supervise::SupervisorConfig;
        use aterm_phase::Program;
        let seed = |cfg: &Config, key: &str| {
            editable_fields(cfg)
                .into_iter()
                .find(|f| f.key == key)
                .expect("the row exists")
                .seed
        };
        let full = SupervisorConfig::default();
        let agents: Vec<Program> = Program::ALL
            .into_iter()
            .filter(|p| p.harness_table())
            .collect();
        let key_of = |agent: Program| format!("harness.{}.enabled", agent.name());
        for agent in agents.iter().copied() {
            let key = key_of(agent);
            let key = key.as_str();
            let row = super::NESTED_LEAVES
                .iter()
                .find(|leaf| leaf.key == key)
                .unwrap_or_else(|| panic!("{key}: no Settings row for {agent:?}"));
            assert_eq!(
                row.label,
                super::agent_switch_label(agent.product()),
                "{key}"
            );
            assert_eq!(super::agent_switch(key), Some(agent), "{key}");
            assert_eq!(super::section_of(key), super::Section::Harness, "{key}");
            assert_eq!(super::application_timing(key), None, "{key}: live");
            assert!(super::application_has_live_effect(key), "{key}");
            assert!(!keywords_of(key).is_empty(), "{key}");
            assert_eq!(
                super::footnote_of(key),
                Some(super::HARNESS_AGENT_NOTE),
                "{key}"
            );
            assert_eq!(super::edit_kind(key), EditKind::Bool, "{key}");
            assert_eq!(seed(&Config::default(), key).as_deref(), Some("true"));

            let written = apply_prefs_edits("font_px = 14\n", &[(key, set("false"))]).expect("set");
            assert!(
                written.contains("font_px = 14")
                    && written.contains(&format!("[harness.{}]\nenabled = false", agent.name())),
                "{written}"
            );
            let cfg = Config::parse(&written).expect("re-parses as Config");
            assert!(cfg.harness_notices().is_empty(), "{written}");
            assert!(cfg.harness.notes.is_empty(), "{:?}", cfg.harness.notes);
            let policy = &cfg.harness.policy;
            assert!(!policy.on_for(agent), "{written}");
            assert!(policy.enabled, "{written}");
            assert_eq!(seed(&cfg, key).as_deref(), Some("false"), "{written}");
            for other in agents.iter().copied().filter(|other| *other != agent) {
                assert!(policy.on_for(other), "{other:?}: {written}");
                assert_eq!(
                    seed(&cfg, &key_of(other)).as_deref(),
                    Some("true"),
                    "{other:?}: {written}"
                );
            }

            let cleared = apply_prefs_edits(&written, &[(key, None)]).expect("unset");
            assert!(
                !cleared.contains("[harness"),
                "no empty table left: {cleared}"
            );
            let back = Config::parse(&cleared).expect("re-parses as Config");
            assert_eq!(back.harness.policy, full, "{cleared}");
            assert!(back.harness.policy.on_for(agent), "{cleared}");
            assert_eq!(seed(&back, key).as_deref(), Some("true"), "{cleared}");
        }
        for key in [
            super::EDIT_HARNESS_ENABLED,
            "harness.generic.enabled",
            "harness.claude-code.enabled",
            "harness.disk.enabled",
            "harness.claude.approve",
            "claude.enabled",
        ] {
            assert_eq!(super::agent_switch(key), None, "{key}");
        }
        let (claude, codex) = (
            super::EDIT_HARNESS_CLAUDE_ENABLED,
            super::EDIT_HARNESS_CODEX_ENABLED,
        );

        for text in [
            "[harness.codex]\nenabled = false\n",
            "[harness.codex]\nenabled = \"no\"\n",
            "[harness]\ncodex.enabled = false\n",
        ] {
            let cfg = Config::parse(text).expect("parses");
            assert_eq!(seed(&cfg, codex).as_deref(), Some("false"), "{text}");
            assert_eq!(seed(&cfg, claude).as_deref(), Some("true"), "{text}");
            assert_eq!(
                seed(&cfg, super::EDIT_HARNESS_ENABLED).as_deref(),
                Some("true"),
                "{text}"
            );
        }
        let off = Config::parse("[harness]\nenabled = false\n").expect("parses");
        for agent in agents {
            assert!(!off.harness.policy.on_for(agent), "{agent:?}");
            assert_eq!(
                seed(&off, &key_of(agent)).as_deref(),
                Some("true"),
                "{agent:?}"
            );
        }
    }

    #[test]
    fn packages_consent_rows_surface_in_the_settings_model() {
        for key in [
            super::EDIT_PACKAGES_ENABLED,
            super::EDIT_PACKAGES_AUTO_INSTALL,
        ] {
            assert_eq!(super::edit_kind(key), EditKind::Bool, "{key} types Bool");
        }

        let fields = editable_fields(&Config::default());
        for (retired, _) in super::RETIRED_PACKAGES_SPELLINGS {
            assert!(
                fields.iter().all(|f| f.key != *retired),
                "{retired} is retired: no row writes it"
            );
        }
        let enabled = fields
            .iter()
            .find(|f| f.key == super::EDIT_PACKAGES_ENABLED)
            .expect("Automatic updates row exists");
        assert_eq!(enabled.seed.as_deref(), Some("true"), "default ON");
        let auto_install = fields
            .iter()
            .find(|f| f.key == super::EDIT_PACKAGES_AUTO_INSTALL)
            .expect("install consent row exists");
        assert_eq!(
            auto_install.seed.as_deref(),
            Some("true"),
            "batteries included: default ON"
        );

        let seed = |text: &str, key: &str| {
            editable_fields(&aterm_toml::from_str::<Config>(text).unwrap())
                .into_iter()
                .find(|f| f.key == key)
                .unwrap()
                .seed
        };
        assert_eq!(
            seed(
                "[packages]\nauto_update = false\n",
                super::EDIT_PACKAGES_ENABLED
            )
            .as_deref(),
            Some("false"),
            "the retired auto_update = false seeds the switch off"
        );
        assert_eq!(
            seed(
                "[packages]\nseed_install = false\n",
                super::EDIT_PACKAGES_AUTO_INSTALL
            )
            .as_deref(),
            Some("false"),
            "the retired seed_install = false seeds the consent off"
        );
        assert_eq!(
            seed(
                "[packages]\nenabled = false\nauto_install = true\n",
                super::EDIT_PACKAGES_ENABLED
            )
            .as_deref(),
            Some("false")
        );
    }

    #[test]
    fn the_update_and_reroute_switches_are_rows_and_the_repoint_is_not() {
        let fields = editable_fields(&Config::default());
        for key in [super::EDIT_UPDATE_ENABLED, super::EDIT_REROUTE_ANNOUNCE] {
            assert_eq!(super::edit_kind(key), EditKind::Bool, "{key}");
            let row = fields.iter().find(|f| f.key == key).expect("a row");
            assert_eq!(row.seed.as_deref(), Some("true"), "{key} defaults ON");
            assert_eq!(super::environment_precedence(key), None, "{key}");
        }
        for key in [
            "update.owner",
            "update.repo",
            "update.require_team_id",
            "packages.account",
        ] {
            assert!(fields.iter().all(|f| f.key != key), "{key} is not a row");
            assert_eq!(super::environment_precedence(key), None, "{key}");
        }
        let off: Config =
            aterm_toml::from_str("[update]\nenabled = false\n[reroute]\nannounce = false\n")
                .unwrap();
        let fields = editable_fields(&off);
        for key in [super::EDIT_UPDATE_ENABLED, super::EDIT_REROUTE_ANNOUNCE] {
            let row = fields.iter().find(|f| f.key == key).unwrap();
            assert_eq!(row.seed.as_deref(), Some("false"), "{key}");
        }
    }

    #[test]
    fn packages_dotted_writes_preserve_the_atpkg_owned_siblings() {
        let existing = "# consent ledger\n[packages]\naccount = \"alabsystems\" # index owner\nexclude = [\"trust\"]\nseed_install = false\nauto_update = false\n\n[packages.links]\nay = \"~/ay\"\n";
        let out = apply_prefs_edits(
            existing,
            &[(super::EDIT_PACKAGES_AUTO_INSTALL, set("true"))],
        )
        .expect("dotted write");
        assert!(out.contains("# consent ledger"), "comment survives");
        assert!(
            out.contains("account = \"alabsystems\" # index owner"),
            "atpkg-owned sibling + inline note survive: {out:?}"
        );
        assert!(out.contains("[packages.links]") && out.contains("ay = \"~/ay\""));
        assert!(out.contains("auto_install = true"));
        assert!(
            !out.contains("seed_install"),
            "writing the consent drops its retired spelling: {out:?}"
        );
        assert!(
            out.contains("auto_update = false"),
            "an unrelated retired key is left for its own switch: {out:?}"
        );
        let config: Config = aterm_toml::from_str(&out).expect("round-trips through serde");
        assert!(config.packages_auto_install());
        assert!(
            !config.packages_enabled(),
            "auto_update = false still reads as off"
        );
        let on = apply_prefs_edits(&out, &[(super::EDIT_PACKAGES_ENABLED, set("true"))])
            .expect("the switch");
        assert!(!on.contains("auto_update"), "{on:?}");
        let config: Config = aterm_toml::from_str(&on).unwrap();
        assert!(
            config.packages_enabled(),
            "the switch decides once it is written"
        );

        let reverted = apply_prefs_edits(&out, &[(super::EDIT_PACKAGES_AUTO_INSTALL, None)])
            .expect("dotted revert");
        assert!(!reverted.contains("auto_install"));
        assert!(
            reverted.contains("[packages]") && reverted.contains("account = \"alabsystems\""),
            "atpkg-owned keys keep the table alive: {reverted:?}"
        );
        let config: Config = aterm_toml::from_str(&reverted).unwrap();
        assert!(
            config.packages_auto_install(),
            "back to the batteries-included default"
        );

        assert!(matches!(
            apply_prefs_edits("", &[(super::EDIT_PACKAGES_ENABLED, set("yes"))]),
            Err(PrefsEditError::BadValue { .. })
        ));
    }

    #[test]
    fn removing_a_key_keeps_the_comment_lines_above_it() {
        for (existing, key, expected) in [
            (
                "# keep me\nfont_px = 14.0  # cozy\nfont_family = \"Menlo\"\n",
                EDIT_FONT_PX,
                "# keep me\nfont_family = \"Menlo\"\n",
            ),
            (
                "[matrix_rain]\n# mine\nenabled = true\nfps = 24\n",
                super::EDIT_MATRIX_RAIN_ENABLED,
                "[matrix_rain]\n# mine\nfps = 24\n",
            ),
            (
                "# rain\nmatrix_rain.enabled = true\nfont_px = 13.0\n",
                super::EDIT_MATRIX_RAIN_ENABLED,
                "# rain\nfont_px = 13.0\n",
            ),
            (
                "font_px = 13.0\n# alt themes\ntheme = \"X\"\n\n[update]\nenabled = false\n",
                EDIT_THEME,
                "font_px = 13.0\n# alt themes\n\n[update]\nenabled = false\n",
            ),
        ] {
            let out = apply_prefs_edits(existing, &[(key, None)]).expect("remove");
            assert_eq!(out, expected, "removing {key} from {existing:?}");
            aterm_toml::from_str::<Config>(&out).expect("the result still parses");
        }
    }

    #[test]
    fn reset_then_undo_keeps_the_comment_lines_above_the_key() {
        use crate::native_config_service::{ConfigPatchResult, VersionedConfigService};
        let comments = "# ---- My fonts ----\n# keep these in sync with my editor\n";
        let mut service = VersionedConfigService::new(format!(
            "{comments}font_px = 14.0\nfont_family = \"Menlo\"\n"
        ))
        .unwrap();
        let base = service.snapshot().revision;
        let ConfigPatchResult::Applied { snapshot, undo } =
            service.reset_all(base, [EDIT_FONT_PX.to_string()])
        else {
            panic!("the reset should commit");
        };
        assert_eq!(
            *snapshot.text,
            format!("{comments}font_family = \"Menlo\"\n")
        );
        let ConfigPatchResult::Applied { snapshot, .. } = service.undo(undo) else {
            panic!("the undo should commit");
        };
        assert_eq!(
            *snapshot.text,
            format!("{comments}font_px = 14.0\nfont_family = \"Menlo\"\n")
        );
    }

    #[test]
    fn preserves_comments_and_unrelated_keys() {
        let existing = "\
# my aterm config
font_px = 12.0  # cozy
gpu = true
[keybindings]
\"cmd+shift+t\" = \"new_tab\"
";
        let out = apply_prefs_edits(existing, &[(EDIT_THEME, set("Nord"))]).unwrap();
        assert!(out.contains("# my aterm config"), "{out}");
        assert!(out.contains("# cozy"), "{out}");
        assert!(out.contains("gpu = true"), "{out}");
        assert!(out.contains("[keybindings]"), "{out}");
        assert!(out.contains("\"cmd+shift+t\" = \"new_tab\""), "{out}");
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.theme.as_deref(), Some("Nord"));
        assert_eq!(c.font_px, Some(12.0));
    }

    #[test]
    fn value_edit_preserves_the_edited_keys_inline_comment() {
        let existing = "\
# header comment stays
font_px = 12.0  # cozy size
scrollback_lines = 5000 # deep history

[update]
auto_apply = true # hands off
";
        let out = apply_prefs_edits(
            existing,
            &[
                (EDIT_FONT_PX, set("18")),
                ("update.auto_apply", set("false")),
            ],
        )
        .unwrap();
        assert!(out.contains("font_px = 18  # cozy size"), "{out}");
        assert!(out.contains("auto_apply = false # hands off"), "{out}");
        assert!(out.contains("# header comment stays"), "{out}");
        assert!(
            out.contains("scrollback_lines = 5000 # deep history"),
            "{out}"
        );
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.font_px, Some(18.0));
        assert_eq!(c.update.as_ref().and_then(|u| u.auto_apply), Some(false));
        assert_eq!(c.scrollback_lines, Some(5000));
    }

    #[test]
    fn w2_w9_motion_keys_round_trip_and_surface() {
        let out = apply_prefs_edits(
            "",
            &[
                (super::EDIT_TEXT_BLENDING, set("linear")),
                (super::EDIT_FONT_THICKEN, set("true")),
                (super::EDIT_STEM_GAMMA, set("1.4")),
                (super::EDIT_FONT_WEIGHT, set("600")),
                (super::EDIT_FONT_VARIATION, set("wght=450, opsz=14")),
                (super::EDIT_MOTION, set("reduced")),
                (super::EDIT_SERIOUS_MODE, set("true")),
                (super::EDIT_LOAD_ADAPTIVE_MOTION, set("false")),
            ],
        )
        .expect("writes typed values");
        let c: Config = aterm_toml::from_str(&out).expect("round-trips through serde");
        assert_eq!(c.load_adaptive_motion, Some(false));
        assert!(!c.load_adaptive_motion_or_default());
        assert_eq!(
            c.text_blending_or_default(),
            aterm_render::TextBlending::Linear
        );
        assert!(c.font_thicken_or_default());
        assert_eq!(c.stem_gamma, Some(1.4));
        assert_eq!(c.font_weight, Some(600));
        assert_eq!(c.motion.as_deref(), Some("reduced"));
        assert_eq!(c.motion_mode(), crate::motion::MotionMode::Reduced);
        assert_eq!(c.serious_mode, Some(true));
        assert!(c.serious_mode_or_default());
        assert_eq!(
            c.font_variation.as_ref().map(|l| l.0.clone()),
            Some(vec!["wght=450".to_string(), "opsz=14".to_string()])
        );
        let arr: Config =
            aterm_toml::from_str("font_variation = [\"wght=450\", \"opsz=14\"]").unwrap();
        assert_eq!(
            arr.font_variation.map(|l| l.0),
            c.font_variation.as_ref().map(|l| l.0.clone())
        );
        let (reqs, warns) = c.font_variation_requests();
        assert!(warns.is_empty(), "clean specs warn nothing");
        assert!(
            reqs.iter().any(|&(t, _)| t == u32::from_be_bytes(*b"opsz")),
            "opsz axis reaches the engine"
        );
        assert_eq!(
            reqs.last().map(|&(t, v)| (t, v)),
            Some((aterm_render::variation::WGHT_TAG, 600.0)),
            "font_weight is appended last so it wins on conflict"
        );

        let fields = editable_fields(&c);
        let row = fields
            .iter()
            .find(|f| f.key == super::EDIT_FONT_VARIATION)
            .unwrap();
        assert_eq!(row.seed.as_deref(), Some("wght=450, opsz=14"));
    }

    #[test]
    fn types_each_field_correctly() {
        let out = apply_prefs_edits(
            "",
            &[
                (EDIT_FONT_PX, set("14")),
                (EDIT_SCROLLBACK, set("50000")),
                (EDIT_COPY_ON_SELECT, set("true")),
                (EDIT_THEME, set("Dracula")),
                (EDIT_FONT_FAMILY, set("JetBrains Mono")),
                (EDIT_CURSOR_STYLE, set("bar")),
            ],
        )
        .unwrap();
        let c: Config = aterm_toml::from_str(&out).expect("typed values round-trip");
        assert_eq!(c.font_px, Some(14.0)); // float, even from "14"
        assert_eq!(c.scrollback_lines, Some(50000)); // integer
        assert_eq!(c.copy_on_select, Some(true)); // bool
        assert_eq!(c.theme.as_deref(), Some("Dracula")); // string
        assert_eq!(c.font_family.as_deref(), Some("JetBrains Mono")); // string w/ space
        assert_eq!(c.cursor_style.as_deref(), Some("bar")); // string
        assert!(out.contains("font_px = 14"), "float unquoted: {out}");
        assert!(
            out.contains("scrollback_lines = 50000"),
            "int unquoted: {out}"
        );
        assert!(
            out.contains("copy_on_select = true"),
            "bool unquoted: {out}"
        );
    }

    #[test]
    fn bad_numeric_value_is_rejected() {
        let err = apply_prefs_edits("", &[(EDIT_FONT_PX, set("not-a-number"))]).unwrap_err();
        match err {
            PrefsEditError::BadValue { key, .. } => assert_eq!(key, EDIT_FONT_PX),
            other => panic!("expected BadValue, got {other:?}"),
        }
    }

    #[test]
    fn font_px_outside_the_resolvers_range_is_rejected_with_the_range() {
        let range = format!("{}..={}", crate::FONT_PX_MIN, crate::FONT_PX_MAX);
        for raw in [
            "0", "500", "-5", "0.0", "200.5", "5.99", "NaN", "inf", "-inf",
        ] {
            let err =
                apply_prefs_edits("theme = \"Nord\"\n", &[(EDIT_FONT_PX, set(raw))]).unwrap_err();
            match &err {
                PrefsEditError::OutOfRange { key, range: got } => {
                    assert_eq!(key, EDIT_FONT_PX, "{raw}");
                    assert_eq!(got, &range, "{raw}");
                }
                other => panic!("{raw}: expected OutOfRange, got {other:?}"),
            }
            assert_eq!(
                err.to_string(),
                format!("invalid value for font_px: must be {range}"),
                "{raw}: the wire reply is `ERR ` + this"
            );
        }
        for raw in ["6", "200", "14.5", "16"] {
            let out = apply_prefs_edits("", &[(EDIT_FONT_PX, set(raw))])
                .unwrap_or_else(|e| panic!("{raw} is inside the range: {e}"));
            let c: Config = aterm_toml::from_str(&out).expect("round-trips");
            assert_eq!(c.font_px, Some(raw.parse::<f32>().unwrap()), "{raw}");
        }
        assert!(apply_prefs_edits("", &[(super::EDIT_LINE_HEIGHT, set("0"))]).is_ok());
    }

    #[test]
    fn float_for_integer_field_is_rejected() {
        for raw in ["1.5", "1.0000000000000001", "0.99999999999999999", "1e-400"] {
            let err = apply_prefs_edits("", &[(EDIT_SCROLLBACK, set(raw))]).unwrap_err();
            assert!(
                matches!(err, PrefsEditError::BadValue { .. }),
                "{raw}: {err:?}"
            );
        }
        for (raw, expected) in [
            ("5000.0", 5000i64),
            ("5e3", 5000),
            ("50e-1", 5),
            ("9007199254740993.0", 9_007_199_254_740_993),
            ("9223372036854775807.0", i64::MAX),
            ("-9223372036854775808.0", i64::MIN),
            ("0e999999999999999999999", 0),
        ] {
            let out = apply_prefs_edits("", &[(super::EDIT_ADJUST_BASELINE, set(raw))]).unwrap();
            let doc = out.parse::<aterm_toml::edit::DocumentMut>().unwrap();
            assert_eq!(
                doc[super::EDIT_ADJUST_BASELINE].as_integer(),
                Some(expected),
                "{raw}: {out}"
            );
        }
        for raw in [
            "9223372036854775808.0",
            "-9223372036854775809.0",
            "1e400",
            "1e+",
            ".",
        ] {
            assert!(
                apply_prefs_edits("", &[(super::EDIT_ADJUST_BASELINE, set(raw))]).is_err(),
                "{raw}"
            );
        }
    }

    fn written_spelling(toml: &str, key: &str) -> String {
        let doc = toml
            .parse::<aterm_toml::edit::DocumentMut>()
            .expect("written TOML parses");
        let mut item = doc.as_item();
        for part in key.split('.') {
            item = item
                .get(part)
                .unwrap_or_else(|| panic!("{key} is written: {toml}"));
        }
        item.as_value()
            .unwrap_or_else(|| panic!("{key} is a value: {toml}"))
            .to_string()
            .trim()
            .to_string()
    }

    #[test]
    fn a_number_is_written_as_its_kind_types_it() {
        for (key, raw, spelled) in [
            (EDIT_WINDOW_PADDING, "20", "20"),
            (EDIT_WINDOW_PADDING_TOP, "4", "4"),
            (EDIT_FONT_PX, "15", "15"),
            (EDIT_FONT_PX, "15.0", "15"),
            (EDIT_FONT_PX, "1.5e1", "15"),
            (EDIT_FONT_PX, "13.25", "13.25"),
            (EDIT_LINE_HEIGHT, "1.15", "1.15"),
            (EDIT_LINE_HEIGHT, "1.0", "1"),
            (EDIT_BACKGROUND_OPACITY, "0.85", "0.85"),
            (EDIT_FONT_WEIGHT_DARK_NUDGE, "120", "120"),
            ("sparkle_words.ink.strength", "1", "1"),
            ("sparkle_words.ink.strength", "0.35", "0.35"),
            (EDIT_SCROLLBACK, "5000", "5000"),
            (EDIT_SCROLLBACK, "5000.0", "5000"),
            (EDIT_FONT_WEIGHT, "4e2", "400"),
        ] {
            let out = apply_prefs_edits("", &[(key, set(raw))])
                .unwrap_or_else(|e| panic!("{key} = {raw}: {e}"));
            assert_eq!(written_spelling(&out, key), spelled, "{key} = {raw}: {out}");
            let config = Config::parse(&out).unwrap_or_else(|e| panic!("{out}: {e}"));
            let seed = editable_fields(&config)
                .into_iter()
                .find(|field| field.key == key)
                .and_then(|field| field.seed)
                .unwrap_or_else(|| panic!("{key} reads back: {out}"));
            assert_eq!(
                seed.parse::<f32>().ok(),
                raw.parse::<f32>().ok(),
                "{key} = {raw} reads back as written: {out}"
            );
        }

        let fields = editable_fields(&Config::default());
        let floats = fields
            .iter()
            .filter(|field| field.kind == EditKind::Float)
            .collect::<Vec<_>>();
        assert!(floats.len() >= 15, "the fractional settings are found");
        for field in floats {
            let range = range_of(field.key).unwrap_or(Range {
                min: 0.0,
                max: 100.0,
                step: 0.5,
            });
            let whole = range.max.floor();
            let fraction = (((range.min + range.max) * 50.0).round() + 1.0) / 100.0;
            assert!(fraction.fract() != 0.0, "{}: {fraction}", field.key);
            for (value, spelled) in [
                (whole, format!("{whole:.0}")),
                (fraction, fraction.to_string()),
            ] {
                let out = apply_prefs_edits("", &[(field.key, set(&value.to_string()))])
                    .unwrap_or_else(|e| panic!("{} = {value}: {e}", field.key));
                assert_eq!(
                    written_spelling(&out, field.key),
                    spelled,
                    "{}: {out}",
                    field.key
                );
                let config = Config::parse(&out).unwrap_or_else(|e| panic!("{out}: {e}"));
                let seed = editable_fields(&config)
                    .into_iter()
                    .find(|read| read.key == field.key)
                    .and_then(|read| read.seed)
                    .unwrap_or_else(|| panic!("{} reads back: {out}", field.key));
                assert_eq!(
                    seed.parse::<f32>().ok(),
                    value.to_string().parse::<f32>().ok(),
                    "{} = {value} reads back as written: {out}",
                    field.key
                );
            }
        }

        let old = "window_padding = 20.0\nfont_px = 15.0\n";
        let config = Config::parse(old).expect("an old float file reads");
        assert_eq!(
            (config.window_padding, config.font_px),
            (Some(20.0), Some(15.0))
        );
        let out = apply_prefs_edits(old, &[(EDIT_LINE_HEIGHT, set("1.2"))]).unwrap();
        assert_eq!(
            out,
            "window_padding = 20.0\nfont_px = 15.0\nline_height = 1.2\n"
        );
        let out = apply_prefs_edits(old, &[(EDIT_WINDOW_PADDING, set("24"))]).unwrap();
        assert_eq!(out, "window_padding = 24\nfont_px = 15.0\n");
    }

    #[test]
    fn a_number_setting_refuses_a_value_that_is_not_finite() {
        let fields = editable_fields(&Config::default());
        let numbers = fields
            .iter()
            .filter(|field| matches!(field.kind, EditKind::Float | EditKind::Integer))
            .collect::<Vec<_>>();
        assert!(numbers.len() >= 30, "the number settings are found");
        for field in numbers {
            for raw in ["nan", "NaN", "inf", "-inf", "infinity", "1e400", "wide"] {
                let err = apply_prefs_edits("font_px = 15.0\n", &[(field.key, set(raw))])
                    .expect_err(&format!("{} = {raw} must be refused", field.key));
                if field.key == EDIT_FONT_PX && raw.parse::<f64>().is_ok() {
                    assert!(
                        matches!(err, PrefsEditError::OutOfRange { .. }),
                        "{raw}: {err:?}"
                    );
                    continue;
                }
                assert!(
                    matches!(err, PrefsEditError::BadValue { .. }),
                    "{} = {raw}: {err:?}",
                    field.key
                );
                if field.kind == EditKind::Float {
                    assert!(
                        err.to_string().ends_with("; it must be a number"),
                        "{} = {raw}: {err}",
                        field.key
                    );
                }
            }
        }
    }

    #[test]
    fn malformed_existing_file_is_refused() {
        let err = apply_prefs_edits("this = = broken", &[(EDIT_THEME, set("Nord"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::Parse(_)));
    }

    #[test]
    fn batched_set_update_remove() {
        let existing = "font_px = 12.0\ntheme = \"Dracula\"\ngpu = true\n";
        let out = apply_prefs_edits(
            existing,
            &[
                (EDIT_FONT_PX, set("20.0")), // update
                (EDIT_THEME, None),          // remove
                (EDIT_SCROLLBACK, set("0")), // set new (0 = unlimited)
            ],
        )
        .unwrap();
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.font_px, Some(20.0));
        assert_eq!(c.theme, None);
        assert_eq!(c.scrollback_lines, Some(0));
        assert!(out.contains("gpu = true"), "unrelated key survives: {out}");
    }

    #[test]
    fn split_theme_string_round_trips() {
        let out =
            apply_prefs_edits("", &[(EDIT_THEME, set("dark:Dracula,light:GitHub Light"))]).unwrap();
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.theme.as_deref(), Some("dark:Dracula,light:GitHub Light"));
    }

    #[test]
    fn enum_value_is_validated_and_canonicalised() {
        let out = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("Phaser"))]).unwrap();
        assert!(
            out.contains("cursor_trail_style = \"phaser\""),
            "canonical lower-case spelling written: {out}"
        );
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.cursor_trail_style.as_deref(), Some("phaser"));
        let out = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("fire"))]).unwrap();
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.cursor_trail_style.as_deref(), Some("fire"));
    }

    #[test]
    fn color_value_is_validated_on_save() {
        let out = apply_prefs_edits("", &[(EDIT_FOREGROUND, set("#1E2030"))]).unwrap();
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.foreground.as_deref(), Some("#1E2030"));
        let err = apply_prefs_edits("", &[(EDIT_BACKGROUND, set("not-a-color"))]).unwrap_err();
        match err {
            PrefsEditError::BadValue { key, .. } => assert_eq!(key, EDIT_BACKGROUND),
            other => panic!("expected BadValue, got {other:?}"),
        }
    }

    #[test]
    fn the_palette_permission_seeds_on_and_the_rest_off() {
        let seed = |cfg: &Config, key: &str| {
            super::editable_fields(cfg)
                .into_iter()
                .find(|f| f.key == key)
                .and_then(|f| f.seed)
                .unwrap_or_else(|| panic!("{key} has a seeded row"))
        };
        let unset = Config::default();
        for &k in super::SECURITY_BOOL_KEYS {
            let want = if k == super::EDIT_ALLOW_PALETTE_RECONFIGURE {
                "true"
            } else {
                "false"
            };
            assert_eq!(seed(&unset, k), want, "{k}");
        }
        let off: Config = aterm_toml::from_str("allow_palette_reconfigure = false").unwrap();
        assert_eq!(seed(&off, super::EDIT_ALLOW_PALETTE_RECONFIGURE), "false");
    }

    #[test]
    fn scalar_schema_keys_classify_and_round_trip() {
        for k in super::SECURITY_BOOL_KEYS {
            assert!(
                matches!(super::edit_kind(k), super::EditKind::Bool),
                "{k} is Bool"
            );
        }
        for k in [
            super::EDIT_COLUMNS,
            super::EDIT_LINES,
            super::EDIT_TAB_STRIP_ROWS,
            super::EDIT_SEARCH_HISTORY_LINES,
        ] {
            assert!(
                matches!(super::edit_kind(k), super::EditKind::Integer),
                "{k} is Integer"
            );
        }
        let out = apply_prefs_edits(
            "",
            &[
                (super::EDIT_ALLOW_OSC52_QUERY, set("true")),
                (super::EDIT_COLUMNS, set("100")),
            ],
        )
        .unwrap();
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.allow_osc52_query, Some(true)); // TOML bool, not a string
        assert_eq!(c.columns, Some(100)); // TOML int, not a string
    }

    #[test]
    fn enum_domain_keys_classify_and_round_trip_canonical() {
        let cases: [(&str, &[&str]); 7] = [
            (EDIT_CURSOR_STYLE, super::CURSOR_STYLES),
            (super::EDIT_WINDOW_THEME, super::WINDOW_THEMES),
            (super::EDIT_PALETTE_GENERATE, super::PALETTE_GENERATE_MODES),
            (super::EDIT_BIDI, super::BIDI_MODES),
            (super::EDIT_AMBIGUOUS_WIDTH, super::AMBIGUOUS_WIDTHS),
            (super::EDIT_TRAIL_SOUND_STYLE, super::TRAIL_SOUND_STYLES),
            (
                super::EDIT_MACHINE_UNIVERSAL_CONTROL,
                super::UNIVERSAL_CONTROL_POLICIES,
            ),
        ];
        for (key, opts) in cases {
            match super::edit_kind(key) {
                super::EditKind::Enum { options } => assert_eq!(options, opts, "{key} options"),
                other => panic!("{key} should be Enum, got {other:?}"),
            }
            for &o in opts {
                let out = apply_prefs_edits("", &[(key, set(o))]).unwrap();
                let (table, leaf) = key
                    .rsplit_once('.')
                    .map_or((None, key), |(t, l)| (Some(t), l));
                assert!(
                    out.contains(&format!("{leaf} = \"{o}\"")),
                    "{key}={o} writes its canonical string: {out}"
                );
                if let Some(table) = table {
                    assert!(out.contains(&format!("[{table}]")), "{key}={o}: {out}");
                }
            }
        }
    }

    #[test]
    fn bracket_prefixed_comma_lists_keep_their_literal_members() {
        let out =
            apply_prefs_edits("", &[(super::EDIT_SHELL_ARGS, set("[working], --flag"))]).unwrap();
        let config: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(
            config.shell_args,
            Some(vec!["[working]".to_string(), "--flag".to_string()])
        );
        let out = apply_prefs_edits(
            "",
            &[(super::EDIT_SHELL_ARGS, set(r#"["one,two", "three"]"#))],
        )
        .unwrap();
        let config: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(
            config.shell_args,
            Some(vec!["one,two".to_string(), "three".to_string()])
        );
        assert!(apply_prefs_edits("", &[(super::EDIT_SHELL_ARGS, set("[1]"))]).is_err());
    }

    #[test]
    fn windows_paths_round_trip_through_scalar_and_list_rows() {
        let bold = r"C:\Users\example\AppData\Local\Microsoft\Windows\Fonts\Cascadia.ttf";
        let fallback_a = r"C:\Windows\Fonts\seguiemj.ttf";
        let fallback_b = r"\\fileserver\share\fonts\Sarasa-Mono.ttf";
        let shell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        let script = r"C:\Users\example\.config\aterm\profile.ps1";

        let out = apply_prefs_edits(
            "",
            &[
                (super::EDIT_FONT_FAMILY_BOLD, set(bold)),
                (
                    super::EDIT_FALLBACK_FONTS,
                    set(&format!("{fallback_a}, {fallback_b}")),
                ),
                (super::EDIT_SHELL, set(shell)),
                (super::EDIT_SHELL_ARGS, set(script)),
            ],
        )
        .expect("native Windows paths are writable Settings values");

        let cfg: Config = aterm_toml::from_str(&out)
            .unwrap_or_else(|error| panic!("Windows-path TOML re-parses: {error}\n{out}"));
        assert_eq!(cfg.font_family_bold.as_deref(), Some(bold));
        assert_eq!(cfg.shell.as_deref(), Some(shell));
        assert_eq!(
            cfg.fallback_fonts.map(|list| list.0),
            Some(vec![fallback_a.to_string(), fallback_b.to_string()]),
            "the comma-joined FontList keeps every entry, UNC prefix included"
        );
        assert_eq!(
            cfg.shell_args,
            Some(vec![script.to_string()]),
            "the LIST_KEYS array branch writes a survivable path too"
        );

        let again = apply_prefs_edits(&out, &[(super::EDIT_FONT_PX, set("14.0"))])
            .expect("a Windows-path config is editable again, not wedged");
        let cfg: Config = aterm_toml::from_str(&again).expect("second Save re-parses");
        assert_eq!(cfg.font_family_bold.as_deref(), Some(bold));
        assert_eq!(cfg.font_px, Some(14.0));
    }

    #[test]
    fn editable_fields_seed_from_config() {
        let c: Config = aterm_toml::from_str(
            "font_px = 13.0\ntheme = \"Nord\"\nscrollback_lines = 4000\ncopy_on_select = true\n",
        )
        .unwrap();
        let fields = editable_fields(&c);
        let keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
        let mut uniq: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for k in &keys {
            assert!(uniq.insert(k), "duplicate key {k} in editable_fields");
        }
        let mut last_section = 0usize;
        for f in &fields {
            let idx = super::section_of(f.key).order_index();
            assert!(
                idx >= last_section,
                "row {} ({:?}) breaks section grouping order",
                f.key,
                super::section_of(f.key)
            );
            last_section = idx;
        }
        for k in [
            EDIT_FOREGROUND,
            EDIT_BACKGROUND,
            EDIT_CURSOR_COLOR,
            EDIT_SELECTION_COLOR,
        ] {
            assert!(keys.contains(&k), "missing key {k}");
        }
        let seed = |k: &str| {
            fields
                .iter()
                .find(|f| f.key == k)
                .and_then(|f| f.seed.clone())
        };
        assert_eq!(seed(EDIT_FONT_PX).as_deref(), Some("13"));
        assert_eq!(seed(EDIT_THEME).as_deref(), Some("Nord"));
        assert_eq!(seed(EDIT_SCROLLBACK).as_deref(), Some("4000"));
        assert_eq!(seed(EDIT_COPY_ON_SELECT).as_deref(), Some("true"));
        assert_eq!(seed(EDIT_LIGATURES).as_deref(), Some("true"));
        assert_eq!(seed(EDIT_FONT_FAMILY), None);
        assert_eq!(seed(EDIT_CURSOR_STYLE), None);
    }

    #[test]
    fn editable_fields_default_config_is_blank() {
        let fields = editable_fields(&Config::default());
        let seed = |k: &str| {
            fields
                .iter()
                .find(|f| f.key == k)
                .and_then(|f| f.seed.clone())
        };
        assert_eq!(seed(EDIT_FONT_PX), None);
        assert_eq!(seed(EDIT_FONT_FAMILY), None);
        assert_eq!(seed(EDIT_THEME), None);
        assert_eq!(seed(EDIT_CURSOR_STYLE), None);
        assert_eq!(seed(EDIT_SCROLLBACK), None);
        assert_eq!(
            seed(EDIT_COPY_ON_SELECT).as_deref(),
            Some(if cfg!(target_os = "linux") {
                "false"
            } else {
                "true"
            })
        );
        assert_eq!(seed(EDIT_LIGATURES).as_deref(), Some("true"));
        assert_eq!(seed(super::EDIT_MACHINE_UNIVERSAL_CONTROL), None);
        let placeholder = fields
            .iter()
            .find(|f| f.key == super::EDIT_MACHINE_UNIVERSAL_CONTROL)
            .map(|f| f.placeholder.as_str());
        assert_eq!(placeholder, Some("off (default)"));
    }

    #[test]
    fn range_of_covers_bounded_numerics_only() {
        for f in editable_fields(&Config::default()) {
            match f.kind {
                EditKind::Float | EditKind::Integer => {
                    let bounded = range_of(f.key).is_some();
                    let unbounded = matches!(
                        f.key,
                        EDIT_SCROLLBACK
                            | EDIT_COLUMNS
                            | EDIT_LINES
                            | EDIT_SEARCH_HISTORY_LINES
                            | "matrix_rain.seed"
                    );
                    assert!(
                        bounded ^ unbounded,
                        "{} must be exactly one of slider (range_of) / free-form field",
                        f.key
                    );
                }
                _ => assert!(
                    range_of(f.key).is_none(),
                    "{} is non-numeric and must have no slider range",
                    f.key
                ),
            }
        }
        let r = range_of(EDIT_FONT_PX).expect("font size is a slider");
        assert!(r.min < r.max && r.step > 0.0);
        let range = range_of(super::EDIT_TAB_STRIP_ROWS).expect("tab strip range");
        assert_eq!(range.min, 0.0);
        assert_eq!(
            range.max,
            f64::from(crate::app_config::MAX_TAB_STRIP_ROWS),
            "control and shipping config clamp must agree",
        );
    }

    #[test]
    fn editable_fields_blank_string_seeds_none() {
        let c: Config = aterm_toml::from_str("theme = \"   \"\nfont_family = \"\"\n").unwrap();
        let fields = editable_fields(&c);
        let seed = |k: &str| {
            fields
                .iter()
                .find(|f| f.key == k)
                .and_then(|f| f.seed.clone())
        };
        assert_eq!(seed(EDIT_THEME), None);
        assert_eq!(seed(EDIT_FONT_FAMILY), None);
    }
    #[test]
    fn a_row_draws_its_own_note_and_neighbours_share_one_once() {
        use super::RowNote::{Own, Shared};
        let own = |key: &str| super::footnote_of(key).map(Own);
        assert_eq!(
            super::row_notes(&[super::EDIT_OPTION_AS_META, super::EDIT_PREDICTIVE_ECHO]),
            vec![None, own(super::EDIT_PREDICTIVE_ECHO)]
        );
        assert_eq!(
            super::row_notes(&[super::EDIT_MINIMUM_CONTRAST, super::EDIT_SELECTION_INACTIVE]),
            vec![own(super::EDIT_MINIMUM_CONTRAST), None],
            "Minimum contrast's note stays in its own row"
        );
        let sound = [
            super::EDIT_TRAIL_SOUNDS,
            super::EDIT_TRAIL_SOUND_VOLUME,
            super::EDIT_TONE_MELODY,
            super::EDIT_BELL_SOUND,
        ];
        assert_eq!(
            super::row_notes(&sound),
            sound.map(own).to_vec(),
            "each Sound row's note is its own"
        );
        for voice in [
            super::EDIT_TRAIL_SOUND_STYLE,
            super::EDIT_TONE_MELODY,
            super::EDIT_TRAIL_SOUND_BED,
            super::EDIT_TRAIL_SOUND_RIFF,
            super::EDIT_SPARKLE_BONK,
            super::EDIT_SPARKLE_BONK_DETONATION,
            super::EDIT_OUTPUT_STREAK_SOUND,
        ] {
            let note = super::footnote_of(voice);
            assert!(
                note.is_some_and(|note| note != super::TRAIL_SOUNDS_NOTE
                    && note != super::TRAIL_SOUND_VOLUME_NOTE),
                "{voice} says what it sounds like: {note:?}"
            );
        }
        let permissions = super::SECURITY_BOOL_KEYS;
        assert_eq!(
            super::row_notes(permissions),
            permissions.iter().map(|key| own(key)).collect::<Vec<_>>()
        );
        for key in permissions {
            assert!(super::footnote_of(key).is_some(), "{key} has a note");
            assert_eq!(super::row_notes(&[key]), vec![own(key)], "{key} alone");
        }
        let questions = [
            super::EDIT_HARNESS_ANSWER_QUESTIONS,
            super::EDIT_HARNESS_QUESTION_PRIORITY,
        ];
        assert_eq!(
            super::row_notes(&questions),
            vec![None, Some(Shared(super::HARNESS_QUESTIONS_NOTE))]
        );
        assert_eq!(
            super::row_notes(&questions[1..]),
            vec![Some(Own(super::HARNESS_QUESTIONS_NOTE))],
            "a shared note listed under one of its rows is that row's own"
        );
        assert_eq!(
            super::row_notes(&[super::EDIT_PACKAGES_ENABLED]),
            vec![own(super::EDIT_PACKAGES_ENABLED)],
            "Modified listing one ALab tools switch explains only that switch"
        );
        assert_eq!(
            super::footnote_of(EDIT_FOREGROUND),
            None,
            "the field says Theme (default)"
        );
    }

    #[test]
    fn off_keeps_the_style_it_replaced_and_a_drawn_style_takes_it_out() {
        use super::{
            EDIT_CURSOR_MOMENTUM_GLOW, EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF as BEFORE_OFF,
            RETIRED_CURSOR_TRAIL,
        };
        let read = |toml: &str, key: &str| {
            toml.parse::<aterm_toml::edit::DocumentMut>()
                .unwrap()
                .get(key)
                .map(|item| item.to_string().trim().trim_matches('"').to_string())
        };
        let write =
            |toml: &str, edits: &[(&str, Option<String>)]| apply_prefs_edits(toml, edits).unwrap();
        let off = write(
            "cursor_trail_style = \"comet\"\n",
            &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))],
        );
        assert_eq!(read(&off, EDIT_CURSOR_TRAIL_STYLE).as_deref(), Some("off"));
        assert_eq!(read(&off, BEFORE_OFF).as_deref(), Some("comet"));
        let from_default = write("", &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
        assert_eq!(
            read(&from_default, BEFORE_OFF),
            crate::app_config::DEFAULT_DECORATIVE_EFFECTS
                .then(|| super::DEFAULT_CURSOR_TRAIL_STYLE.to_string())
        );
        assert_eq!(
            read(
                &write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, set("OFF"))]),
                BEFORE_OFF
            )
            .as_deref(),
            Some("comet")
        );
        assert_eq!(
            read(&write(&off, &[(EDIT_FONT_PX, set("15"))]), BEFORE_OFF).as_deref(),
            Some("comet")
        );
        let water = write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, set("water"))]);
        assert_eq!(read(&water, BEFORE_OFF), None);
        let reset = write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, None)]);
        assert_eq!(
            read(&reset, BEFORE_OFF).is_none(),
            crate::app_config::DEFAULT_DECORATIVE_EFFECTS
        );
        let forgot = write(
            &off,
            &[(EDIT_CURSOR_TRAIL_STYLE, set("off")), (BEFORE_OFF, None)],
        );
        assert_eq!(read(&forgot, BEFORE_OFF), None);
        let held = "cursor_trail = false\ncursor_trail_style = \"fire\"\n";
        let glow = write(held, &[(EDIT_CURSOR_MOMENTUM_GLOW, set("true"))]);
        assert_eq!(read(&glow, RETIRED_CURSOR_TRAIL), None);
        assert_eq!(read(&glow, EDIT_CURSOR_TRAIL_STYLE).as_deref(), Some("off"));
        assert_eq!(read(&glow, BEFORE_OFF).as_deref(), Some("fire"));
        let chose_off = write(held, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
        assert_eq!(read(&chose_off, BEFORE_OFF).as_deref(), Some("fire"));
        let chose_water = write(held, &[(EDIT_CURSOR_TRAIL_STYLE, set("water"))]);
        assert_eq!(read(&chose_water, BEFORE_OFF), None);
        let alias = write(
            "cursor_trail_style = \"nyan\"\n",
            &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))],
        );
        assert_eq!(
            read(&alias, BEFORE_OFF).as_deref(),
            Some("rainbow kitty flying")
        );
        let unknown = write(
            "cursor_trail_style = \"cometz\"\n",
            &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))],
        );
        assert_eq!(
            read(&unknown, BEFORE_OFF).as_deref(),
            Some(super::DEFAULT_CURSOR_TRAIL_STYLE)
        );
        for kept in ["rainbow kitty flying", "nyan"] {
            assert!(
                crate::native_config_language::registered_enum_value_is_valid(
                    BEFORE_OFF,
                    kept,
                    super::CURSOR_TRAIL_STYLES
                ),
                "{kept}"
            );
        }
        let config: Config = aterm_toml::from_str(&off).unwrap();
        assert_eq!(config.cursor_trail_style_raw(), "off");
    }

    #[test]
    fn a_classic_mono_spelling_keeps_its_colour_when_rewritten() {
        use super::{
            CURSOR_TRAIL_STYLE_ALIASES, EDIT_CURSOR_MOMENTUM_GLOW,
            EDIT_CURSOR_TRAIL_COLOR as COLOR, EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF as BEFORE_OFF,
        };
        use aterm_effects::cursor_glow::GlowStyle;
        let read = |toml: &str, key: &str| {
            toml.parse::<aterm_toml::edit::DocumentMut>()
                .unwrap()
                .get(key)
                .map(|item| item.to_string().trim().trim_matches('"').to_string())
        };
        let write =
            |toml: &str, edits: &[(&str, Option<String>)]| apply_prefs_edits(toml, edits).unwrap();
        let theme_cursor = 0x0009_69DA;
        let drawn = |toml: &str| {
            let config: Config = aterm_toml::from_str(toml).unwrap();
            let presentation =
                crate::app_config::resolve_trail_presentation(config.cursor_trail_style_raw());
            let glow = crate::app_config::resolve_cursor_glow(
                config.cursor_glow_inputs(true, theme_cursor, presentation),
                presentation,
                theme_cursor,
                true,
                0x00C8_D3F5,
                0x001A_1B26,
                0.5,
            );
            (
                glow.enabled.then_some(glow.style),
                glow.classic_mono,
                config.trail_color(),
            )
        };
        let classic_in_cursor_color = (
            Some(GlowStyle::Classic),
            true,
            aterm_effects::cursor_fx::TrailColor::Cursor,
        );
        for mono in ["classic mono", "Classic Lumen", " mono classic "] {
            let file = format!("cursor_trail_style = \"{mono}\"\n");
            assert_eq!(drawn(&file).0, Some(GlowStyle::Classic));
            assert!(drawn(&file).1, "{mono:?} draws the one-colour face");
            let off = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
            assert_eq!(
                read(&off, BEFORE_OFF).as_deref(),
                Some("classic"),
                "{mono:?}"
            );
            assert_eq!(read(&off, COLOR).as_deref(), Some("cursor"), "{mono:?}");
            assert_eq!(drawn(&off).0, None, "{mono:?}");
            let back = write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, set("classic"))]);
            assert_eq!(drawn(&back), classic_in_cursor_color, "{mono:?}");
            assert_eq!(read(&back, BEFORE_OFF), None, "{mono:?}");
            for (style, glow) in [
                ("comet", GlowStyle::Comet),
                ("sparkle", GlowStyle::Sparkle),
                ("beam", GlowStyle::Beam),
                ("laser", GlowStyle::Laser),
                ("fire", GlowStyle::Fire),
            ] {
                let other = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, set(style))]);
                assert_eq!(read(&other, COLOR), None, "{mono:?} to {style}");
                assert_eq!(
                    drawn(&other),
                    (
                        Some(glow),
                        false,
                        aterm_effects::cursor_fx::TrailColor::Automatic
                    ),
                    "{mono:?} to {style}"
                );
            }
            let named = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, set("classic"))]);
            assert_eq!(drawn(&named), classic_in_cursor_color, "{mono:?}");
            let shifting = write(&named, &[(COLOR, None)]);
            assert_eq!(
                drawn(&shifting),
                (
                    Some(GlowStyle::Classic),
                    false,
                    aterm_effects::cursor_fx::TrailColor::Automatic
                ),
                "{mono:?}"
            );
            let held = format!("cursor_trail = false\n{file}");
            let glow = write(&held, &[(EDIT_CURSOR_MOMENTUM_GLOW, set("true"))]);
            assert_eq!(
                read(&glow, BEFORE_OFF).as_deref(),
                Some("classic"),
                "{mono:?}"
            );
            assert_eq!(read(&glow, COLOR).as_deref(), Some("cursor"), "{mono:?}");
            let tinted = format!("{file}cursor_trail_color = \"#50FA7B\"\n");
            let tinted_off = write(&tinted, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
            assert_eq!(read(&tinted_off, COLOR).as_deref(), Some("#50FA7B"));
            let typo = format!("{file}cursor_trail_color = \"#50FA7\"\n");
            assert_eq!(
                drawn(&typo),
                (
                    Some(GlowStyle::Classic),
                    true,
                    aterm_effects::cursor_fx::TrailColor::Automatic
                ),
                "{mono:?}: the one-colour face, in the cursor's colour"
            );
            let typo_off = write(&typo, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
            assert_eq!(
                read(&typo_off, COLOR).as_deref(),
                Some("cursor"),
                "{mono:?}"
            );
            let typo_back = write(&typo_off, &[(EDIT_CURSOR_TRAIL_STYLE, set("classic"))]);
            assert_eq!(drawn(&typo_back), classic_in_cursor_color, "{mono:?}");
            let reset = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, None), (COLOR, None)]);
            assert_eq!(read(&reset, COLOR), None, "{mono:?}");
            let other = write(&file, &[(EDIT_FONT_PX, set("15"))]);
            assert_eq!(read(&other, COLOR), None, "{mono:?}");
            assert_eq!(read(&other, EDIT_CURSOR_TRAIL_STYLE).as_deref(), Some(mono));
        }
        for &(alias, canonical) in CURSOR_TRAIL_STYLE_ALIASES {
            if GlowStyle::style_names_classic_mono(alias) {
                continue;
            }
            assert_eq!(
                crate::app_config::resolve_trail_presentation(alias),
                crate::app_config::resolve_trail_presentation(canonical),
                "{alias:?} draws what {canonical:?} draws"
            );
            let off = write(
                &format!("cursor_trail_style = \"{alias}\"\n"),
                &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))],
            );
            assert_eq!(
                read(&off, BEFORE_OFF).as_deref(),
                Some(canonical),
                "{alias:?}"
            );
            assert_eq!(read(&off, COLOR), None, "{alias:?}");
        }
        let chosen = write("", &[(COLOR, set(" CURSOR "))]);
        assert_eq!(read(&chosen, COLOR).as_deref(), Some("cursor"));
        assert!(apply_prefs_edits("", &[(EDIT_FOREGROUND, set("cursor"))]).is_err());
        assert!(
            apply_prefs_edits("", &[(super::EDIT_CURSOR_TRAIL_ACCENT, set("cursor"))]).is_err()
        );
    }
}

#[cfg(test)]
mod registry_conformance_tests {
    use super::{
        Config, EDIT_ADJUST_BASELINE, EDIT_COLUMNS, EDIT_SCROLLBACK, EditKind, NESTED_LEAVES,
        PrefsEditError, apply_prefs_edits, edit_kind, editable_fields, integer_domain, range_of,
        section_of,
    };

    fn set(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn nested_leaves_have_agreeing_rows_and_serde_round_trip() {
        let fields = editable_fields(&Config::default());
        for leaf in NESTED_LEAVES {
            let row = fields
                .iter()
                .find(|f| f.key == leaf.key)
                .unwrap_or_else(|| panic!("{} missing from editable_fields", leaf.key));
            assert_eq!(row.kind, leaf.kind, "{} row kind", leaf.key);
            assert_eq!(edit_kind(leaf.key), leaf.kind, "{} writer kind", leaf.key);
            match leaf.kind {
                EditKind::Bool => assert!(
                    row.seed.is_some(),
                    "{} is a Bool row and must seed its resolved state",
                    leaf.key
                ),
                _ => assert!(
                    !row.placeholder.is_empty(),
                    "{} must advertise its effective default in the placeholder",
                    leaf.key
                ),
            }
            let (group, order) = super::group_of(leaf.key);
            assert_ne!(group, "General", "{} fell to the catch-all group", leaf.key);
            assert!(order < u8::MAX, "{} group order unset", leaf.key);
            let _ = section_of(leaf.key); // total (never panics) — routing pinned above via group
            let sample = match leaf.kind {
                EditKind::Bool => "true",
                EditKind::Integer => "5",
                EditKind::Float => "0.5",
                EditKind::Enum { options } => options[0],
                EditKind::Color => "#123456",
                EditKind::Text if leaf.key == super::EDIT_HARNESS_LAND_AT => "18:00",
                EditKind::Text | EditKind::Theme => "sample",
            };
            let out = apply_prefs_edits("", &[(leaf.key, set(sample))])
                .unwrap_or_else(|e| panic!("{} rejected its own sample value: {e}", leaf.key));
            let _: Config = aterm_toml::from_str(&out).unwrap_or_else(|e| {
                panic!("{} wrote TOML serde rejects: {e}\n---\n{out}", leaf.key)
            });
        }
    }

    #[test]
    fn the_session_log_switches_are_bool_rows_beside_restore() {
        let read = |key: &str, cfg: &Config| match key {
            super::EDIT_SESSION_LOG_JOURNAL => cfg.session_log_journal(),
            _ => cfg.session_log_clause(),
        };
        let fields = editable_fields(&Config::default());
        for key in [
            super::EDIT_SESSION_LOG_JOURNAL,
            super::EDIT_SESSION_LOG_CLAUSE,
        ] {
            let (table, leaf) = key.split_once('.').expect("a dotted key");
            let off = apply_prefs_edits("", &[(key, set("false"))])
                .unwrap_or_else(|e| panic!("{key} = false: {e}"));
            assert!(
                off.lines().any(|l| l.trim() == format!("[{table}]")),
                "a [{table}] table: {off}"
            );
            assert!(
                off.lines().any(|l| l.trim() == format!("{leaf} = false")),
                "a TOML boolean, never a string: {off}"
            );
            let saved: Config = aterm_toml::from_str(&off)
                .unwrap_or_else(|e| panic!("{key} wrote TOML serde rejects: {e}\n---\n{off}"));
            assert!(!read(key, &saved), "{key} reads back off");
            let seeded = editable_fields(&saved);
            let row = seeded.iter().find(|f| f.key == key).expect("the row");
            assert_eq!(
                row.seed.as_deref(),
                Some("false"),
                "{key} seeds the saved value"
            );
            let on = apply_prefs_edits(&off, &[(key, set("true"))])
                .unwrap_or_else(|e| panic!("{key} = true: {e}"));
            let saved: Config = aterm_toml::from_str(&on)
                .unwrap_or_else(|e| panic!("{key} wrote TOML serde rejects: {e}\n---\n{on}"));
            assert!(read(key, &saved), "{key} reads back on: {on}");
            let row = fields
                .iter()
                .find(|f| f.key == key)
                .unwrap_or_else(|| panic!("{key} is a Settings row"));
            assert_eq!(row.kind, EditKind::Bool, "{key} row kind");
            assert_eq!(edit_kind(key), EditKind::Bool, "{key} writer kind");
            assert_eq!(row.seed.as_deref(), Some("true"), "{key} seeds its default");
            assert_eq!(
                super::group_of(key),
                super::group_of(super::EDIT_RESTORE_SESSION),
                "{key} sits beside Restore session at launch"
            );
            assert_eq!(
                section_of(key),
                section_of(super::EDIT_RESTORE_SESSION),
                "{key} section"
            );
        }
    }

    #[test]
    fn an_integer_refusal_names_the_bound_it_broke() {
        for (key, raw, words) in [
            (EDIT_COLUMNS, "70000", "must be at most 65535"),
            (EDIT_COLUMNS, "-1", "must be 0 or more"),
            (
                EDIT_COLUMNS,
                "wide",
                "must be a whole number from 0 to 65535",
            ),
            (
                EDIT_COLUMNS,
                "99999999999999999999",
                "must be at most 65535",
            ),
            (
                EDIT_SCROLLBACK,
                "9223372036854775808",
                "must be at most 9223372036854775807",
            ),
            (
                EDIT_SCROLLBACK,
                "99999999999999999999x",
                "must be a whole number, 0 or more",
            ),
            (
                EDIT_SCROLLBACK,
                "-99999999999999999999",
                "must be 0 or more",
            ),
            (
                "sparkle_words.profanity.jitter",
                "-99999999999999999999",
                "must be at least -128 px",
            ),
            (
                "sparkle_words.profanity.jitter",
                "200",
                "must be at most 127 px",
            ),
            (
                EDIT_ADJUST_BASELINE,
                "-9223372036854775809",
                "must be at least -9223372036854775808 px",
            ),
        ] {
            let error = apply_prefs_edits("", &[(key, set(raw))])
                .expect_err("an out-of-domain integer is refused")
                .to_string();
            assert!(
                error.contains(key) && error.contains(raw) && error.contains(words),
                "{key} = {raw}: {error}"
            );
        }
    }

    #[test]
    fn integer_keys_enforce_their_serde_domain() {
        let fields = editable_fields(&Config::default());
        let integer_keys: Vec<&str> = fields
            .iter()
            .filter(|f| matches!(f.kind, EditKind::Integer))
            .map(|f| f.key)
            .collect();
        assert!(
            integer_keys.contains(&"scrollback_lines")
                && integer_keys.contains(&"matrix_rain.seed"),
            "sanity: the walk sees top-level AND nested Integer keys ({integer_keys:?})"
        );
        let saves = |key: &str, value: i128| -> bool {
            match apply_prefs_edits("", &[(key, set(&value.to_string()))]) {
                Ok(out) => {
                    let _: Config = aterm_toml::from_str(&out).unwrap_or_else(|e| {
                        panic!(
                            "{key} = {value} was ACCEPTED at Save yet serde rejects the \
                             file: {e}\n---\n{out}"
                        )
                    });
                    true
                }
                Err(PrefsEditError::BadValue { .. }) => false,
                Err(e) => panic!("{key} = {value}: unexpected error class {e}"),
            }
        };
        for key in integer_keys {
            let domain = integer_domain(key);
            let (lo, hi) = (i128::from(*domain.start()), i128::from(*domain.end()));
            assert!(saves(key, lo), "{key}: domain floor {lo} must save");
            assert!(saves(key, hi), "{key}: domain ceiling {hi} must save");
            if lo > i128::from(i64::MIN) {
                assert!(!saves(key, lo - 1), "{key}: {} must be a BadValue", lo - 1);
            }
            if hi < i128::from(i64::MAX) {
                assert!(!saves(key, hi + 1), "{key}: {} must be a BadValue", hi + 1);
            }
            assert_eq!(
                saves(key, -1),
                lo < 0,
                "{key}: -1 acceptance must match the writable domain's signedness"
            );
            assert!(
                !saves(key, i128::from(u64::MAX) + 1),
                "{key}: u64::MAX + 1 must be a BadValue"
            );
        }
    }

    #[test]
    fn dotted_key_writes_create_and_edit_nested_tables_preserving_comments() {
        let out = apply_prefs_edits(
            "",
            &[
                ("update.auto_apply", set("false")),
                ("sparkle_words.profanity.enabled", set("false")),
            ],
        )
        .unwrap();
        assert!(
            out.contains("[update]"),
            "an explicit table, not a dotted key: {out}"
        );
        let c: Config = aterm_toml::from_str(&out).expect("created tables re-parse");
        assert_eq!(c.update.as_ref().and_then(|u| u.auto_apply), Some(false));
        assert_eq!(
            c.sparkle_words
                .as_ref()
                .and_then(|s| s.profanity.as_ref())
                .and_then(|p| p.enabled),
            Some(false)
        );

        let existing = "\
# my config
font_px = 12.0
[update]
# checks stay on
enabled = true  # keep me
[matrix_rain]
enabled = true
";
        let out = apply_prefs_edits(
            existing,
            &[
                ("update.auto_apply", set("false")),
                ("matrix_rain.fps", set("42")),
            ],
        )
        .unwrap();
        assert!(out.contains("# my config"), "{out}");
        assert!(out.contains("# checks stay on"), "{out}");
        assert!(out.contains("# keep me"), "{out}");
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.font_px, Some(12.0));
        let update = c.update.as_ref().unwrap();
        assert_eq!(update.enabled, Some(true));
        assert_eq!(update.auto_apply, Some(false));
        let mr = c.matrix_rain.as_ref().unwrap();
        assert_eq!(mr.enabled, Some(true));
        assert_eq!(mr.fps, Some(42));

        let out = apply_prefs_edits(
            "update = { enabled = true }\n",
            &[("update.auto_apply", set("false"))],
        )
        .unwrap();
        let c: Config = aterm_toml::from_str(&out).unwrap();
        let update = c.update.as_ref().unwrap();
        assert_eq!(update.enabled, Some(true));
        assert_eq!(update.auto_apply, Some(false));
    }

    #[test]
    fn dotted_key_removal_and_non_table_refusal() {
        let existing = "[sparkle_words]\n# note\nenabled = false\nreduced_motion = true\n";
        let out = apply_prefs_edits(existing, &[("sparkle_words.reduced_motion", None)]).unwrap();
        assert!(!out.contains("reduced_motion"), "{out}");
        assert!(out.contains("# note"), "{out}");
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(
            c.sparkle_words.as_ref().and_then(|s| s.enabled),
            Some(false)
        );
        assert_eq!(
            c.sparkle_words.as_ref().and_then(|s| s.reduced_motion),
            None
        );
        let untouched = "font_px = 12.0\n";
        assert_eq!(
            apply_prefs_edits(untouched, &[("update.auto_apply", None)]).unwrap(),
            untouched
        );
        let err =
            apply_prefs_edits("update = 5\n", &[("update.auto_apply", set("false"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::Parse(_)), "{err}");
        assert!(err.to_string().contains("not a table"), "{err}");
        assert_eq!(
            apply_prefs_edits("update = 5\n", &[("update.auto_apply", None)]).unwrap(),
            "update = 5\n"
        );
        let err = apply_prefs_edits(
            "[update.auto_apply]\nx = 1\n",
            &[("update.auto_apply", set("true"))],
        )
        .unwrap_err();
        assert!(matches!(err, PrefsEditError::Parse(_)), "{err}");
    }

    #[test]
    fn list_keys_write_toml_arrays_and_round_trip() {
        let out = apply_prefs_edits(
            "",
            &[
                (super::EDIT_PALETTE, set("#1d1f21, #cc6666")),
                (super::EDIT_SHELL_ARGS, set("-l, -i")),
                (super::EDIT_FONT_FEATURES, set("ss01, zero, -calt")),
            ],
        )
        .unwrap();
        let c: Config = aterm_toml::from_str(&out).expect("arrays re-parse as Vec<String>");
        assert_eq!(
            c.palette,
            Some(vec!["#1d1f21".to_string(), "#cc6666".to_string()])
        );
        assert_eq!(c.shell_args, Some(vec!["-l".to_string(), "-i".to_string()]));
        assert_eq!(
            c.font_features,
            Some(vec![
                "ss01".to_string(),
                "zero".to_string(),
                "-calt".to_string()
            ])
        );
        let fields = editable_fields(&c);
        let seed = |k: &str| {
            fields
                .iter()
                .find(|f| f.key == k)
                .and_then(|f| f.seed.clone())
        };
        assert_eq!(
            seed(super::EDIT_PALETTE).as_deref(),
            Some("#1d1f21, #cc6666")
        );
        assert_eq!(seed(super::EDIT_SHELL_ARGS).as_deref(), Some("-l, -i"));
        let err =
            apply_prefs_edits("", &[(super::EDIT_PALETTE, set("#123456, nope"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
        let err = apply_prefs_edits("", &[(super::EDIT_SHELL_ARGS, set(", ,"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
    }

    #[test]
    fn full_coverage_scalar_keys_classify_and_round_trip() {
        use crate::app_config::{BackgroundMaterial, WindowColorspace};
        for k in [
            super::EDIT_GPU,
            super::EDIT_CURSOR_TRAIL_RING,
            super::EDIT_CURSOR_TRAIL_BLOOM,
            super::EDIT_CURSOR_FIRE_SHIMMER,
            super::EDIT_HDR_GLOW,
            super::EDIT_RESTORE_SESSION,
            super::EDIT_TEMPORAL_RECORDING,
            super::EDIT_TRAIL_SOUND_BED,
        ] {
            assert!(
                matches!(edit_kind(k), EditKind::Bool),
                "{k} is Bool (else Save writes a string for a serde bool)"
            );
        }
        for o in super::WINDOW_COLORSPACES {
            assert!(WindowColorspace::parse(o).is_some(), "colorspace {o}");
        }
        for o in super::BACKGROUND_MATERIALS {
            assert!(BackgroundMaterial::parse(o).is_some(), "material {o}");
        }
        let out = apply_prefs_edits(
            "",
            &[
                (super::EDIT_GPU, set("false")),
                (super::EDIT_TRAIL_SOUND_BED, set("true")),
                (super::EDIT_TRAIL_SOUND_VOLUME, set("0.2")),
                (super::EDIT_CURSOR_TRAIL_LENGTH, set("48")),
                (super::EDIT_WINDOW_COLORSPACE, set("Display-P3")),
                (super::EDIT_BACKGROUND_MATERIAL, set("under-window")),
                (super::EDIT_CURSOR_TRAIL_COLOR, set("#50FA7B")),
                (super::EDIT_SHELL, set("bash")),
                (super::EDIT_WINDOW_PADDING, set("20")),
                (super::EDIT_WINDOW_PADDING_TOP, set("6")),
            ],
        )
        .unwrap();
        let c: Config = aterm_toml::from_str(&out).expect("typed values round-trip");
        assert_eq!(c.gpu, Some(false));
        assert_eq!(c.trail_sound_bed, Some(true));
        assert!(c.trail_sound_bed_or_default());
        assert_eq!(c.trail_sound_volume, Some(0.2));
        assert_eq!(c.cursor_trail_length, Some(48));
        assert_eq!(c.window_colorspace.as_deref(), Some("display-p3"));
        assert_eq!(c.background_material.as_deref(), Some("under-window"));
        assert_eq!(c.cursor_trail_color.as_deref(), Some("#50FA7B"));
        assert_eq!(c.shell.as_deref(), Some("bash"));
        assert_eq!(c.window_padding, Some(20.0));
        assert_eq!(c.window_padding_top, Some(6.0));
        assert_eq!(c.window_padding_or_default(), 20.0);
        assert_eq!(c.window_padding_top_or_default(), 6.0);
        let err =
            apply_prefs_edits("", &[(super::EDIT_WINDOW_COLORSPACE, set("rec2020"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
        let err =
            apply_prefs_edits("", &[(super::EDIT_CURSOR_TRAIL_ACCENT, set("teal"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
        assert!(range_of(super::EDIT_TRAIL_SOUND_VOLUME).is_some());
        assert!(range_of(super::EDIT_WINDOW_PADDING).is_some());
        assert!(range_of(super::EDIT_WINDOW_PADDING_TOP).is_some());
    }

    #[test]
    fn the_predictive_editing_switch_is_a_bool_row_that_round_trips() {
        let key = super::EDIT_PREDICTIVE_ECHO_EDITING;
        assert!(matches!(edit_kind(key), EditKind::Bool));
        assert_eq!(section_of(key), section_of(super::EDIT_PREDICTIVE_ECHO));
        let fields = editable_fields(&Config::default());
        let row = fields
            .iter()
            .find(|f| f.key == key)
            .expect("a settings row");
        assert_eq!(row.seed.as_deref(), Some("true"), "on by default");
        let out = apply_prefs_edits("", &[(key, set("false"))]).expect("a Bool writes");
        let c: Config = aterm_toml::from_str(&out).expect("a TOML bool round-trips");
        assert_eq!(c.predictive_echo_editing, Some(false));
        assert!(!c.predictive_echo_editing_or_default());
        assert!(Config::default().predictive_echo_editing_or_default());
    }

    #[test]
    fn chrome_color_rows_are_wired() {
        let fields = editable_fields(&Config::default());
        let at = |key: &str| {
            fields
                .iter()
                .position(|f| f.key == key)
                .unwrap_or_else(|| panic!("{key} row"))
        };
        for key in [
            super::EDIT_ACCENT_COLOR,
            super::EDIT_BORDER_COLOR,
            super::EDIT_SEARCH_MATCH_COLOR,
        ] {
            assert_eq!(
                super::group_of(key),
                super::group_of(super::EDIT_ACTIVE_TAB_COLOR),
                "{key} sits beside Selected tab color"
            );
            assert!(matches!(fields[at(key)].kind, EditKind::Color), "{key}");
            let written = apply_prefs_edits("", &[(key, Some("#336699".to_string()))]).unwrap();
            let c: Config = aterm_toml::from_str(&written).unwrap();
            let resolved = c.resolved_colors(
                aterm_types::Appearance::Dark,
                &crate::app_config::ThemeCatalog::default(),
                None,
            );
            assert_eq!(resolved.config_keys(), vec![key.to_string()], "{key}");
            let reset = apply_prefs_edits(&written, &[(key, None)]).unwrap();
            let c: Config = aterm_toml::from_str(&reset).unwrap();
            assert!(
                c.resolved_colors(
                    aterm_types::Appearance::Dark,
                    &crate::app_config::ThemeCatalog::default(),
                    None,
                )
                .config_keys()
                .is_empty()
            );
            assert!(
                matches!(
                    apply_prefs_edits("", &[(key, Some("teal".to_string()))]).unwrap_err(),
                    PrefsEditError::BadValue { .. }
                ),
                "{key}: not a colour"
            );
        }
    }

    #[test]
    fn display_font_and_tab_color_registry_rows_are_wired() {
        let off = Config {
            display_font: Some("off".to_string()),
            font_family: Some("Menlo".to_string()),
            ..Config::default()
        };
        assert_eq!(off.font_family_request().as_deref(), Some("Menlo"));
        assert!(off.display_faces_request().is_empty());
        let set = |v: &str| Some(v.to_string());
        let out = apply_prefs_edits(
            "",
            &[
                (super::EDIT_DISPLAY_FONT, set("pixel")),
                (super::EDIT_ACTIVE_TAB_COLOR, set("#FF00AA")),
            ],
        )
        .unwrap();
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.display_font.as_deref(), Some("pixel"));
        assert_eq!(c.active_tab_color.as_deref(), Some("#FF00AA"));
        assert_eq!(
            c.resolved_colors(
                aterm_types::Appearance::Dark,
                &crate::app_config::ThemeCatalog::default(),
                None
            )
            .authored_chrome()
            .active_tab,
            Some([0xFF, 0x00, 0xAA])
        );
        assert_eq!(c.font_family_request(), None);
        assert_eq!(c.display_faces_request(), ["pixel"]);
        let out = apply_prefs_edits("", &[(super::EDIT_DISPLAY_FONT, set(" pixel + engraved "))])
            .unwrap();
        let mixed: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(mixed.display_font.as_deref(), Some("pixel+engraved"));
        assert_eq!(mixed.display_faces_request(), ["pixel", "engraved"]);
        let reversed = Config {
            display_font: Some("engraved+pixel".to_string()),
            font_family: Some("Menlo".to_string()),
            ..Config::default()
        };
        assert_eq!(
            reversed.display_faces_request(),
            ["pixel", "engraved"],
            "the toggle order changes nothing"
        );
        assert_eq!(reversed.font_family_request().as_deref(), Some("Menlo"));
        for bad in [
            "doom",
            "pixel+doom",
            "engraved+engraved",
            "chunky+pixel+engraved+bubble",
        ] {
            assert!(
                matches!(
                    apply_prefs_edits("", &[(super::EDIT_DISPLAY_FONT, set(bad))]).unwrap_err(),
                    PrefsEditError::BadValue { .. }
                ),
                "{bad} must be rejected"
            );
        }
        assert!(matches!(
            apply_prefs_edits("", &[(super::EDIT_ACTIVE_TAB_COLOR, set("pink"))]).unwrap_err(),
            PrefsEditError::BadValue { .. }
        ));
        let mut cleared = c.clone();
        cleared.display_font = None;
        assert_eq!(cleared.font_family_request(), cleared.font_family);
        assert!(cleared.display_faces_request().is_empty());
        let mut typo = c;
        typo.display_font = Some("dooom".to_string());
        assert_eq!(typo.font_family_request(), typo.font_family);
        assert!(typo.display_faces_request().is_empty());
    }

    #[test]
    fn the_trail_color_note_follows_the_colour_law() {
        use aterm_effects::cursor_fx::{
            glow_color_follows_live_cursor, own_trail_color, trail_color_default_name,
        };
        use aterm_effects::cursor_glow::GlowStyle;
        let old = "Blank: Lightning keeps its violet, and Beam, Comet and Sparkle keep their own \
                   color until you set a cursor color; other styles follow the cursor color. \
                   Cursor color (or \u{201c}cursor\u{201d}) makes every style follow it.";
        let mut checked = 0;
        for row in aterm_effects::cursor_glow::spellings::TRAIL_STYLES {
            for block in [true, false] {
                for pinned in [false, true] {
                    let note = super::trail_color_note(row.name, block, pinned);
                    let case = format!("{} block={block} pinned={pinned}: {note}", row.name);
                    let Some(style) = row.style else {
                        assert!(note.contains("Off"), "{case}");
                        continue;
                    };
                    assert_ne!(note, old, "{case}");
                    if !style.draws_trail_color() {
                        assert_eq!(note.contains("does not use these"), block, "{case}");
                        assert!(!note.contains("follows the cursor color"), "{case}");
                    } else if style == GlowStyle::Classic {
                        assert_eq!(
                            trail_color_default_name(style, pinned),
                            Some("Shifting colors")
                        );
                        assert!(note.contains("shifts through colors"), "{case}");
                    } else if let Some((_, own)) = own_trail_color(style, pinned) {
                        assert!(note.contains(&own.to_lowercase()), "{case}");
                        assert_eq!(
                            note.contains("until you set a cursor color"),
                            glow_color_follows_live_cursor(style, true),
                            "{case}"
                        );
                    } else {
                        assert!(note.contains("follows the cursor color"), "{case}");
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 40, "{checked}");
        assert!(super::trail_color_note("classic mono", true, false).contains("cursor color"));
    }
}
