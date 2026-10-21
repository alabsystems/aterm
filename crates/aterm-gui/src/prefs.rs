// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The settings registry and non-destructive `aterm.toml` writer shared by the
//! native Settings tab and the Manual editor (App ▸ Settings…, ⌘,).
//!
//!   * [`editable_fields`]: the config-control registry (label, key, kind, seed,
//!     placeholder, grouped by [`Section`]) the Settings tab and the Manual schema
//!     select from;
//!   * [`apply_prefs_edits`] / [`save_prefs_snapshot_observed`]: write edited
//!     values back non-destructively (other keys, comments and formatting kept,
//!     via `aterm-toml`; atomic temp-write and rename), returning the committed
//!     bytes and post-publication proof for direct admission.
//!
//! Clearing a field removes its key (back to the built-in default) rather than
//! writing an empty string. Save is best-effort: a missing file is created, an
//! unwritable one is logged, never a panic.

use crate::app_config::{
    Config, DEFAULT_TAB_STATUS_DWELL_MS, DEFAULT_TAB_STATUS_QUIET_AFTER_MS,
    MAX_TAB_STATUS_DWELL_MS, MAX_TAB_STATUS_QUIET_AFTER_MS, MIN_TAB_STATUS_DWELL_MS,
    MIN_TAB_STATUS_QUIET_AFTER_MS,
};
use aterm_effects::cursor_glow::{GlowStyle, spellings};
use aterm_effects::trail_sound::SoundVoice;
use aterm_phase::Program;

/// The TOML keys the settings model edits, each typed on write by
/// [`apply_prefs_edits`]. Each is the exact `Config` field name, so a Save and
/// reload round-trips through serde.
pub(crate) const EDIT_FONT_PX: &str = "font_px";
pub(crate) const EDIT_FONT_FAMILY: &str = "font_family";
/// The bundled display-face selector (`display_font`): up to
/// `aterm_render::DISPLAY_FACE_MIX_MAX` `+`-joined [`DISPLAY_FONT_OPTIONS`] ids,
/// cleared = the normal font selection. Driven by the Text & Fonts page's
/// Display Faces card.
pub(crate) const EDIT_DISPLAY_FONT: &str = "display_font";

// W6 per-style fonts + TOML fallback chain.
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
/// The RETIRED cursor-effects master (`Config::cursor_trail`): a hidden third key
/// over the two visible controls it gated. Folded into its successors —
/// `false` reads as `cursor_trail_style = "off"` and `cursor_momentum_glow =
/// false`, `true` as their defaults where unset — by the resolvers
/// (`Config::cursor_trail_style_raw`, `Config::cursor_momentum_glow_or_default`);
/// dropped from the file whenever Settings writes either successor, the other
/// pinned to what the file drew ([`apply_prefs_edits`]); named as retired by the
/// config language service and Settings ▸ Modified ([`RETIRED_SPELLINGS`]).
pub(crate) const RETIRED_CURSOR_TRAIL: &str = "cursor_trail";
pub(crate) const EDIT_CURSOR_TRAIL_STYLE: &str = "cursor_trail_style";
/// THE STYLE OFF REPLACED: Off is a value of the one style key, so an edit
/// that turns a drawn style Off would overwrite the person's style — their
/// own Off, and Serious Mode's exit, which holds the cursor Off while another
/// effect turns on ("other effects stay off"). The writer keeps the style it
/// replaced here ([`apply_prefs_edits`]), Cursor Styles marks it, and an edit
/// that makes the style draw again takes it out. It never draws: the style
/// key alone decides what the cursor draws.
pub(crate) const EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF: &str = "cursor_trail_style_before_off";
pub(crate) const EDIT_TRAIL_SOUNDS: &str = "trail_sounds";
/// Tone-melody toggle (`Config::tone_melody`, default ON): the trail-sound
/// melody leans with the typed line's inferred mood (the on-device
/// `aterm_effects::tone` classifier); OFF keeps the neutral melody and stops the
/// classifier.
pub(crate) const EDIT_TONE_MELODY: &str = "tone_melody";
/// Ambient-bed toggle (`Config::trail_sound_bed`, default ON since the
/// owner's 2026-09-09 ruling): ON plays the continuous per-style background
/// texture behind the trail notes; OFF gates the synth's bed mixer entirely
/// (zero bed samples — the notes, brrrring, bonk and melody are untouched).
pub(crate) const EDIT_TRAIL_SOUND_BED: &str = "trail_sound_bed";
/// Typing-sound picker (`Config::trail_sound_style`, default `auto`): `auto`
/// follows the visual trail style's signature palette; every other value
/// ([`TRAIL_SOUND_STYLES`]) names an instrument — one of the nine palettes by
/// what it sounds like (`music box`, `droplet`, …), the `mechanical`
/// keyboard, or a sound-only voice (`typewriter`, `marimba`, `felt`) —
/// spoken by every keystroke whatever the trail looks like.
pub(crate) const EDIT_TRAIL_SOUND_STYLE: &str = "trail_sound_style";
/// The held-key sing-along riff's own switch (`Config::trail_sound_riff`,
/// default ON). The riff is the loudest voice the synth emits; OFF schedules no
/// riff bar and leaves every other voice at its level. A sound gate only: the
/// sing-along's visuals stay. Subordinate to `trail_sounds`, `trail_sound_volume`
/// and window focus.
pub(crate) const EDIT_TRAIL_SOUND_RIFF: &str = "trail_sound_riff";
/// Audible terminal bell (`Config::bell_sound`, default ON): the BEL beep
/// (`NSBeep` on macOS, `MessageBeep` on Windows), an OS alert sound outside
/// `trail_sound_volume`. OFF suppresses only the beep; the visual bell flash and
/// the attention request stay. Inert on other platforms (no beep call), as
/// [`super::native_settings`]' Advanced gate and [`crate::diagnostics`] say.
pub(crate) const EDIT_BELL_SOUND: &str = "bell_sound";
/// The curse BONK's two gates, named so the Sound menu, the section router and
/// the visibility allowlist all spell them once. They are `[sparkle_words]`
/// leaves in the FILE (that is where the feature's table lives and the spelling
/// must never change), but they are SFX in the UI.
pub(crate) const EDIT_SPARKLE_BONK: &str = "sparkle_words.profanity.bonk";
pub(crate) const EDIT_SPARKLE_BONK_DETONATION: &str = "sparkle_words.profanity.bonk_detonation";
/// PRISM WAKE's pip (`[output_streak] sound`, default ON;
/// `docs/DESIGN-output-streak-2026-08-30.md` §7): the output streak's
/// once-per-episode pip and closing exhale. The ear's own opt-out; the comet
/// keeps flying. An `[output_streak]` leaf in the file but SFX in the UI: it
/// routes to the Sound box through [`SOUND_MENU_KEYS`], which [`section_of`] and
/// [`group_of`] consult ahead of their prefix rules. Only ever subtractive: the
/// sound master, volume, focus and serious mode apply first, and a pip sounds
/// only on an edge that also minted light, so with `output_streak.enabled` off
/// it is silent (`Config::output_streak_warning` says so).
pub(crate) const EDIT_OUTPUT_STREAK_SOUND: &str = "output_streak.sound";
/// PRISM WAKE's switch (`[output_streak] enabled`): the comet over new program
/// output. Its absent-key default is platform-split
/// ([`crate::app_config::DEFAULT_DECORATIVE_EFFECTS`]), and Settings seeds the
/// row from the resolver that owns it. Appearance's Effects box, beside Matrix
/// rain; the pip's row names it while it is Off.
pub(crate) const EDIT_OUTPUT_STREAK_ENABLED: &str = "output_streak.enabled";

/// PRESENCE: the `[presence]` leaf the View menu's needs switch writes — Show
/// Needs in Windows, the needs border and its pill (owner surfaces §5).
/// Default ON (`Config::presence_rim_enabled`); the View menu flips the live
/// bit at the click and persists it through the one config lane by this key
/// (`App::queue_presence_write`). `[presence] band` is retired with the band
/// row (`native_config_language`'s retired-key table says so once).
pub(crate) const EDIT_PRESENCE_RIM: &str = "presence.rim";

/// Every config key that changes what aterm sounds like, and nothing else. One
/// list drives [`section_of`] (the Sound page), [`group_of`] (the synth voices in
/// one box, the bell in its own) and the native Advanced audit, so a new audible
/// knob cannot reach one and miss the others.
///
/// Deliberately absent:
///   * `serious_mode`: it silences every row here (`SeriousEffect::TerminalSound`),
///     but it is a whole-product policy with escape semantics (turning a playful
///     control on clears it) and keeps its own "Effect policy" home; each audible
///     row discloses it instead (pinned by
///     `serious_mode_is_disclosed_on_the_sound_rows_it_silences`).
///   * `matrix_rain.bell_alert`: a visual bell; only [`EDIT_BELL_SOUND`] is
///     audible.
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
/// The tuning a cursor style can read. The Cursor Styles page shows the ones
/// the selected style uses (`native_settings::trail_style_uses`), HDR glow and
/// SDR glow boost among them: one of the two sets every trail's brightness,
/// as the display allows, and each row says when the other is in use.
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
/// `cursor_trail_color`'s one word beside a colour: the cursor's colour,
/// chosen as the trail colour ([`aterm_effects::cursor_fx::TrailColor::Cursor`]).
/// A trail colour like any other — Classic draws its one-colour face, Laser
/// drops its violet, a bar's rod wears it — whose value follows the live
/// cursor colour. It is what the old `classic mono` style spellings say beside
/// Classic, so the writer can say it too when it rewrites one
/// ([`apply_prefs_edits`]).
pub(crate) const TRAIL_COLOR_CURSOR: &str = "cursor";

/// [`TRAIL_COLOR_CURSOR`] as a Settings row names it.
pub(crate) const TRAIL_COLOR_CURSOR_LABEL: &str = "Cursor color";

/// What the trail colour key `key` (the colour or its accent) draws while
/// unset, in a row's words, under the cursor style aterm.toml spells
/// `style_raw`, the trail colour setting `color` and whether a cursor colour
/// is set (`cursor_color_pinned`, the colour law's pin): the colour, a style's
/// own (Laser's "Violet", Comet's "Pale blue" with no cursor colour set),
/// Classic's "Shifting colors" (an old `classic mono` spelling's cursor
/// colour), else the cursor's; the accent a brighter one of
/// what a window brightens ([`aterm_effects::cursor_fx::unset_accent_base`]:
/// the trail colour chosen, where the style draws it). The row's placeholder
/// and Modified's "Default:" both say it, so Reset is named by what it gives
/// back.
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
    // The old `classic mono` spelling draws Classic in the cursor's colour.
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

/// Whether `raw` is a value colour key `key` takes: a colour the runtime
/// parses (`RRGGBB`, `#RRGGBB`, `#RGB`), or, for the trail colour, the word
/// [`TRAIL_COLOR_CURSOR`]. The writer and the config check both ask it.
pub(crate) fn color_value_is_valid(key: &str, raw: &str) -> bool {
    let raw = raw.trim();
    aterm_types::scheme::parse_hex(raw).is_some()
        || (key == EDIT_CURSOR_TRAIL_COLOR && raw.eq_ignore_ascii_case(TRAIL_COLOR_CURSOR))
}

/// What a `cursor_trail_color` value says: unset, blank or not a colour is
/// each style's automatic colour (the config check flags the last), the word
/// [`TRAIL_COLOR_CURSOR`] (any case) the cursor's colour chosen, and a hex
/// colour that colour.
pub(crate) fn trail_color_setting(raw: Option<&str>) -> aterm_effects::cursor_fx::TrailColor {
    use aterm_effects::cursor_fx::TrailColor;
    let Some(raw) = raw.map(str::trim) else {
        return TrailColor::Automatic;
    };
    if raw.eq_ignore_ascii_case(TRAIL_COLOR_CURSOR) {
        return TrailColor::Cursor;
    }
    aterm_types::scheme::parse_hex(raw).map_or(TrailColor::Automatic, |color| {
        TrailColor::Pinned(crate::app_config::rgb_u32(color))
    })
}
/// The rainbow kitty's user sprite. KEEPS the `nyan` spelling — unlike the
/// trail-style VALUE (which has an alias table), a config KEY has no aliasing
/// seam, so renaming it would silently orphan every `cursor_nyan_sprite` line
/// already in a user's `config.toml`. The constant is named after its key so
/// the two can never drift.
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
// Typography and appearance.
pub(crate) const EDIT_LINE_HEIGHT: &str = "line_height";
/// `font_size_scale`: multiplies the resolved font size without fixing it.
pub(crate) const EDIT_FONT_SIZE_SCALE: &str = "font_size_scale";
pub(crate) const EDIT_ADJUST_BASELINE: &str = "adjust_baseline";
pub(crate) const EDIT_MINIMUM_CONTRAST: &str = "minimum_contrast";
pub(crate) const EDIT_SELECTION_INACTIVE: &str = "selection_inactive";
/// Whether a split inks a hairline along the edges of the pane that takes
/// keystrokes (`Config::split_focus_mark`), a Bool, default ON.
pub(crate) const EDIT_SPLIT_FOCUS_MARK: &str = "split_focus_mark";
pub(crate) const EDIT_CURSOR_BREAK_LIGATURES: &str = "cursor_break_ligatures";
/// Admit Cascadia N:1 merged ligatures (`Config::merged_ligatures`), a Bool,
/// default OFF.
pub(crate) const EDIT_MERGED_LIGATURES: &str = "merged_ligatures";
pub(crate) const EDIT_BOLD_IS_BRIGHT: &str = "bold_is_bright";
/// Where slots 16-255 come from (`Config::palette_generate`).
pub(crate) const EDIT_PALETTE_GENERATE: &str = "palette_generate";
pub(crate) const EDIT_FAINT_OPACITY: &str = "faint_opacity";
// Font-metric decoration.
pub(crate) const EDIT_ADJUST_UNDERLINE_POSITION: &str = "adjust_underline_position";
pub(crate) const EDIT_ADJUST_UNDERLINE_THICKNESS: &str = "adjust_underline_thickness";
pub(crate) const EDIT_UNDERLINE_SKIP_DESCENDERS: &str = "underline_skip_descenders";
// Glyph appearance and variable-font instantiation.
pub(crate) const EDIT_TEXT_BLENDING: &str = "text_blending";
pub(crate) const EDIT_FONT_THICKEN: &str = "font_thicken";
pub(crate) const EDIT_STEM_GAMMA: &str = "stem_gamma";
pub(crate) const EDIT_FONT_VARIATION: &str = "font_variation";
pub(crate) const EDIT_FONT_WEIGHT: &str = "font_weight";
// Motion / Reduce Motion policy.
pub(crate) const EDIT_MOTION: &str = "motion";
/// Process-wide serious-mode overlay. A Bool, default OFF. It suppresses
/// nonessential sound and decoration without rewriting any individual effect
/// preference, so clearing it restores those settings exactly.
pub(crate) const EDIT_SERIOUS_MODE: &str = "serious_mode";
/// Load-adaptive effect shedding, a Bool, default ON; OFF opts out of the
/// render-overload heuristic so animations follow `motion` / the OS flag alone.
pub(crate) const EDIT_LOAD_ADAPTIVE_MOTION: &str = "load_adaptive_motion";

/// The hex-colour keys (theme colours plus the cursor-trail overrides), edited as
/// `RRGGBB`/`#RRGGBB` strings ([`EditKind::Color`]). Listed once so `edit_kind`
/// and the schema rows agree on which keys are colours.
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
/// The selected-tab color override (`active_tab_color`, `#RRGGBB`). Unset follows
/// the system's selected-tab pill, which Settings shows as the field's default label
/// (this key's `EditField` placeholder).
pub(crate) const EDIT_ACTIVE_TAB_COLOR: &str = "active_tab_color";
/// The chrome accent (`accent_color`, `#RRGGBB`), cleared = the theme's, else
/// derived from its cursor or ANSI blue/cyan.
pub(crate) const EDIT_ACCENT_COLOR: &str = "accent_color";
/// Split dividers and chrome seams (`border_color`), cleared = the theme's,
/// else the background stepped toward the foreground.
pub(crate) const EDIT_BORDER_COLOR: &str = "border_color";
/// Find's highlight on the other matches (`search_match_color`), cleared = the
/// theme's, else a tint of the background toward the accent.
pub(crate) const EDIT_SEARCH_MATCH_COLOR: &str = "search_match_color";

/// The `display_font` Enum options: `"off"` (the default font — what clearing
/// the key also means) followed by the ids of [`aterm_render::DISPLAY_FACES`]
/// in its order, built from that table at compile time so the registry, the
/// resolver, and the toggles can never disagree about the set. `"off"` exists
/// so the plain popup control (Compact / search results) can turn the display
/// face off without a separate reset affordance — `font_family_request` treats
/// it exactly like an unset key.
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

/// Whether one `display_font` id names a current registry face. `"off"` is
/// deliberately NOT accepted here — this answers "does this name a face", and
/// an off-mix is just a cleared key.
#[must_use]
pub(crate) fn display_font_id_is_accepted(id: &str) -> bool {
    aterm_render::display_face_canonical_id(id).is_some()
}
/// Tab Subject & Status. `EDIT_TAB_STATUS` is the master switch; the other three
/// are inert while it is off, which [`crate::native_settings`] discloses.
pub(crate) const EDIT_TAB_STATUS: &str = "tab_status";
pub(crate) const EDIT_TAB_STATUS_QUIET_AFTER_MS: &str = "tab_status_quiet_after_ms";
pub(crate) const EDIT_TAB_STATUS_DWELL_MS: &str = "tab_status_dwell_ms";
pub(crate) const EDIT_TAB_STATUS_BADGE: &str = "tab_status_badge";
/// The tab connection mark's opt-out (Session Connections). Independent of the
/// status classifier quartet above: it gates only the fourth tab mark, never
/// the connections themselves.
pub(crate) const EDIT_TAB_CONNECTION_BADGE: &str = "tab_connection_badge";
/// Whether aterm posts its OWN notices as desktop notifications
/// (`Config::desktop_alerts`, default OFF — owner, 2026-09-28, kept by the
/// lead on 2026-10-18; needs only once on, and the menu bar's Notify Me When
/// I'm Needed turns it on): the herald's
/// escalations, the consent path's and the update-health banner. An ordinary
/// Settings ▸ Security row, in its own "Notifications" box beside the
/// Permissions box that holds `allow_notifications` (the PROGRAMS'
/// notifications) — the two notification switches on one page.
pub(crate) const EDIT_DESKTOP_ALERTS: &str = "desktop_alerts";
pub(crate) const EDIT_SEARCH_HISTORY_LINES: &str = "search_history_lines";
pub(crate) const EDIT_ALLOW_OSC52_QUERY: &str = "allow_osc52_query";
/// macOS Secure Keyboard Entry (`Config::secure_keyboard_entry`, default OFF):
/// the OS keystroke-snooping guard, actuated by `crate::secure_input`.
pub(crate) const EDIT_SECURE_KEYBOARD_ENTRY: &str = "secure_keyboard_entry";
pub(crate) const EDIT_ALLOW_WINDOW_OPS: &str = "allow_window_ops";
pub(crate) const EDIT_ALLOW_NOTIFICATIONS: &str = "allow_notifications";
pub(crate) const EDIT_ALLOW_PALETTE_RECONFIGURE: &str = "allow_palette_reconfigure";
pub(crate) const EDIT_ALLOW_KITTY_FILE_TRANSFER: &str = "allow_kitty_file_transfer";
/// The subtle top-right build/version badge switch (`Config::show_build_badge`) — one
/// Bool, default OFF. See [`crate::build_badge`].
pub(crate) const EDIT_SHOW_BUILD_BADGE: &str = "show_build_badge";

/// The PHOSPHOR matrix-rain master switch, `[matrix_rain] enabled`. Its Settings
/// row is a [`NESTED_LEAVES`] entry (Bool, default OFF); this constant serves
/// code that addresses the key directly. Per-session overrides (View ▸ Matrix
/// Rain, `aterm ctl rain`) win over it until the session ends.
pub(crate) const EDIT_MATRIX_RAIN_ENABLED: &str = "matrix_rain.enabled";

/// The `[packages]` toolchain-manager switches, as dotted keys (the nested writer
/// [`EDIT_MATRIX_RAIN_ENABLED`] uses). Both Bools, default ON: `enabled` is "Keep
/// ALab tools up to date" and [`EDIT_PACKAGES_AUTO_INSTALL`] the install consent;
/// each absorbed a retired spelling, which writing it drops from the file
/// ([`apply_prefs_edits`]). The rows render on the special Settings ▸ Packages
/// page; Search and Modified find them through the registry.
pub(crate) const EDIT_PACKAGES_ENABLED: &str = "packages.enabled";
/// `[packages] auto_install` — may atpkg install ALab toolset members nobody named (the
/// first-run fill, new members of the signed set)? Default ON, batteries included; the
/// explicit opt-outs (`uninstall`, `exclude`, `enabled = false`) always win.
pub(crate) const EDIT_PACKAGES_AUTO_INSTALL: &str = "packages.auto_install";
/// The retired `[packages]` spellings and the key that replaced each: still honoured
/// by the resolvers, dropped from the file whenever Settings writes the new key, and
/// named as retired by the config language service and Settings ▸ Modified — where each
/// is resettable, and Reset All removes it with the rest.
pub(crate) const RETIRED_PACKAGES_SPELLINGS: &[(&str, &str)] = &[
    (RETIRED_PACKAGES_AUTO_UPDATE, EDIT_PACKAGES_ENABLED),
    (RETIRED_PACKAGES_SEED_INSTALL, EDIT_PACKAGES_AUTO_INSTALL),
];
/// Every retired key Settings ▸ Modified names as retired, with the setting it
/// folded into: the `[packages]` spellings ([`RETIRED_PACKAGES_SPELLINGS`]) and
/// the cursor-effects master ([`RETIRED_CURSOR_TRAIL`], read as the cursor style
/// and the typing glow). Each is resettable, and Reset All removes it with the
/// rest.
pub(crate) const RETIRED_SPELLINGS: &[(&str, &str)] = &[
    (RETIRED_PACKAGES_AUTO_UPDATE, EDIT_PACKAGES_ENABLED),
    (RETIRED_PACKAGES_SEED_INSTALL, EDIT_PACKAGES_AUTO_INSTALL),
    (RETIRED_CURSOR_TRAIL, EDIT_CURSOR_TRAIL_STYLE),
];
/// The retired `[packages] auto_update`: folded into [`EDIT_PACKAGES_ENABLED`] — a `false`
/// keeps Automatic updates off whatever `enabled` says.
pub(crate) const RETIRED_PACKAGES_AUTO_UPDATE: &str = "packages.auto_update";
/// The retired `[packages] seed_install`: read as [`EDIT_PACKAGES_AUTO_INSTALL`] while that
/// is unset.
pub(crate) const RETIRED_PACKAGES_SEED_INSTALL: &str = "packages.seed_install";
/// `[update] enabled` — [`UPDATE_CHECK_LABEL`] (Settings ▸ Software Update's Automatic
/// updates card, Search and Modified), default ON. It
/// replaces `$ATERM_NO_AUTO_UPDATE` (2026-09-23): off stops the background checker only
/// (Check for Updates and Update Now still work), read once per process, so it applies
/// from the next launch.
pub(crate) const EDIT_UPDATE_ENABLED: &str = "update.enabled";
/// The ONE name of [`EDIT_UPDATE_ENABLED`]'s switch: its registry label, which the
/// Software Update card paints, the switch and Reset are named by, and Search,
/// Modified, the status line and the card's caption all say — the updater's own
/// constant ([`aterm_update_core::settings::CHECK_LABEL`]), which the Manual page
/// and the updater's messages say too.
pub(crate) const UPDATE_CHECK_LABEL: &str = aterm_update_core::settings::CHECK_LABEL;
/// The ONE name of `[update] auto_apply`'s switch, as [`UPDATE_CHECK_LABEL`] is the
/// check switch's.
pub(crate) const UPDATE_INSTALL_LABEL: &str = aterm_update_core::settings::INSTALL_LABEL;
/// `[reroute] announce` — whether a rerouted upstream Rust tool (`cargo`, `rustc`,
/// `tlc`) prints its signpost before it runs, default ON. The owner's 2026-09-08 ask
/// ("a printed message … that could be suppressed with a flag") made a setting on
/// 2026-09-23, replacing `$ATERM_REROUTE_QUIET`; the co-located atpkg reads it.
pub(crate) const EDIT_REROUTE_ANNOUNCE: &str = "reroute.announce";
/// The aterm harness's durable master switch, `[harness] enabled` (design
/// `docs/DESIGN-aterm-wrapper-2026-09-17.md` §4.6.2), a Bool, default ON. It
/// lives in `aterm.toml` so the kill switch never depends on the thing it kills:
/// it works when the harness tree is missing, broken or mid-update. Every finer
/// key sits in the same table (`aterm_agent::supervise::config::KEYS`), at full
/// power by default and able only to limit it; the row says which are set
/// (`HarnessPolicy::words`).
pub(crate) const EDIT_HARNESS_ENABLED: &str = "harness.enabled";
/// `[harness] autopilot`: how far the harness carries a session that has no
/// Autopilot word of its own (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §2.1). An Enum of the supervisor's words in the chip's cycle order
/// ([`HARNESS_AUTOPILOT_WORDS`]), default `cruise` (full power).
///
/// `cruise` (FOREVER CRUISE): every point answered and the work continued;
/// nothing the agent says ends it — a done claim gets the Cruise prompt.
/// `land` (FOREVER LAND): all of the work landed on the remote main branch —
/// the land instruction once, then a nudge at every point until a clean
/// `Nothing left:` defense stands over a repository holding nothing no
/// remote main has, then nothing typed until someone's turn. `off`: the
/// harness keeps its hands off (plain Claude Code; the window's host stops
/// the session's worker). Each step down takes power
/// away, so `land` and `off` are limits. A session's own word (the footer's
/// chip, Settings ▸ Harness, `aterm ctl @<sid> meta set autopilot …`)
/// decides over it; the master switch off wins over both. The row seeds
/// from the SUPERVISOR's resolved mode ([`Config::harness`]), the default
/// its placeholder.
pub(crate) const EDIT_HARNESS_AUTOPILOT: &str = "harness.autopilot";
/// `[harness] land_at` — THE TIME A TIMED CRUISE IS OFFERED TO LAND AT
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.4): a local `HH:MM`,
/// `00:00` to `23:59`, empty by default. The Autopilot chip's menu and
/// Settings ▸ Harness's session rows offer "Cruise until 18:00, then land"
/// with it; nothing arms it by itself, so it takes no power and gives none.
/// A Text row registered through [`NESTED_LEAVES`] beside the default
/// Autopilot, so it is a Settings row, a Search hit and a Manual key; a value
/// that is no time is refused at Save (the supervisor's reader keeps the
/// default and says so, `aterm_agent::supervise::LandAt`). The row seeds from
/// the SUPERVISOR's resolved time ([`Config::harness`]).
pub(crate) const EDIT_HARNESS_LAND_AT: &str = "harness.land_at";
/// [`EDIT_HARNESS_AUTOPILOT`]'s options: `aterm_agent::supervise::Autopilot`'s
/// three words in the order the chip cycles them, read off `Autopilot::ALL`.
pub(crate) const HARNESS_AUTOPILOT_WORDS: [&str; 3] = {
    use aterm_agent::supervise::Autopilot;
    let [a, b, c] = Autopilot::ALL;
    [a.word(), b.word(), c.word()]
};
/// `[harness] approve` — what the supervisor answers a permission box with
/// (owner, 2026-09-24: *"all such dialogs must be approved by default unless
/// there is a setting added later by the user explicitly to NOT do this"*,
/// and *"configuration limits power"*). An Enum, `all` by default,
/// registered through [`NESTED_LEAVES`] beside the master switch, so it is a
/// Settings row, a Search hit and a Manual key at once — "the setting added
/// later" is this row.
///
/// `all`: every box its answer — a permission box its one-shot allow
/// (`allow-once@v1`; never a standing grant, never a purchase). `safe`: only
/// what the safe rules prove (owner decision 1: a read-only command, an rm
/// under a scratch root in bypass, a Read under an allowed root outside the
/// secrets, the trust dialog under a trust root) — they judge Claude Code's
/// boxes alone: aterm answers no Codex box (codex-alab answers its own). `none`:
/// every box is the owner's. One level for every agent: no
/// key but `enabled` is per agent. The row seeds from the SUPERVISOR's
/// resolved level ([`Config::harness`]), so a value reads as the reader took
/// it: an off spelling `none`, any other value that is no level no setting
/// (`all`).
pub(crate) const EDIT_HARNESS_APPROVE: &str = "harness.approve";
/// `[harness] answer_questions`: whether the supervisor answers a Claude Code
/// question dialog (AskUserQuestion), a Bool, default ON. ON, it answers as
/// [`EDIT_HARNESS_QUESTION_PRIORITY`] says: under `recommended` the recommended
/// option (option 1 when none is marked; every recommended option of a
/// multi-select) by Enter once a person has been quiet (`answer-recommended@v1`),
/// never the free-text row, chat row or Cancel; a question asked in prose gets
/// `[harness] answer_text` (`answer@v1`). A question a person has begun
/// answering is theirs. OFF, every question goes to the menu bar. Independent of
/// [`EDIT_HARNESS_APPROVE`]: a question is no permission. The row seeds from the
/// supervisor's resolved bit ([`Config::harness`]): an off spelling is OFF, any
/// other value that is not a boolean is no setting (ON).
pub(crate) const EDIT_HARNESS_ANSWER_QUESTIONS: &str = "harness.answer_questions";
/// `[harness] question_priority`: how the supervisor answers a question dialog
/// while [`EDIT_HARNESS_ANSWER_QUESTIONS`] is on. An Enum, default `recommended`
/// (the dialog's recommended option). Under `long-term` / `short-term` no option
/// is chosen: the policy's fixed text is written into the free-text row and
/// submitted, so the worker decides by the policy (a preview tab, or one the text
/// would not fit, gets its recommended option). A session's own `meta set
/// questions` word decides over it; a refused value is no setting (the reader,
/// [`Config::harness`], keeps `recommended`).
pub(crate) const EDIT_HARNESS_QUESTION_PRIORITY: &str = "harness.question_priority";
/// `[harness.claude] enabled` — whether Claude Code's sessions are
/// supervised, under the master switch ([`EDIT_HARNESS_ENABLED`]). A Bool, ON
/// by default; `false` puts Claude Code in the supervisor's `off` list. The
/// row seeds from that list ([`Config::harness_agent_enabled`]), never from
/// the master switch, which is its own row. The window's host enforces it
/// when saved (live: no [`application_timing`]).
pub(crate) const EDIT_HARNESS_CLAUDE_ENABLED: &str = "harness.claude.enabled";
/// `[harness.codex] enabled` — [`EDIT_HARNESS_CLAUDE_ENABLED`] for Codex.
pub(crate) const EDIT_HARNESS_CODEX_ENABLED: &str = "harness.codex.enabled";

/// The label of Codex's own switch ([`EDIT_HARNESS_CODEX_ENABLED`]): aterm
/// supervises no Codex (the Codex screen lane's retirement, 2026-10-16); the
/// switch turns off its watch beside codex-alab, which supervises itself.
pub(crate) const CODEX_SWITCH_LABEL: &str = "Watch codex-alab sessions";

/// The label of the switch of the agent named `product`
/// (`Program::product`): "Supervise <product> sessions", but Codex's
/// ([`CODEX_SWITCH_LABEL`]).
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

/// The agent a `harness.<agent>.enabled` row switches, read from its key: a
/// [`Program`] with a harness table ([`Program::harness_table`]) by its own
/// name. Every per-agent arm of a row
/// (its seed, timing, live effect, keywords, note, inactive state and
/// Settings' effect) answers through this, so a new agent's switch is its
/// [`NESTED_LEAVES`] row and its Manual table header
/// (`native_config_language`), nothing else.
pub(crate) fn agent_switch(key: &str) -> Option<Program> {
    let word = key.strip_prefix("harness.")?.strip_suffix(".enabled")?;
    aterm_phase::program_of(word).filter(|p| p.harness_table() && p.name() == word)
}

/// The name a row is searched by as its OWN, beside its label and key
/// (`native_config_language::query_match_rank`): an agent's switch answers
/// to its agent's word (`claude`, `codex`), which its label says only
/// mid-sentence ("Supervise Claude Code sessions") and its key only
/// mid-path, so a search for the agent finds its switch before the rows
/// that list the word as a synonym ([`keywords_of`]).
pub(crate) fn own_name_of(key: &str) -> Option<&'static str> {
    agent_switch(key).map(Program::name)
}

/// `[ai] enabled`: whether the window looks at the AI CLIs aterm manages and
/// says, in Messages, which one needs a person
/// (`docs/DESIGN-ai-connections-2026-10-16.md` §7.3). A Bool, default ON, read
/// by the AI layer's own reader ([`crate::app_config::AiPolicy`]); off, no look
/// runs and the AI lines end. The `aterm ai` verbs always work.
pub(crate) const EDIT_AI_ENABLED: &str = "ai.enabled";
/// `[ai] preferred`: this machine's main AI CLI (the AI-connections design
/// §6.1), the one that asks when a review names none and the doctor's first
/// row. An Enum over every connection's id ([`aterm_agent::ai::connector::IDS`]),
/// defaulting to the first CLI this platform has a build of. A choice, not a
/// grant, so a Settings row; a fleet document may set it too, and the row says
/// so.
pub(crate) const EDIT_AI_PREFERRED: &str = "ai.preferred";
/// `[ai] reviews` — whether an AI agent here may ask the other AI CLIs used
/// here to review its change (`aterm ai review`, §9). A Bool, ON by default:
/// batteries included, every CLI used here reviews with all its tools.
pub(crate) const EDIT_AI_REVIEWS: &str = "ai.reviews";
/// `[ai] review_permissions` — the reviewers' permission level: `inherit`
/// (the default: the asking agent's own, yolo when a person asks) or a pinned
/// `yolo`, `edits`, `ask` or `plan` ([`aterm_agent::ai::permission`]).
pub(crate) const EDIT_AI_REVIEW_PERMISSIONS: &str = "ai.review_permissions";
/// `[ai] machine` — this machine's `[machine.<name>]` row in the fleet
/// document (§6.2). Text; the AI layer's reader notes a bad name.
pub(crate) const EDIT_AI_MACHINE: &str = "ai.machine";
/// `[ai] fleet` — the fleet document: an absolute path, or an `https://` URL
/// aterm fetches and verifies against `[ai] fleet_key` (§6.3). Text.
pub(crate) const EDIT_AI_FLEET: &str = "ai.fleet";
/// `[ai] fleet_key` — the fleet document's signer, pinned: an Ed25519 public
/// key in base64 (`aterm ai fleet keygen` prints one). Text.
pub(crate) const EDIT_AI_FLEET_KEY: &str = "ai.fleet_key";

/// The AI CLI an `ai.<id>.use` row switches, read from its key: a connection
/// by its own id (`aterm_agent::ai::connector`). Every per-CLI arm of a row
/// (its seed, note, keywords and the AI CLIs page's placement) answers through
/// this, so a fifth CLI's switch is its [`NESTED_LEAVES`] row and its Manual
/// table header (`native_config_language`), nothing else.
pub(crate) fn ai_use_switch(key: &str) -> Option<&'static aterm_agent::ai::connector::Connector> {
    let id = key.strip_prefix("ai.")?.strip_suffix(".use")?;
    aterm_agent::ai::connector::connector(id)
}

/// `[machine] universal_control`: `"off"` (default) disables macOS Universal
/// Control for this host whenever the `[machine]` settings are applied (`aterm
/// pkg machine apply`, by the co-located atpkg); `"leave"` never touches it. The
/// Security page shows the measured state beside the switch.
pub(crate) const EDIT_MACHINE_UNIVERSAL_CONTROL: &str = "machine.universal_control";

/// The permission toggles. All fail-closed (default OFF) except
/// `allow_palette_reconfigure`, which defaults ON
/// ([`crate::app_config::Config::allow_palette_reconfigure_or_default`]). Grouped so
/// `edit_kind` and the schema rows agree they are booleans, and so the Settings UI can
/// label them together as "Security" controls.
pub(crate) const SECURITY_BOOL_KEYS: &[&str] = &[
    EDIT_ALLOW_OSC52_QUERY,
    EDIT_ALLOW_WINDOW_OPS,
    EDIT_ALLOW_NOTIFICATIONS,
    EDIT_ALLOW_PALETTE_RECONFIGURE,
    EDIT_ALLOW_KITTY_FILE_TRANSFER,
    // The one protection in the set: the five above grant programs abilities,
    // this one takes one away from every other process.
    EDIT_SECURE_KEYBOARD_ENTRY,
];

pub(crate) const EDIT_WINDOW_THEME: &str = "window_theme";
pub(crate) const EDIT_BIDI: &str = "bidi";
pub(crate) const EDIT_AMBIGUOUS_WIDTH: &str = "ambiguous_width";

// Enum option domains — the CANONICAL spellings each config loader accepts (aliases like
// beam/on/off/single/double still parse from a hand-edited file, but the picker only
// offers — and `typed_item` only writes — these canonical values). Keep in sync with the
// parsers in `app_config.rs` (a hand-typed alias outside this set resolves at load).
// block + bar only; DECSCUSR still honoured.
pub(crate) const CURSOR_STYLES: &[&str] = &["block", "bar"];
/// `[machine] universal_control` — the two spellings `atpkg::config` resolves.
pub(crate) const UNIVERSAL_CONTROL_POLICIES: &[&str] = &["off", "leave"];
pub(crate) const WINDOW_THEMES: &[&str] = &["auto", "light", "dark"];
/// `palette_generate`: follow the theme's own key, derive for every theme,
/// or keep the xterm table.
pub(crate) const PALETTE_GENERATE_MODES: &[&str] = &["theme", "always", "never"];
pub(crate) const BIDI_MODES: &[&str] = &["implicit", "disabled", "explicit"];
pub(crate) const AMBIGUOUS_WIDTHS: &[&str] = &["narrow", "wide"];
/// Closed domain for `predictive_echo` (`PredictMode::parse` also accepts aliases
/// like auto/on/force). An Enum, so the overlay cycles a valid value and Save
/// rejects a typo that `parse` would silently map to Off.
pub(crate) const PREDICTIVE_ECHO_MODES: &[&str] = &["off", "adaptive", "always"];
/// Closed domain for `text_blending` (the anti-aliasing weight): the canonical
/// spellings `text_blending_or_default` accepts (the `linear_corrected` alias
/// still parses from a hand-edited file).
pub(crate) const TEXT_BLENDINGS: &[&str] = &["linear-corrected", "linear"];
/// Closed domain for the `motion` accessibility policy: the exact spellings
/// `MotionMode::parse` accepts and the picker writes.
pub(crate) const MOTION_MODES: &[&str] = &["auto", "full", "reduced"];
/// Closed domain for `window_colorspace`: the canonical spellings of
/// [`crate::app_config::WindowColorspace::parse`] (the `displayp3`/`p3` aliases
/// still parse from a hand-edited file).
pub(crate) const WINDOW_COLORSPACES: &[&str] = &["srgb", "display-p3"];
/// Closed domain for `background_material` (vibrancy): the canonical spellings of
/// [`crate::app_config::BackgroundMaterial::parse`] (the `underwindow` /
/// `under_window` aliases still parse from a hand-edited file).
pub(crate) const BACKGROUND_MATERIALS: &[&str] = &["none", "hud", "sidebar", "under-window"];
/// Closed domain for `sparkle_words.profanity.style` — the spellings
/// `sparkle_deco_config` classifies (unknown values fall back to `rainbow`).
pub(crate) const SPARKLE_PROFANITY_STYLES: &[&str] = &["rainbow", "nova", "sparkle"];
/// Closed domain for `sparkle_words.feline.style` (unknown → `cat`).
pub(crate) const SPARKLE_FELINE_STYLES: &[&str] = &["cat", "paw"];
pub(crate) const EDIT_PREDICTIVE_ECHO: &str = "predictive_echo";
/// Predictive editing (`Config::predictive_echo_editing`, default on): the
/// kill switch for `←`, `→` and the end-of-line Backspace as predicted edits
/// (`Predictor::set_editing`). Inert while `predictive_echo` is off.
pub(crate) const EDIT_PREDICTIVE_ECHO_EDITING: &str = "predictive_echo_editing";
/// Focus-linked shell priority boost (Windows QoS; no-op elsewhere).
pub(crate) const EDIT_FOCUS_BOOST: &str = "focus_boost";

// Every remaining `Config` scalar key has a registry row so Manual/schema
// discovery and `settings set|unset` can address it. Each key's type arm in
// `edit_kind` matters: a Bool that falls to Text writes a TOML string a serde
// `Option<bool>` rejects on reload.

/// GPU rendering master switch (`Config::gpu`, default ON with CPU fallback).
/// Restart-only: the backend is built at launch, and `restart_notices` raises the
/// "applies on next launch" banner when it changes.
pub(crate) const EDIT_GPU: &str = "gpu";
/// Indexed ANSI palette (`Config::palette`, `Vec<String>` of `#RRGGBB` by 0-based
/// index) — a [`LIST_KEYS`] row (comma-separated text ⇄ TOML array; entries
/// hex-validated at Save like the [`EditKind::Color`] rows).
pub(crate) const EDIT_PALETTE: &str = "palette";
/// Sparse indexed palette slots (`Config::palette_overrides`, a table of
/// `#RRGGBB` by slot index applied over `palette`). The patch lane writes one
/// slot as the dotted key `palette_overrides.<index>`
/// ([`crate::resolved_colors::palette_override_key`]), hex-validated as an
/// [`EditKind::Color`]; the whole table is removed by this key.
pub(crate) const EDIT_PALETTE_OVERRIDES: &str = "palette_overrides";
/// Trail sound level 0..=1 (`Config::trail_sound_volume`, default 0.4).
pub(crate) const EDIT_TRAIL_SOUND_VOLUME: &str = "trail_sound_volume";
// (The trail colour/accent, kitty sprite, comet-geometry, bloom, shimmer and
// HDR/SDR glow keys are declared once with the core cursor-trail keys above.)
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
/// Terminal-tab backdrop image path (`wallpaper`). Unlike the other asset keys it
/// stays structured-writable: the Settings ▸ Wallpaper picker writes it, and
/// `VersionedConfigService::patch` re-resolves the image on that patch.
pub(crate) const EDIT_WALLPAPER: &str = "wallpaper";
/// Wallpaper legibility dim (`wallpaper_dim`, default 0.3, clamp 0..=1).
pub(crate) const EDIT_WALLPAPER_DIM: &str = "wallpaper_dim";
/// Backdrop-hue glyph tint while a wallpaper is attached
/// (`wallpaper_text_tint`, default ON).
pub(crate) const EDIT_WALLPAPER_TEXT_TINT: &str = "wallpaper_text_tint";
/// Per-session temporal recorder opt-in (`temporal_recording`, default OFF).
pub(crate) const EDIT_TEMPORAL_RECORDING: &str = "temporal_recording";
/// All-edge interior window padding in logical px (`window_padding`, default 12,
/// clamp 0..=64). Hot-applies: a reload re-resolves every window's metrics and
/// re-grids (see `App::reload_config`).
pub(crate) const EDIT_WINDOW_PADDING: &str = "window_padding";
/// Top-edge padding override in logical px (`window_padding_top`, default 2;
/// resolver-clamped to `0..=window_padding` — the renderer's `pad_top <= pad` law).
pub(crate) const EDIT_WINDOW_PADDING_TOP: &str = "window_padding_top";

/// The LIST-VALUED keys accepted by the compatibility `settings set` writer.
///
/// Native Settings deliberately does not render these as comma-separated text
/// controls: a hand-authored member can itself contain a comma (for example a
/// shell argument), so flattening an existing TOML array would be lossy. Manual
/// owns their structured editing. The writer still accepts a comma-separated
/// replacement for legacy control clients and serializes that explicit replacement
/// as a real TOML array. `palette` entries are additionally hex-validated.
pub(crate) const LIST_KEYS: &[&str] = &[EDIT_PALETTE, EDIT_SHELL_ARGS, EDIT_FONT_FEATURES];

/// Collection-shaped values that have no lossless one-line Settings control.
/// `fallback_fonts` and `font_variation` accept either a string or an array via
/// `FontList`; the other keys are strict TOML arrays. All remain searchable and
/// assisted in Manual, and Modified reports their exact authored representation.
pub(crate) fn manual_collection_key(key: &str) -> bool {
    LIST_KEYS.contains(&key) || matches!(key, EDIT_FALLBACK_FONTS | EDIT_FONT_VARIATION)
}

/// Config keys whose value names filesystem-backed assets. Structured Settings
/// cannot publish these atomically without worker validation, so Manual owns
/// their editing and the exact-observation worker admits text + decoded assets
/// as one generation.
pub(crate) fn config_asset_source_key(key: &str) -> bool {
    matches!(key, EDIT_CURSOR_NYAN_SPRITE)
}

/// Values edited only in Manual: lossless collections, asset sources that need
/// off-thread preparation, and the legacy feline style with no honest native
/// control (`style = "paw"` is ink-only). Keys that do nothing are not registry
/// rows; Manual types them from `native_config_language`'s compatibility
/// metadata.
pub(crate) fn manual_only_key(key: &str) -> bool {
    manual_collection_key(key)
        || config_asset_source_key(key)
        || key == "sparkle_words.feline.style"
}

/// One SCALAR leaf of a nested config table (`[update]` / `[sparkle_words]`
/// (+ sub-tables) / `[matrix_rain]`), addressed by its DOTTED TOML path
/// (`"update.auto_apply"`, `"sparkle_words.profanity.enabled"`). The writer
/// ([`apply_prefs_edits`]) walks/creates the table chain non-destructively, so
/// these edit exactly like top-level keys; `settings set update.auto_apply …`
/// works.
pub(crate) struct NestedLeaf {
    /// Dotted `aterm.toml` path (every segment a bare TOML key).
    pub(crate) key: &'static str,
    /// The on-screen row label.
    pub(crate) label: &'static str,
    /// The TOML value type — same corruption stakes as the top-level arms.
    pub(crate) kind: EditKind,
}

/// Every REGISTERED nested scalar leaf — the single source `edit_kind`,
/// [`editable_fields`], and the conformance tests all read, so a leaf cannot be
/// typed one way in the UI and another in the writer. LIST-valued nested fields
/// (`sparkle_words.languages`/`toy_packs`/`deny`, the per-class
/// `extra_words`/`ignore_words`/`palette`, `[[sparkle_words.custom]]`) are
/// deliberately NOT here: only scalar leaves are registered, and those tables'
/// arrays remain hand-edit/TOML-native (no faithful single-text shape, so a
/// lossy encoding could corrupt them on a Save round-trip).
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
    // [update]: whether the self-updater runs, and its apply policy. The channel
    // (`owner`/`repo`) and `require_team_id` are development settings (a shipped
    // build reads its compiled channel and pin), so they are not rows.
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
    // [reroute] — the upstream-Rust signpost line (atpkg reads it).
    NestedLeaf {
        key: EDIT_REROUTE_ANNOUNCE,
        label: "Show a note when cargo or rustc is rerouted",
        kind: EditKind::Bool,
    },
    // [harness]: the supervisor's master switch, the first of its rows. Keys with
    // no row here only limit it and are written in Manual; the row's words name
    // each limit written, by its row where it has one (`HarnessPolicy::words`
    // reads this registry).
    NestedLeaf {
        key: EDIT_HARNESS_ENABLED,
        label: "Supervise coding agents",
        kind: EditKind::Bool,
    },
    // [harness.<agent>] — one switch per supervisable agent, under the
    // master switch (`docs/DESIGN-harness-plugins-2026-10-15.md` §4),
    // labelled "Supervise <product> sessions" (the product name is
    // `Program::product`) as the master switch is "Supervise coding agents":
    // the row turns the harness's hold on that agent off, never the agent
    // itself. Codex's reads "Watch codex-alab sessions" ([`agent_switch_label`]):
    // aterm supervises no Codex, and watches beside codex-alab, which
    // supervises itself (the Codex screen lane's retirement, 2026-10-16).
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
    // The default Autopilot (the Autopilot design, 2026-10-15): how far the
    // harness carries a session with no word of its own — `cruise`, `land`
    // or `off`, each step down a limit.
    NestedLeaf {
        key: EDIT_HARNESS_AUTOPILOT,
        label: "Default autopilot",
        kind: EditKind::Enum {
            options: &HARNESS_AUTOPILOT_WORDS,
        },
    },
    // The time a timed Cruise is offered to land at (the Autopilot design
    // §9.3.4): offered by the chip's menu and the session rows, never armed
    // by itself.
    NestedLeaf {
        key: EDIT_HARNESS_LAND_AT,
        label: "Land at",
        kind: EditKind::Text,
    },
    // The supervisor's approval level (owner, 2026-09-24): `all` answers
    // every box; `safe` is decision 1's proven rules; `none` answers none.
    NestedLeaf {
        key: EDIT_HARNESS_APPROVE,
        label: "Approve permission boxes",
        kind: EditKind::Enum {
            options: &["all", "safe", "none"],
        },
    },
    // Whether a question is answered (a dialog, or one asked in prose), as the
    // priority row below says; OFF hands every question over. The label names
    // no way of answering: that is the priority's.
    NestedLeaf {
        key: EDIT_HARNESS_ANSWER_QUESTIONS,
        label: "Answer questions",
        kind: EditKind::Bool,
    },
    // How a question is answered: its recommended option, or the worker's own
    // choice under the long-term or short-term policy.
    NestedLeaf {
        key: EDIT_HARNESS_QUESTION_PRIORITY,
        label: "Question priority",
        kind: EditKind::Enum {
            options: &aterm_agent::supervise::QuestionPriority::WORDS,
        },
    },
    // [ai] — the AI CLIs aterm keeps a login to (docs/DESIGN-ai-connections-
    // 2026-10-16.md §10): this machine's preferred CLI, the look's switch,
    // reviews and their permissions, the fleet document, and one "Use on this
    // machine" switch per CLI. Drawn by Settings ▸ Harness ▸ AI CLIs
    // (`native_settings_ai.rs`). The reviewers list and the review knobs
    // (`reviewers`, `review_count`, `review_timeout_s`) are Manual rows.
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
    // [sparkle_words] — master + top-level scalars.
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
    // [sparkle_words.profanity]
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
    // The bonk's two gates keep their `[sparkle_words.profanity]` file spelling
    // (a renamed key orphans every authored line) but carry Sound-menu labels:
    // the row has to say what it sounds like.
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
    // [sparkle_words.feline]
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
    // [sparkle_words.ink] / [sparkle_words.emphasis]
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
    // [matrix_rain]: every scalar knob with an effect, the `enabled` master
    // included. The inert v1 keys still parse and Manual types them, but they are
    // not rows.
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
        // `matrix`/`theme`/`#RRGGBB`: an open domain, so Text, not Enum; a
        // malformed hex fails closed to matrix green at load.
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
    // [output_streak] — PRISM WAKE. Two leaves are rows: its switch, in
    // Appearance's Effects box beside Matrix rain (its preview ruling
    // is the pip's: the workbench scene has no program output to answer), and
    // the pip, in the Sound menu (design §7). The four tuning keys
    // (`intensity`, `tail`, `max_streaks`, `idle_secs`) shape a sub-second
    // comet no Settings preview can show, so they stay Manual's;
    // `native_config_language` lists them there.
    //
    // The pip's label is written for the Sound menu it appears in, not its table:
    // a row away from the volume slider has to say what it sounds like.
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
    // [presence] — the needs border and its pill (owner surfaces §5). The View
    // menu's switch is primary; this row keeps it findable (Search /
    // Modified) and hand-editable with the same typing the writer uses.
    NestedLeaf {
        key: EDIT_PRESENCE_RIM,
        label: "Show needs in windows",
        kind: EditKind::Bool,
    },
];

/// Registry lookup for a dotted nested key — `None` for top-level keys and
/// unregistered paths.
pub(crate) fn nested_leaf(key: &str) -> Option<&'static NestedLeaf> {
    NESTED_LEAVES.iter().find(|l| l.key == key)
}

/// Every setting whose uncommitted value must have a renderer-native visual
/// projection. The Settings coverage gate compares it with the editable-field
/// sections, so a new visual field without a preview mapping fails tests.
pub(crate) const VISUAL_PREVIEW_KEYS: &[&str] = &[
    // Appearance (11)
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
    // Text & Fonts (22)
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
    // Cursor Styles — the style list's page leads with its own key, so a page
    // preview with nothing focused shows the style; everything below previews
    // through the same cursor scene.
    EDIT_CURSOR_TRAIL_STYLE,
    EDIT_CURSOR_NYAN_SPRITE,
    // Cursor & Motion, and the shape and tuning on Cursor Styles
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

/// Default scrollback line cap when `scrollback_lines` is unset (mirrors the engine
/// `TerminalConfig.scrollback_limit` default). Shown as the [`EditField::placeholder`]
/// for the scrollback row so an unset control reads as the real resolved value, not a
/// confusing blank.
const DEFAULT_SCROLLBACK_LINES: usize = 100_000;

/// The default cursor style when `cursor_style` is unset (the engine default is a block
/// cursor). The placeholder hint for the cursor-style row.
const DEFAULT_CURSOR_STYLE: &str = "block";

/// The default `cursor_trail_style` when unset: the rainbow ribbon trailed by the
/// full-body cat that walks, runs and pounces along the line. Older spellings
/// resolve via [`CURSOR_TRAIL_STYLE_ALIASES`]. The single definition of the
/// default: `app_config`, `native_settings` and `settings_preview` read it.
pub(crate) const DEFAULT_CURSOR_TRAIL_STYLE: &str = "rainbow kitty pet";

/// Every canonical `cursor_trail_style`, in the order the Settings Cursor
/// Styles page and the Cursor & Motion picker list them: the save-time domain
/// the writer and `--validate-config`'s near-miss suggestions read, and the
/// picker's options. Every row is listed; nothing is hidden. The rows, the
/// aliases, the list names and the one-line descriptions are one table in
/// the engine ([`aterm_effects::cursor_glow::spellings`]), which a web page's
/// `set_cursor_glow` reads too. The old `classic mono` spellings are aliases
/// of `classic` that still draw its one-colour face
/// (`GlowStyle::style_names_classic_mono` reads the authored spelling).
pub(crate) const CURSOR_TRAIL_STYLES: &[&str] = &spellings::TRAIL_STYLE_NAMES;

/// `trail_sound_style` options, the typing-sound picker (Settings ▸ Sound ▸
/// "Typing sound"): what the picker, the save-time validation, the a11y option
/// list and the introspection dump read. Built in picker order from the synth's
/// roster ([`SoundVoice::ALL`]; each name is spelled once, in
/// [`aterm_effects::trail_sound::SoundVoice::name`]), so a voice cannot join the
/// synth without joining the picker (`trail_sound_styles_are_the_synth_roster`).
///
/// `auto` (the default) follows the visual trail's palette; the next nine are the
/// shipped palettes by what they sound like (`music box` is the rainbow kitty's,
/// `droplet` water's, …); `mechanical` is the keyboard; `typewriter`, `marimba`
/// and `felt` are sound-only. Aliases (the trail-style names, `mech`, `thock`, …)
/// resolve through [`trail_sound_style_canonical`] / `Config::trail_sound_voice`:
/// accepted on load, never offered.
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

/// Resolve a `trail_sound_style` spelling (canonical or documented alias,
/// case-insensitive, whitespace-tolerant) to its canonical picker option, or
/// `None` for one the runtime would play as `auto`, which the validator flags.
/// Answered by the synth's own parser, so the UI, the validator and the engine
/// share one vocabulary.
pub(crate) fn trail_sound_style_canonical(token: &str) -> Option<&'static str> {
    SoundVoice::parse(token).map(SoundVoice::name)
}

/// Documented aliases accepted by the runtime, the validator and the Settings
/// writer, each mapped in one step to a [`CURSOR_TRAIL_STYLES`] name. The
/// engine's table ([`spellings::TRAIL_STYLE_ALIASES`]), so every host reads an
/// alias the same way.
pub(crate) const CURSOR_TRAIL_STYLE_ALIASES: &[(&str, &str)] = spellings::TRAIL_STYLE_ALIASES;

/// Resolve a `cursor_trail_style` spelling (a canonical row or a
/// documented alias; trimmed, case-insensitive) to its canonical name, or
/// `None` for a spelling nothing recognizes — the case that falls back to
/// [`DEFAULT_CURSOR_TRAIL_STYLE`], and that the validator and the load-time
/// warning must flag so the substitution is not silent.
pub(crate) fn cursor_trail_style_canonical(token: &str) -> Option<&'static str> {
    spellings::canonical(token)
}

/// How a Preferences key should be TYPED in the written TOML, so a Save round-trips
/// through `Config`'s serde types (font_px a number, scrollback_lines int,
/// copy_on_select bool, the rest strings). Drives [`apply_prefs_edits`]'s value
/// construction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EditKind {
    /// A number that may have a fraction (`font_px`, `line_height`). Written as
    /// the shortest decimal that is exactly the value, with no fraction when it
    /// has none ([`written_number`]): `font_px = 15`, `line_height = 1.15`.
    Float,
    /// An integer (`scrollback_lines`). Written as a TOML integer, whatever the
    /// value's spelling (`5000.0` writes `5000`, [`whole_number`]). [`typed_item`]
    /// REJECTS values outside the key's serde-representable domain
    /// ([`integer_domain`]): most Integer fields are unsigned or narrow, and a
    /// `-1` written for a `usize`/`u16` field is valid TOML the reload's serde
    /// parse rejects WHOLESALE — the live reload would keep the old config
    /// while the NEXT launch silently resets everything to defaults.
    Integer,
    /// A boolean (`copy_on_select`). Written as a TOML `true`/`false`.
    Bool,
    /// A free-form string (`theme`, `font_family`, `cursor_style`). Written as a
    /// TOML basic string.
    Text,
    /// A string from a fixed set (`cursor_trail_style`). [`typed_item`] rejects a
    /// value outside `options` (case-insensitive) and writes the canonical
    /// spelling; `controls settings` advertises `options`.
    Enum { options: &'static [&'static str] },
    /// The `theme` key: a colour-scheme name from the built-in registry or a
    /// valid user `<name>.conf`, resolved dynamically. Not domain-rejected: the
    /// value can also be the `dark:X,light:Y` split form.
    Theme,
    /// An `RRGGBB`/`#RRGGBB` colour string (`foreground`, `cursor_color`, …).
    /// [`typed_item`] rejects a value that does not parse as a hex colour
    /// ([`aterm_types::scheme::parse_hex`]), which load would ignore. The overlay
    /// edits these rows with the colour wheel popover.
    Color,
}

/// The TOML type of each editable key, so [`apply_prefs_edits`] can parse the raw
/// control text into a correctly-typed `aterm-toml` value. The single source of truth
/// shared by the window (which builds the controls) and the writer (which types them).
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
    // Nested dotted-key leaves carry their type in their one registry row
    // (`NESTED_LEAVES`), so the writer and the UI cannot disagree.
    if let Some(leaf) = nested_leaf(key) {
        return leaf.kind;
    }
    // List-valued keys edit as Text (the one-line comma form, like
    // `fallback_fonts`); `typed_item` writes them as TOML arrays.
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
        // The trail colour overrides are hex like the theme colours but live in
        // the Cursor section, so they get direct Color arms rather than a
        // COLOR_KEYS entry (whose slice also routes section/group). The
        // selected-tab override and the chrome colours beside it are the same
        // shape in the Window section.
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
        // The dotted [packages] switches must type as Bool: a TOML string would
        // corrupt a table the co-located atpkg also parses.
        | EDIT_PACKAGES_ENABLED
        | EDIT_PACKAGES_AUTO_INSTALL
        // …and their retired spellings: Undo of a Reset writes one back, as the
        // Bool atpkg's serde reads.
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

/// How the Settings theme card is set: one theme for every OS appearance, or a
/// light/dark pair that follows the OS. Read off the value itself (a
/// `dark:…,light:…` split that names a theme is Follow system; no value, or one
/// that names no theme, is Default, one theme), so the card opens in the mode
/// the file is in and no hidden flag can disagree with `aterm.toml` or with
/// what the terminal draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeMode {
    /// One theme whatever the OS appearance: a plain name, or no theme at all
    /// (Default).
    Always,
    /// The OS appearance picks the side: a `dark:…,light:…` split.
    FollowSystem,
}

/// The mode a configured `theme` value (`None` when unset) is in, by the one
/// selector grammar ([`aterm_types::scheme::ThemeSelector::parse`]). A value
/// that names no theme ([`theme_is_unset`]) draws Default on both sides, so it
/// is one theme, whatever its spelling.
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

/// The name an unset `theme` draws.
const DEFAULT_THEME_NAME: &str = "Default";

/// The `(dark, light)` pair `name` seeds when Follow system is turned on. The
/// name keeps the side it is drawn for (declared or measured, as the catalog
/// admitted it), so turning the mode on never changes what that side draws; the
/// other side is its catalog pair, else aterm's own theme for that side
/// (Default, Default Light), so the mode really follows the system. A name the
/// catalog does not know stays on the dark side.
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

/// What the Follow-system card's Dark and Light slots show for a configured
/// value, as `(dark, light)`. A split shows exactly what the runtime draws on
/// each side ([`aterm_types::scheme::ThemeSelector::name_for`], so a side the
/// value leaves out shows the named side's built-in pair, and no side at all
/// is Default). A plain name, or no theme at all (Default), shows the pair
/// turning the mode on would write.
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

/// The `(dark, light)` themes a Follow-system `theme` value draws, read off
/// the value alone: a side the value leaves out is what the runtime draws
/// there ([`aterm_types::scheme::ThemeSelector::name_for`], else Default).
/// `None` for a value that is not a split.
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

/// How a Follow-system `theme` value reads on a control: the theme each side
/// draws, dark first, then that it follows the OS ("Aterm Night / Aterm Day
/// · follows system"), never the raw `dark:…,light:…` text. A split that
/// draws one theme on both sides (`dark:Aterm Day,light:Aterm Day`) does not
/// change with the OS, so it reads as that one name.
/// `None` for a value that is not a split (a plain name reads as itself).
pub(crate) fn theme_pair_label(raw: &str) -> Option<String> {
    theme_pair_names(raw).map(|(dark, light)| {
        if dark == light {
            dark
        } else {
            format!("{dark} / {light} \u{b7} follows system")
        }
    })
}

/// [`theme_pair_label`] without its tail, for a control too narrow for the
/// whole of it: the two names are what the reader needs, and a pair of them
/// already says the value changes with the OS.
pub(crate) fn theme_pair_short_label(raw: &str) -> Option<String> {
    theme_pair_names(raw).map(|(dark, light)| {
        if dark == light {
            dark
        } else {
            format!("{dark} / {light}")
        }
    })
}

/// The `theme` value for a `(dark, light)` pair, in the selector grammar's
/// spelling.
pub(crate) fn theme_split_value(dark: &str, light: &str) -> String {
    format!("dark:{dark},light:{light}")
}

/// What turning Follow system off replaced: the one name it wrote, and the
/// pair that name came from. View state, never config (the mode is the
/// value), so turning the switch back on while the name is still that one
/// gives the pair back ([`theme_follow_system_value`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThemeFollowKept {
    /// The value Off wrote.
    pub(crate) always: String,
    /// The value it replaced.
    pub(crate) pair: String,
}

/// The value turning Follow system on writes. Where the value is still the
/// name turning it off wrote (`kept`), the pair that name came from, so Off
/// then On is a round trip and the side the person had chosen for the other
/// appearance comes back. Otherwise the configured name (Default when none
/// is set, so an unset theme writes `dark:Default,light:Default Light`) on its
/// own side and its pair on the other. A value already following the system
/// is returned as written.
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

/// The value choosing `name` in the `side` slot writes: that side changes,
/// and the other keeps what its slot shows (written out, so a side the value
/// left implicit does not move when its partner changes).
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

/// Whether a configured `theme` value draws what an unset one does (Default,
/// on every appearance): no value, a blank one, or one that names no theme on
/// either side.
pub(crate) fn theme_is_unset(raw: Option<&str>) -> bool {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return true;
    };
    let (selector, _) = aterm_types::scheme::ThemeSelector::parse(raw);
    selector.name_for(aterm_types::Appearance::Dark).is_none()
        && selector.name_for(aterm_types::Appearance::Light).is_none()
}

/// The `theme` value choosing theme `id` for a NAMED side writes (Settings ▸
/// Themes' Keep and Save as, whose Dark | Light segment says the side Keep
/// writes while the theme follows the system): while the value follows the
/// system only the `side` slot changes ([`theme_with_side`]) and the side is
/// returned; over one theme (a plain name, or no theme at all) it is
/// replaced by `id` itself, for every appearance: a choice never starts following the OS
/// (that is the Follow system switch's to do). How the pair is spelled (two
/// themes, or one theme on both sides) never changes the rule.
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

/// The `theme` value a choice that names NO side writes (the command
/// palette, the whole-theme picker), decided by what the chosen theme is
/// made for rather than by how `theme` is spelled. While the value follows
/// the system, a theme made for the `shown` side takes that side alone
/// ([`theme_choice_value`]); one made for the other appearance is kept as
/// the one theme for both, Follow system off: it is what is on screen now,
/// and filed under the shown side it would sit in the slot of the wrong
/// appearance, while filed under its own side it would leave the screen at
/// once. Over one theme, `id` itself. A theme the catalog does not know
/// counts as made for `shown`.
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

/// The value turning Follow system off writes: the theme the `visible` side
/// shows, as one name for both appearances, so what is on screen stays.
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
        // Unset (blank, or naming no theme) draws Default on both sides, so
        // it is one theme always, as a plain name is.
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
        // A light theme keeps the light side.
        assert_eq!(
            theme_follow_system_value(Some("Gruvbox Light"), None, &themes),
            "dark:Gruvbox Dark,light:Gruvbox Light"
        );
        // From Default, Default Light or no theme at all, the switch writes
        // Default's pair out: following the OS is a choice the file records.
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
        // A theme with no pair keeps its own side, and the other side gets
        // aterm's own theme for it, so the mode really follows the system
        // (never the same theme twice); one the catalog does not know stays
        // on the dark side.
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
        // An existing split is never replaced.
        assert_eq!(
            theme_follow_system_value(Some("dark:Dracula,light:Nord"), None, &themes),
            "dark:Dracula,light:Nord"
        );
    }

    /// A choice over a pair changes only the side shown; over a plain name or
    /// no theme at all it is the name itself, for every appearance, so a pick
    /// over an unset theme never starts following the OS.
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
        // Every spelling of "nothing configured" is unset; one that names a
        // theme is not.
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

    /// A pair that draws one theme on both sides follows the same rules as
    /// any pair, never a rule of its own: a choice for a named side changes
    /// that side, and a choice that names no side keeps a theme made for the
    /// other appearance as the one theme for both (the old special case wrote
    /// `dark:Linen,light:Linen`, a pair that follows nothing).
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
        // A name the catalog does not know counts as made for the shown side.
        assert_eq!(
            theme_sideless_choice_value(same, Light, "Harbor Day", &themes).0,
            "dark:Aterm Day,light:Harbor Day"
        );
        // The same choices over a pair of two themes give the same shapes.
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

    /// A choice that names no side never files a theme under the slot of the
    /// other appearance, however `theme` is spelled: a theme made for the
    /// shown side takes that side; one made for the other appearance is kept
    /// as the one theme for both (Follow system off), so what is on screen
    /// stays. Law over every built-in, both sides and every spelling.
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
                    // The shown side draws the chosen theme.
                    let (dark, light) = theme_slots(Some(&value), &themes);
                    let drawn = match (theme_mode(Some(&value)), shown) {
                        (ThemeMode::Always, _) => value.clone(),
                        (ThemeMode::FollowSystem, Dark) => dark.clone(),
                        (ThemeMode::FollowSystem, Light) => light.clone(),
                    };
                    assert_eq!(drawn, theme.name, "{raw:?} {shown:?} -> {value}");
                    if theme_mode(Some(&value)) == ThemeMode::FollowSystem {
                        // Only the shown side changed, and it holds a theme
                        // made for it.
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
        // The lab's cases: Keep Aterm Day on a dark OS over a pair, and
        // Hearth on a light one, keep the theme for both appearances.
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
        // Negative control: a choice for a NAMED side (a slot, the Themes
        // segment) files any theme under that side.
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

    /// Off then On is a round trip: the pair Off replaced comes back while
    /// the value is still the name Off wrote, on either side; any other one
    /// theme seeds its own pair as before.
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
            // Without the memory the name seeds its own pair, which loses
            // the other side (the defect).
            assert_ne!(
                theme_follow_system_value(Some(&always), None, &themes),
                pair
            );
        }
        // Negative control: a different theme since then seeds its own.
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
        // Every side edit writes its pair, Default's pair too: following the
        // OS with aterm's own themes is a choice, not the unset value.
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
        // A side the value leaves implicit is written out as the slot shows
        // it, so it does not move with its partner.
        let implicit = Some("dark:Gruvbox Dark");
        assert_eq!(
            theme_slots(implicit, &themes),
            ("Gruvbox Dark".to_string(), "Gruvbox Light".to_string())
        );
        assert_eq!(
            theme_with_side(implicit, Dark, "Nord", &themes),
            "dark:Nord,light:Gruvbox Light"
        );
        // Every slot edit parses back to the side it named and the side it
        // kept (law over a sweep of built-ins).
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
        // On and off again round-trips the name it started from on its side.
        let on = theme_follow_system_value(Some("Gruvbox Dark"), None, &themes);
        assert_eq!(theme_always_value(Some(&on), Dark, &themes), "Gruvbox Dark");
    }
}

/// An error from [`apply_prefs_edits`]. The caller logs it and leaves the file
/// untouched rather than risk clobbering it.
#[derive(Debug)]
pub(crate) enum PrefsEditError {
    /// The current `aterm.toml` text failed to parse as TOML (`aterm-toml` error). The
    /// edit is refused so a malformed file is never overwritten.
    Parse(String),
    /// A control value did not parse as its key's declared [`EditKind`] (e.g. a
    /// non-numeric font size). Carries the offending `(key, raw)` for the message,
    /// and what the value must be where the writer can say ([`integer_requirement`]
    /// for an Integer key).
    BadValue {
        key: String,
        raw: String,
        expected: Option<String>,
    },
    /// A value that parses but that the resolver would ignore at load: `font_px`
    /// outside `FONT_PX_MIN..=FONT_PX_MAX` or not finite (`resolve_font_px` falls
    /// back to the default rather than clamping). Carries the range the reply
    /// names.
    OutOfRange { key: String, range: String },
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

/// Apply a set of settings edits to the CURRENT `aterm.toml` text NON-DESTRUCTIVELY,
/// returning the new text. PURE + UNIT-TESTED — no filesystem — so the edit
/// semantics are proven independently of the overlay that drives them.
///
/// `existing_toml` is the file's current contents (`""` for a missing file). `edits` is
/// a list of `(key, value)`:
///   * `Some(raw)` — SET `key` to `raw`, typed per [`edit_kind`] (`font_px` → a
///     number, [`written_number`]; `scrollback_lines` → integer; `copy_on_select` →
///     bool; the rest → string). An
///     existing key is UPDATED in place: its surrounding formatting/comments survive,
///     INCLUDING the key's own same-line inline comment and `=`-spacing, which live
///     on the replaced value node and are copied across ([`adopt_inline_decor`]);
///     a new key is appended.
///   * `None` — REMOVE `key` (revert to its built-in default). Absent already ⇒ no-op.
///     Its line goes with its same-line comment; the comment lines written ABOVE it
///     stay where they were ([`aterm_toml::edit::DocumentMut::remove_path`]).
///
/// DOTTED KEYS (`"table.key"`, e.g. `matrix_rain.enabled`, or deeper like
/// `sparkle_words.profanity.enabled`): the edit walks the table chain to the
/// leaf — missing tables are created, an existing table is edited in place
/// (comments + sibling keys survive). A removal takes the leaf, then each
/// parent table it left with no entries and no comment of its own
/// ([`aterm_toml::edit::DocumentMut::remove_path_pruning`]): an empty table
/// parses to the same defaults as an absent one, so a reset leaves no bare
/// `[matrix_rain]` header behind, while a table that keeps a comment (above
/// or on its header, or in the lines directly under its last key) stays. A
/// dotted edit REFUSES (fail-closed `Parse` error) when any step of the chain
/// is not a table (e.g. `matrix_rain = 5`): silently clobbering a
/// hand-authored scalar would destroy user data.
///
/// A dotted key (`matrix_rain.enabled`, `sparkle_words.profanity.enabled`) walks
/// the table chain, creating missing tables and editing existing ones in place.
/// Removal takes the leaf, then each parent table it leaves with no entries and
/// no comment of its own ([`aterm_toml::edit::DocumentMut::remove_path_pruning`]):
/// an empty table parses to the same defaults as an absent one, so a reset leaves
/// no bare `[matrix_rain]` header, while a table that keeps a comment stays. A
/// dotted edit refuses with `Parse` when a step of the chain is not a table
/// (`matrix_rain = 5`) rather than clobber a hand-written scalar.
///
/// Every other key, comment and format survives: the edit goes through
/// `aterm-toml`'s format-preserving DOM, not a re-serialize. On an error (the text
/// is not valid TOML, or a value does not parse as its type) the caller leaves the
/// file untouched.
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
            // A key that replaced a retired spelling takes it out of the file: the
            // resolvers still honour the retired one, so a stale
            // `auto_update = false` beside a fresh `enabled = true` would read off.
            for (retired, current) in RETIRED_PACKAGES_SPELLINGS {
                if current == key {
                    let retired: Vec<&str> = retired.split('.').collect();
                    doc.remove_path_pruning(&retired);
                }
            }
            continue;
        }
        match value {
            // Blank/cleared → remove the key (revert to default). Absent ⇒ no-op.
            None => {
                doc.remove(key);
            }
            Some(raw) => {
                let mut item = typed_item(key, raw)?;
                // The old value node carries the key's same-line decor; a bare
                // replace would drop the user's inline comment.
                adopt_inline_decor(doc.get(key), &mut item);
                doc[*key] = item;
            }
        }
    }
    keep_cursor_style_before_off(&cursor_before, &mut doc, edits);
    keep_classic_mono_color(&cursor_before, &mut doc, edits);

    Ok(doc.to_string())
}

/// AN OLD `classic mono` STYLE KEEPS ITS COLOUR when the writer rewrites it.
/// The `classic mono` spellings ([`aterm_effects::cursor_glow::GlowStyle::style_names_classic_mono`])
/// say two things: the style, Classic, and its colour, the cursor's — what
/// Classic with `cursor_trail_color = "cursor"` ([`TRAIL_COLOR_CURSOR`]) says.
/// The style list knows only the first half, so any edit that takes such a
/// spelling out of `cursor_trail_style` (Off over it, whatever wrote the Off,
/// which keeps the style as `classic`; another style; the retired master's
/// fold) writes the second half as the trail colour in the same write — unless
/// the file already names a trail colour that reads (with one, the old
/// spelling drew in that colour, as Classic does; one the loader ignores, a
/// typo, drew the cursor's, so the cursor's takes its place) or the edit names
/// it (Reset All, Undo). So bringing Classic back, from Cursor Styles' "Your
/// style before Off" or by hand, draws it in the cursor's colour again;
/// nothing the spelling said is lost, and the trail colour row shows and
/// resets it.
fn keep_classic_mono_color(
    before: &CursorStyleFacts,
    doc: &mut aterm_toml::edit::DocumentMut,
    edits: &[(&str, Option<String>)],
) {
    let names_mono = |style: Option<&str>| {
        style.is_some_and(aterm_effects::cursor_glow::GlowStyle::style_names_classic_mono)
    };
    if !names_mono(before.style.as_deref())
        || names_mono(CursorStyleFacts::of(doc).style.as_deref())
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

/// What an aterm.toml says about the cursor's style: the style key and the
/// retired master, as written (a master that is not true or false never
/// reads).
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

    /// The style the file draws ([`crate::app_config::drawn_cursor_trail_style`]).
    fn drawn(&self) -> &str {
        crate::app_config::drawn_cursor_trail_style(self.master, self.style.as_deref())
    }
}

/// Whether a drawn style draws a trail: anything but Off (a spelling aterm
/// does not know draws the default).
fn style_draws(style: &str) -> bool {
    crate::app_config::resolve_trail_style(style)
        .style
        .is_some()
}

/// THE STYLE OFF REPLACED STAYS IN THE FILE ([`EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF`]),
/// wherever the edit came from — a Settings row, Serious Mode's exit holding
/// the cursor Off while another effect turns on, `aterm ctl settings set`,
/// the retired master's fold. Only an edit that changed what the file says
/// about the style acts: one that turned a drawn style Off keeps the style
/// that drew (an older file's `cursor_trail = false` held its style off, so
/// that style), by its own name in the style list (an old alias by the style
/// it names, a spelling aterm does not know by the default it drew), so the
/// key holds a value its own check takes; one that makes the style draw again
/// takes the key out, as the style now says what the cursor draws; Off to
/// Off leaves it. An edit that names the key itself is written as it says
/// (Undo, its Reset).
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

/// A write of either successor of the retired [`RETIRED_CURSOR_TRAIL`] takes it
/// out of the file, the other successor pinned to what the file drew where the
/// edits leave it alone and its own default would draw something else: so the
/// file's meaning changes only in the key the person edited. `cursor_trail =
/// false` with Rainbow dog chosen writes the style and `cursor_momentum_glow =
/// false`; with the glow turned on, `cursor_trail_style = "off"`.
fn fold_retired_cursor_trail(
    doc: &mut aterm_toml::edit::DocumentMut,
    edits: &[(&str, Option<String>)],
) {
    let successors = [EDIT_CURSOR_TRAIL_STYLE, EDIT_CURSOR_MOMENTUM_GLOW];
    if !edits.iter().any(|(key, _)| successors.contains(key)) {
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

/// The TOML item a Settings row writes for `key` from `raw` (the same
/// conversion as [`apply_prefs_edits`]).
pub(crate) fn typed_toml_item(
    key: &str,
    raw: &str,
) -> Result<aterm_toml::edit::Item, PrefsEditError> {
    typed_item(key, raw)
}

/// THE VALUE THAT MEANS UNSET: the raw value that resolves exactly as
/// `field.key` absent from aterm.toml does, read off `field` as
/// [`editable_fields`] builds it from a document WITHOUT the key — so a
/// default that follows other settings (the bloom size follows the saved
/// style; a switch seeded by its resolver) is the one that document resolves
/// to. A switch's is its resolved state; a choice's, the option its unset
/// words name; a number's, the one its placeholder marks "(default)".
///
/// `None` where only absence gives the unset behaviour: free text whose unset
/// words describe a behaviour rather than spell a value (a shell, "Automatic"
/// font families, "Built-in sprite"), a theme ("Follow system"), a size the
/// platform chooses ("13 (automatic)": an explicit size turns auto-sizing
/// off), and the `[ai]` keys, whose absence follows the fleet document
/// ([`crate::app_config::AiPolicy`]) — a value that matches the fleet today
/// still pins this machine when the fleet changes.
pub(crate) fn unset_literal(field: &EditField) -> Option<String> {
    if field.key == "ai" || field.key.starts_with("ai.") {
        return None;
    }
    let placeholder = field.placeholder.trim();
    let marked = placeholder.strip_suffix(" (default)").map(str::trim);
    match field.kind {
        EditKind::Bool => field.seed.clone(),
        EditKind::Enum { options } => {
            // "auto (default)", or a choice's own note: "auto (follows
            // live macOS Reduce Motion)", "off (default font)", or the bare
            // option ("cruise").
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

/// Whether `value` is `field`'s [`unset_literal`]: compared as the kind reads
/// it — a switch as a boolean, a choice case-insensitively, a number
/// numerically (an older aterm.toml holds `12.0` where a slider sends `12`),
/// text exactly.
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
        EditKind::Enum { .. } => value.eq_ignore_ascii_case(literal),
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

/// THE ONE "A VALUE EQUAL TO ITS DEFAULT REMOVES THE KEY" RULE, for every
/// writer: for each edit of `edits` (as [`apply_prefs_edits`] takes them)
/// whether its value is its key's [`unset_literal`] in `existing_toml` with
/// the patch's other edits applied and this key removed — a value that
/// changes nothing a person or the runtime can see, so the patch writes a
/// removal instead and Modified, the Reset button and the "Changed" mark
/// keep meaning "differs from the default". A key no Settings row edits, a
/// removal, and a document or value the writer refuses are never elided, nor
/// is a `display_font` over a display layer ([`display_layer_under`]):
/// there `off` is what keeps that layer's faces from drawing, not the
/// default. `themes` resolves the theme a config names.
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
            // Under a theme that sets this type key, aterm's own default is
            // what keeps the theme's value off (Keep colours only's pin).
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

/// Whether the theme `cfg` names sets the typography key `key`, on either
/// side of a light/dark pair: a value of `key` equal to aterm's default is
/// then a choice over the theme's, never the same as no value.
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

/// Whether an unset `display_font` in `cfg` still draws display faces: those
/// a `display:` font family names — the config's own, else (where it sets
/// no family) its theme's on either side of a light/dark pair.
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

/// What `key` reads as in `cfg`, in the words its Settings row shows: the
/// greyed placeholder (the effective value, "implicit (default)", "Follow
/// system"), or a switch's resolved state. `None` for a key no row edits.
pub(crate) fn effective_value_label(cfg: &Config, key: &str) -> Option<String> {
    let field = editable_fields(cfg)
        .into_iter()
        .find(|field| field.key == key)?;
    if field.placeholder.trim().is_empty() {
        field.seed
    } else {
        Some(field.placeholder)
    }
}

/// The unset value of a free-text row whose resolver reads one literal
/// spelling as its default ([`unset_literal`]); the placeholder the row shows
/// is built from the same constant.
fn text_unset_literal(key: &str) -> Option<&'static str> {
    match key {
        "matrix_rain.hue" => Some(MATRIX_RAIN_HUE_UNSET),
        _ => None,
    }
}

/// `matrix_rain.hue`'s default, the spelling `Config::matrix_rain_params`
/// reads as the classic green.
const MATRIX_RAIN_HUE_UNSET: &str = "matrix";

/// `existing` with `key` set to `item` where TOML reads it: a top-level key
/// among the root keys, ahead of every table; a dotted key in its table,
/// created where missing. Formatting and comments elsewhere are kept.
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

/// Copy the SAME-LINE decor of the value being replaced — the whitespace run
/// between `=` and the value, and the trailing inline `# comment` — onto its
/// replacement. `aterm-toml` stores that decor ON THE VALUE NODE, so replacing the
/// `Item` wholesale (both the `doc[key] = …` top-level path and the dotted-leaf
/// `TableLike::insert`) discards it: `font_px = 12.0  # cozy` edited to `18` must
/// serialize as `font_px = 18  # cozy`, never lose the annotation. (Full-line
/// comments ABOVE the key ride the Key's decor and survive an item replacement on
/// their own.) A no-op when either side isn't a plain value — a fresh key has no
/// decor to inherit, and a table squatting on the name is refused upstream.
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

/// SET a nested scalar leaf at dotted path `parts`, creating missing
/// intermediate tables (implicit, so an intermediate created for `[a.b]`-style
/// nesting doesn't print a spurious empty `[a]` header; a table that gains
/// scalar children prints its header as usual). NON-DESTRUCTIVE like the
/// top-level path: an existing table's other keys, comments, and formatting
/// survive because the walk edits the format-preserving DOM in place. Works
/// through BOTH table forms — `[update]` header tables and `update = { … }` inline
/// tables (`as_table_like_mut` spans both). Refuses (a [`PrefsEditError::Parse`],
/// file untouched) when an intermediate exists as a NON-table value (`update = 5`)
/// or when the leaf itself is currently a table — overwriting either would
/// destroy user structure this editor has no business rewriting.
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
    // Same-line decor survives a leaf replacement, as on the top-level path.
    adopt_inline_decor(table.get(leaf), &mut item);
    table.insert(leaf, item);
    Ok(())
}

/// The writable domain of an [`EditKind::Integer`] key: normally the
/// serde-representable range of the key's `Config` field type, not the resolver's
/// clamp ([`range_of`] mirrors those for sliders; an in-domain value beyond a
/// clamp saves and clamps at load). Tab-status controls take the narrower
/// resolver range, so a saved value is the value the classifier uses.
/// [`typed_item`] rejects a write outside it. Past a tab-status bound the
/// resolver would replace the value; past a serde bound serde would reject the
/// whole file, so the live reload would keep the old config while the Save
/// looked fine, and the next launch would fall back to `Config::default()`.
///
/// The catch-all arm is the `u64`/`usize` shape (`0..=i64::MAX`, TOML's integer
/// ceiling), so a future signed key that forgets its arm fails loud at Save
/// rather than corrupting the file. `integer_keys_enforce_their_serde_domain`
/// re-parses every registered Integer key's boundaries through the real serde
/// model, so an arm cannot drift from its field type.
fn integer_domain(key: &str) -> std::ops::RangeInclusive<i64> {
    match key {
        // Tab status: the writer enforces the resolver's domain, so a saved value is
        // exactly what the next reload classifies with.
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
        // `Option<u16>` fields: the window grid + tab-strip sizes.
        EDIT_COLUMNS | EDIT_LINES | EDIT_TAB_STRIP_ROWS => 0..=i64::from(u16::MAX),
        // `Option<u32>` fields.
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
        // `Option<i8>`: the one narrow SIGNED leaf.
        "sparkle_words.profanity.jitter" => i64::from(i8::MIN)..=i64::from(i8::MAX),
        // `Option<i64>`: the full-width signed typography nudges.
        EDIT_ADJUST_BASELINE | EDIT_ADJUST_UNDERLINE_POSITION | EDIT_ADJUST_UNDERLINE_THICKNESS => {
            i64::MIN..=i64::MAX
        }
        // `Option<u64>`/`Option<usize>` (scrollback_lines, cursor_trail_ms,
        // cursor_trail_length, sparkle anim_ms, matrix_rain
        // mutation_ms/idle_secs/seed) + the fail-safe default for future keys
        // (see above).
        _ => 0..=i64::MAX,
    }
}

/// Would the writer take `raw` for `key`? Its own check ([`typed_item`]), for
/// a caller that refuses a value before it reaches the file.
pub(crate) fn check_edit_value(key: &str, raw: &str) -> Result<(), PrefsEditError> {
    typed_item(key, raw).map(|_| ())
}

/// What a whole number for Integer `key` must be, worded for `raw`, a value
/// the writer refused ([`integer_domain`]): the end it passed, or, when it is
/// not a whole number at all, the whole numbers the key takes. The words
/// follow "must be": the Settings row and the control socket's refusal say
/// the same thing. Its numbers read as every other surface reads them
/// ([`integer_reading`]): plain digits, as the field takes them back, with
/// the key's unit.
///
/// Classified by spelling, then by sign: a whole number is what the writer's
/// parse admits (`[+-]?digits`, or a decimal spelling of one such as `5000.0`,
/// [`whole_number`]), so digits too long for `i64` are still a whole number,
/// past the end their sign points at (not "not a number").
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

/// The largest magnitude below which every whole `f64` is exactly an
/// integer (2^53): a whole number spelled as a decimal is taken as that
/// integer only inside it.
const EXACT_WHOLE_F64: f64 = 9_007_199_254_740_992.0;

/// `value` as the integer it is exactly, when it is a whole number inside
/// [`EXACT_WHOLE_F64`] (where the conversion loses nothing).
fn exact_whole(value: f64) -> Option<i64> {
    (value.is_finite() && value.fract() == 0.0 && value.abs() <= EXACT_WHOLE_F64)
        .then_some(value as i64)
}

/// The exact integer spelled by raw decimal or exponent text. Parse the
/// digits before any floating-point conversion: rounding must not turn a
/// fraction into an integer, underflow into zero, or change a large value.
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
    // An exponent beyond i64 can only make nonzero input too large or
    // fractional; a zero coefficient remains exactly zero either way.
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

/// THE ONE SPELLING OF A NUMBER A FRACTIONAL ([`EditKind::Float`]) SETTING
/// WRITES: the shortest decimal that is exactly `value`, with no fraction
/// when it has none — `window_padding = 20`, `font_px = 15`, `font_px =
/// 13.5`, `line_height = 1.15`, never `20.0`. [`typed_item`] is where every
/// writer meets (a Settings slider, stepper or field, Top Settings, `aterm
/// ctl settings set`, Undo writing a value back), and Manual adds a key at
/// its default through this too
/// ([`crate::native_config_language::config_key_default_literal`]).
///
/// A whole value is a TOML integer, which every reader of these keys takes:
/// they deserialize as `f32`, which serde fills from an integer as from a
/// float, and Manual checks a number of either type. A file that still says
/// `20.0` reads exactly as before; a value it already holds is not rewritten
/// (`VersionedConfigService::patch`). A whole value too large to be an exact
/// integer keeps the float spelling; a value that is not finite never gets
/// here ([`typed_item`] refuses it).
pub(crate) fn written_number(value: f64) -> aterm_toml::edit::Value {
    exact_whole(value).map_or_else(
        || aterm_toml::edit::Value::from(value),
        aterm_toml::edit::Value::from,
    )
}

/// Whether the resolver admits a `font_px` value: finite and inside
/// `FONT_PX_MIN..=FONT_PX_MAX`, the predicate `app_config::resolve_font_px_with`
/// filters each source by.
fn font_px_admissible(px: f64) -> bool {
    px.is_finite() && (f64::from(crate::FONT_PX_MIN)..=f64::from(crate::FONT_PX_MAX)).contains(&px)
}

/// The admissible `font_px` range as the reply spells it (`6..=200`).
fn font_px_range() -> String {
    format!("{}..={}", crate::FONT_PX_MIN, crate::FONT_PX_MAX)
}

/// `raw` as a structured TOML value: an array or inline table as written,
/// or the body of a header table (`12 = "#88c0d0"` lines, the config
/// service's spelling of a `[palette_overrides]` section) as an inline table.
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
    // A comment is a valid empty document, but is not a table value. The
    // config service projects an authored empty table as `{}` (and older
    // empty header-table projections as an empty string).
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

/// Build the `aterm-toml` item for `key` from its raw control text, typed per
/// [`edit_kind`]. A malformed value, or an integer outside [`integer_domain`], is
/// a [`PrefsEditError::BadValue`]; a `font_px` the resolver would ignore is
/// [`PrefsEditError::OutOfRange`].
fn typed_item(key: &str, raw: &str) -> Result<aterm_toml::edit::Item, PrefsEditError> {
    use aterm_toml::edit::{Item, Value};
    let bad = || PrefsEditError::BadValue {
        key: key.to_string(),
        raw: raw.to_string(),
        expected: None,
    };
    let trimmed = raw.trim();
    // The whole palette slot table, or a list given as its TOML array (the
    // semantic value the config service reports, which Undo writes back after
    // a Save as cleared it): written as the TOML it is, never as a string the
    // reload would refuse.
    let structured = if key == EDIT_PALETTE_OVERRIDES {
        Some(structured_value(trimmed).ok_or_else(bad)?)
    } else if LIST_KEYS.contains(&key) && trimmed.starts_with('[') {
        // A compatibility list member can itself start with a bracket (a
        // path, or a shell argument such as `[working]`). Only an
        // actual TOML array selects structured interpretation; otherwise the
        // original comma-list writer below preserves those literal members.
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
        // Authored empty arrays/tables are valid typed values too. An actual
        // reset uses None; Undo must be able to restore Some(empty collection).
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
    // List-valued keys deserialize as `Vec<String>`, so the comma form becomes a
    // TOML array (a comma-joined string would fail the whole parse). Entries are
    // trimmed and empties dropped; an all-empty value is rejected (clear the row
    // instead); `palette` entries must parse as hex.
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
            // The refusal says what it takes in the row's words ("must be a
            // number"), so the row and `settings set` agree.
            let not_a_number = || PrefsEditError::BadValue {
                key: key.to_string(),
                raw: raw.to_string(),
                expected: Some("a number".to_string()),
            };
            let px = trimmed.parse::<f64>().map_err(|_| not_a_number())?;
            // `font_px` is the one float the resolver IGNORES rather than clamps
            // when it is out of range (`app_config::resolve_font_px_with` falls
            // through to the default), so a write outside that range — a
            // non-finite one included — is a save that changes nothing:
            // refused here, with the range, the way a non-numeric value is
            // refused above.
            if key == EDIT_FONT_PX && !font_px_admissible(px) {
                return Err(PrefsEditError::OutOfRange {
                    key: key.to_string(),
                    range: font_px_range(),
                });
            }
            // Every other fractional setting takes only a finite number too:
            // `nan`, `inf` and a decimal too large for f64 (`1e400`) parse, but
            // no reader can use them (each maps them to its default or carries
            // a NaN into layout), and TOML would hold `nan`/`inf` where
            // Settings shows a number.
            if !px.is_finite() {
                return Err(not_a_number());
            }
            written_number(px)
        }
        // Sign/width enforcement AFTER the numeric parse: `-1` for a `usize`
        // field or `70000` for a `u16` is perfectly parseable i64 AND valid
        // TOML, but the reload's serde model rejects the whole file over it —
        // the exact corruption this function's contract forbids — so an
        // out-of-domain integer dies here as a BadValue, never in the file.
        EditKind::Integer => match whole_number(trimmed) {
            Some(n) if integer_domain(key).contains(&n) => Value::from(n),
            // The refusal names the bound it broke, so `settings set` says
            // what to type instead.
            _ => {
                return Err(PrefsEditError::BadValue {
                    key: key.to_string(),
                    raw: raw.to_string(),
                    expected: Some(integer_requirement(key, trimmed)),
                });
            }
        },
        EditKind::Bool => Value::from(trimmed.parse::<bool>().map_err(|_| bad())?),
        // An enum value must be one of `options` (case-insensitive) and is stored
        // in its canonical spelling; anything else is a BadValue, never a style
        // the reload would silently replace.
        EditKind::Enum { options } => {
            if key == EDIT_CURSOR_TRAIL_STYLE || key == EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF {
                // Every spelling the loader accepts is accepted here too, and
                // written as its canonical name: an alias (`rainbow kitty`,
                // `lightning`, `nyan rainbow`) as the row it resolves to.
                // Released configs carry aliases, so `settings set` and a
                // Settings re-commit must not refuse them; a typo still dies
                // here as a BadValue. The one alias that draws differently
                // from its row is the old `classic mono` face: it is written
                // as `classic mono`, so the saved file draws what was asked
                // instead of switching to the spectrum.
                let canonical = cursor_trail_style_canonical(trimmed).ok_or_else(bad)?;
                if GlowStyle::style_names_classic_mono(trimmed) {
                    Value::from("classic mono")
                } else {
                    Value::from(canonical)
                }
            } else if key == EDIT_DISPLAY_FONT && trimmed.contains('+') {
                // A mix: 2..=3 distinct bundled ids joined by `+`
                // ("pixel+engraved"), from the Text & Fonts toggles. Each part
                // must be a bundled id (never "off"); parts are canonicalized, so
                // a legacy spelling is rewritten on save.
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
                    return Err(bad());
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
                    .ok_or_else(bad)?;
                Value::from(*canon)
            }
        }
        // Strings keep the user's text verbatim (trimmed of surrounding whitespace) —
        // a multi-word family round-trips unchanged. `Theme` is written the same way (a
        // free string) so the `dark:…,light:…` split form survives, whether the Settings
        // Follow-system pair editor wrote it or it was typed by hand.
        // `[harness] land_at` is a time the supervisor's reader takes
        // (`LandAt::parse`): anything else is refused at Save rather than
        // written as a value it would keep the default over.
        EditKind::Text
            if key == EDIT_HARNESS_LAND_AT
                && aterm_agent::supervise::LandAt::parse(trimmed).is_none() =>
        {
            return Err(bad());
        }
        EditKind::Text | EditKind::Theme => Value::from(trimmed),
        // A colour must parse as RRGGBB/#RRGGBB ([`color_value_is_valid`]);
        // reject a malformed value at Save rather than write a string the
        // config loader would silently ignore. The trail colour's word for the
        // cursor's colour is written as spelled here, whatever its case.
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

/// One editable field of Settings: a `label`, the `Config`/TOML `key`, its
/// [`EditKind`] (the widget and the written type), the configured value
/// (`seed`) and the effective value (`placeholder`).
///
/// `seed` is `None` for an unset key, so the control starts blank and an
/// untouched Save does not materialise the default; clearing a control removes
/// the key. A Bool seeds its resolved state so the checkbox starts right. The
/// `placeholder` (the configured value, or the default rendered explicitly, e.g.
/// `block (default)`) tells the user what is in effect.
pub(crate) struct EditField {
    /// The on-screen row label.
    pub(crate) label: &'static str,
    /// The `aterm.toml` / `Config` key this row edits.
    pub(crate) key: &'static str,
    /// How the value is typed when written ([`apply_prefs_edits`]).
    pub(crate) kind: EditKind,
    /// The configured raw value to seed the control with (`None` = unset = blank).
    pub(crate) seed: Option<String>,
    /// The effective value, shown greyed when the control is blank.
    pub(crate) placeholder: String,
}

/// A configured string field, trimmed, with whitespace-only as unset (`None`), so
/// the seed and the placeholder agree on what counts as set.
fn configured_str(field: Option<&str>) -> Option<String> {
    field
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The `(seed, placeholder)` for one [`NESTED_LEAVES`] row, under the same law
/// as the top-level rows: a Bool seeds its RESOLVED state (checkbox convention,
/// empty placeholder), everything else seeds the CONFIGURED raw value only with
/// the effective default rendered in the placeholder. Defaults here mirror the
/// consuming resolvers (`sparkle_deco_config` / `Config::matrix_rain_params` /
/// `update_auto_apply`); the conformance test
/// `nested_leaves_have_agreeing_rows_and_serde_round_trip` forces an arm for every
/// registered leaf.
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
        // Seeded through the resolver that owns the default, never a literal
        // `true`, so the switch and the harness never start apart.
        EDIT_HARNESS_ENABLED => boolean(Some(cfg.harness.policy.enabled), true),
        // The supervisor's resolved mode, so a refused value shows the default the
        // reader kept; the full-power default is the placeholder, never a seed (a
        // seed is a written override).
        EDIT_HARNESS_AUTOPILOT => {
            let mode = cfg.harness.policy.autopilot;
            (
                (mode != aterm_agent::supervise::Autopilot::Cruise)
                    .then(|| mode.word().to_string()),
                "cruise".to_string(),
            )
        }
        // The supervisor's own resolved time, so a refused one shows the
        // default it kept (none offered); nothing is the placeholder.
        EDIT_HARNESS_LAND_AT => (
            cfg.harness
                .policy
                .land_at
                .map(aterm_agent::supervise::LandAt::text),
            "none (HH:MM)".to_string(),
        ),
        // The same law: the supervisor's own resolved switch, so a value
        // shows as the reader took it (an off its limit, a refused one no
        // setting) — and the full-power default beside it, which Manual's
        // help names (`default true`) as it does for every key that limits.
        EDIT_HARNESS_ANSWER_QUESTIONS => (
            Some(cfg.harness.policy.answer_questions.to_string()),
            "true".to_string(),
        ),
        // The supervisor's resolved level; the full-power default is the
        // placeholder, never a seed (a seed is a written override).
        EDIT_HARNESS_APPROVE => {
            let level = cfg.harness.policy.approve.word();
            (
                (level != "all").then(|| level.to_string()),
                "all".to_string(),
            )
        }
        // The word the supervisor reads; a refused word is no setting, read as
        // the default.
        EDIT_HARNESS_QUESTION_PRIORITY => {
            let word = cfg.harness.policy.question_priority;
            (
                (word != aterm_agent::supervise::QuestionPriority::Recommended)
                    .then(|| word.word().to_string()),
                "recommended (default)".to_string(),
            )
        }
        // The agent's own table, apart from the master switch above it.
        k if let Some(agent) = agent_switch(k) => {
            boolean(Some(cfg.harness_agent_enabled(agent)), true)
        }
        // [ai]: seeded from the AI layer's resolved value, the fleet document
        // included (`AiPolicy::settings`), so a choice the fleet makes is the row's
        // state; Settings ▸ Harness ▸ AI CLIs says where it came from, and Reset
        // removes only this machine's key.
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
        // Resolved through the one resolver that owns the default, so the switch
        // and the engine never start in different positions.
        EDIT_OUTPUT_STREAK_SOUND => boolean(Some(cfg.output_streak_sound_or_default()), true),
        // PRISM WAKE's switch, from the resolver that owns its platform-split
        // default, so the row starts where the engine does.
        EDIT_OUTPUT_STREAK_ENABLED => boolean(
            Some(cfg.output_streak_enabled_or_default()),
            crate::app_config::DEFAULT_DECORATIVE_EFFECTS,
        ),
        // The presence surfaces seed their RESOLVED state through the resolvers
        // that own the default (both ON), the streak pip's rule.
        EDIT_PRESENCE_RIM => boolean(Some(cfg.presence_rim_enabled()), true),
        // Unreachable for registered leaves: the conformance test fails any
        // entry that lands here.
        _ => (None, String::new()),
    }
}

/// A settings section, the header controls are grouped under. Derived from each
/// row's key by [`section_of`], so a new control needs a `section_of` arm, not a
/// per-row field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    Appearance,
    Cursor,
    /// The Cursor Styles page's keys: the cursor's style (the page's list)
    /// and the tuning a style reads. The page draws its own rows.
    CursorStyles,
    Typography,
    /// Window sizing, tabs, tab status, and chrome.
    Window,
    /// Keyboard & clipboard behavior (copy-on-select, paste safety, Option-as-Meta,
    /// predictive echo).
    Input,
    Terminal,
    Security,
    /// The `[packages]` toolchain-manager switches. The special Packages page
    /// renders these rows itself and no registry page owns the section, so it
    /// surfaces through Search and the Modified review only.
    Packages,
    /// Settings ▸ Harness (`SettingsRoute::Harness`; designs
    /// `docs/DESIGN-aterm-wrapper-2026-09-17.md` §4.6.2 and
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §4). Its rows are five
    /// `[harness]` keys (the master switch `harness.enabled`, the default Autopilot
    /// `harness.autopilot`, the approval level `harness.approve`, the question
    /// answer `harness.answer_questions` and its `harness.question_priority`) and
    /// one `harness.<agent>.enabled` switch per supervisable agent; the AI CLIs
    /// sub-page's `ai.*` rows route here too. The finer `[harness]` keys, each able
    /// only to limit, are written in Manual, where the config language lists them
    /// with their full-power defaults; the master switch's note names every limit
    /// the table writes. The route draws its own page (`native_settings_harness.rs`),
    /// these rows in its SUPERVISION card.
    Harness,
    /// aterm's own updater switches (`[update]`). Settings ▸ Software Update draws
    /// these rows itself, so no ordinary registry page owns this section; Search
    /// and Modified find them.
    Updates,
    /// Every key that changes what aterm sounds like ([`SOUND_MENU_KEYS`]).
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
        // aterm's own updater before the ALab tools, as the sidebar has them:
        // "automatic updates" finds aterm's switches first.
        Section::Updates,
        Section::Packages,
        Section::Harness,
    ];

    /// The header label shown above the section's controls.
    pub(crate) fn label(self) -> &'static str {
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

/// The section a control belongs to, by its key. The single source for grouping — keep an
/// arm here for every key in [`editable_fields`] (unmatched keys fall to `Terminal`).
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
    // Nested tables route by PREFIX (one arm per table, not per leaf): the
    // updater's switches are Software Update's; the two costume tables are
    // screen decorations (Appearance).
    if key.starts_with("update.") {
        return Section::Updates;
    }
    // The supervisor's rows answer to the Harness page and nothing else, and so
    // do the AI CLIs' rows: Settings ▸ Harness ▸ AI CLIs, its sub-page, draws
    // them.
    if key.starts_with("harness.") || key.starts_with("ai.") {
        return Section::Harness;
    }
    // Every audible knob answers to one page. The curse bonk (a
    // `[sparkle_words.profanity]` leaf) and PRISM WAKE's pip (an
    // `[output_streak]` leaf) route here ahead of the decorative-table prefix
    // rule below.
    if SOUND_MENU_KEYS.contains(&key) {
        return Section::Sound;
    }
    if key.starts_with("sparkle_words.")
        || key.starts_with("matrix_rain.")
        || key.starts_with("output_streak.")
    {
        return Section::Appearance;
    }
    // The needs border and its pill are WINDOW CHROME — the frame around the
    // grid and the titlebar's pill — so they sit with the tab strip and the
    // tab-status badges, beside the other things a window says about its
    // sessions.
    if key.starts_with("presence.") {
        return Section::Window;
    }
    // THE CURSOR STYLES PAGE: the style list and the tuning a style reads
    // (the page shows the selected style's own knobs, HDR glow and SDR glow
    // boost among them). `cursor_nyan_sprite` is Manual-only; it rides this
    // section for Search. The cursor's shape is the cursor's own, on Cursor
    // & Motion beside its blink.
    if matches!(key, EDIT_CURSOR_TRAIL_STYLE | EDIT_CURSOR_NYAN_SPRITE)
        || CURSOR_STYLE_TUNING_KEYS.contains(&key)
    {
        return Section::CursorStyles;
    }
    match key {
        EDIT_THEME | EDIT_WINDOW_THEME | EDIT_WINDOW_COLORSPACE | EDIT_PALETTE_GENERATE => {
            Section::Appearance
        }
        // W5/SGR text-appearance knobs sit beside Theme + Colors: they all answer
        // "how does color behave on my screen", so Appearance is their matched tab.
        EDIT_SELECTION_INACTIVE
        | EDIT_MINIMUM_CONTRAST
        | EDIT_BOLD_IS_BRIGHT
        | EDIT_FAINT_OPACITY => Section::Appearance,
        // The indexed palette, window glass and wallpaper are colour behaviour
        // too.
        EDIT_PALETTE
        | EDIT_BACKGROUND_OPACITY
        | EDIT_BACKGROUND_MATERIAL
        | EDIT_WALLPAPER
        | EDIT_WALLPAPER_DIM
        | EDIT_WALLPAPER_TEXT_TINT => Section::Appearance,
        // Window sizing + chrome: the initial grid, the tab strip, the version
        // pill, the selected tab and split pane marks, the interior padding,
        // and what a fresh launch reopens.
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
        // Tab-status controls live alongside the tab/window chrome they mark.
        EDIT_TAB_STATUS
        | EDIT_TAB_STATUS_QUIET_AFTER_MS
        | EDIT_TAB_STATUS_DWELL_MS
        | EDIT_TAB_STATUS_BADGE
        | EDIT_TAB_CONNECTION_BADGE => Section::Window,
        // Keyboard & clipboard behavior — how what you type and copy is handled.
        EDIT_COPY_ON_SELECT
        | EDIT_CONFIRM_MULTILINE_PASTE
        | EDIT_OPTION_AS_META
        | EDIT_PREDICTIVE_ECHO
        | EDIT_PREDICTIVE_ECHO_EDITING => Section::Input,
        // Terminal ▸ System: the launch-time renderer choice, the opt-in
        // replay recorder and the focused shell's priority — how the terminal
        // itself runs, under its Shell.
        EDIT_FOCUS_BOOST | EDIT_GPU | EDIT_TEMPORAL_RECORDING => Section::Terminal,
        // aterm's own desktop notifications ride the Security page beside the
        // programs' (`allow_notifications`): one page answers "who may put a
        // notification on my desktop".
        EDIT_DESKTOP_ALERTS => Section::Security,
        // The [packages] switches render on the special Packages page; this
        // section keeps them findable without an ordinary page. The reroute
        // announcement rides along: atpkg lays and reads the rerouted names.
        EDIT_PACKAGES_ENABLED | EDIT_PACKAGES_AUTO_INSTALL | EDIT_REROUTE_ANNOUNCE => {
            Section::Packages
        }
        // The [machine] host setting lives on the Security page, beside the
        // permissions it resembles: a switch about what this Mac lets happen to it,
        // with the measured state shown under it.
        EDIT_MACHINE_UNIVERSAL_CONTROL => Section::Security,
        EDIT_CURSOR_STYLE
        | EDIT_CURSOR_BLINK
        | EDIT_CURSOR_MOMENTUM_GLOW
        | EDIT_MOTION
        | EDIT_SERIOUS_MODE
        | EDIT_LOAD_ADAPTIVE_MOTION => Section::Cursor,
        // The shell program + argv are terminal-session settings.
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

/// The group box a control belongs to inside its [`Section`] pane: `(caption,
/// order)`. The caption is the uppercase header above the rounded box; `order`
/// sorts groups top to bottom (fields keep their [`editable_fields`] order
/// within a group). An ungrouped key goes in a trailing per-section "General"
/// box.
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
    // The Cursor Styles page draws its own rows; these captions group its keys
    // in Search and Modified.
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
    // Nested tables group by PREFIX — one box per table, keeping their (many)
    // leaves out of the per-section "General" catch-all.
    if key.starts_with("update.") {
        return ("Automatic updates", 3);
    }
    // The Harness page draws its own rows, in its SUPERVISION card: Search
    // and Modified name the same card, with the page's name in it, since
    // neither shows the page around it.
    if key.starts_with("harness.") {
        return ("Harness supervision", 0);
    }
    // The AI CLIs page's rows: one box, the page's own name.
    if key.starts_with("ai.") {
        return ("AI CLIs", 1);
    }
    if key.starts_with("sparkle_words.") {
        return ("Sparkle words", 4);
    }
    // Appearance's effects: the Matrix rain and output streak box.
    if key.starts_with("matrix_rain.") || key.starts_with("output_streak.") {
        return ("Effects", 5);
    }
    // Window › the needs border, beside Tab Status (both answer "what does
    // this window say about its sessions").
    if key.starts_with("presence.") {
        return ("Presence", 2);
    }
    match key {
        EDIT_THEME | EDIT_WINDOW_THEME | EDIT_WINDOW_COLORSPACE | EDIT_PALETTE_GENERATE => {
            ("Theme", 0)
        }
        // The indexed ANSI palette edits beside the individual colour wells.
        EDIT_PALETTE => ("Colors", 1),
        // Appearance › how color behaves (contrast floor, unfocused dimming, SGR
        // brightness/faintness) — beside Theme + Colors, its matched tab.
        EDIT_MINIMUM_CONTRAST
        | EDIT_SELECTION_INACTIVE
        | EDIT_BOLD_IS_BRIGHT
        | EDIT_FAINT_OPACITY => ("Text & Contrast", 2),
        // The window behind the text: glass (opacity and vibrancy material).
        EDIT_BACKGROUND_OPACITY | EDIT_BACKGROUND_MATERIAL => ("Background", 3),
        // The terminal-tab backdrop image, its legibility dim, and the
        // backdrop-hue glyph tint.
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
        // Typography: the face and its size first, then how glyphs are drawn,
        // then the per-style and fallback families few people change.
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
        // Window › sizing, tab status, interior padding, chrome, then session.
        EDIT_COLUMNS | EDIT_LINES | EDIT_TAB_STRIP_ROWS => ("Size", 0),
        EDIT_TAB_STATUS
        | EDIT_TAB_STATUS_QUIET_AFTER_MS
        | EDIT_TAB_STATUS_DWELL_MS
        | EDIT_TAB_STATUS_BADGE
        | EDIT_TAB_CONNECTION_BADGE => ("Tab Status", 2),
        EDIT_WINDOW_PADDING | EDIT_WINDOW_PADDING_TOP => ("Padding", 2),
        // How the window marks where you are: the selected tab, the focused
        // split pane, and the chrome colours they are drawn in (the accent, the
        // dividers, find's highlight).
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
        // Input › clipboard (copy and paste) then keyboard.
        EDIT_COPY_ON_SELECT | EDIT_CONFIRM_MULTILINE_PASTE => ("Clipboard", 0),
        EDIT_OPTION_AS_META | EDIT_PREDICTIVE_ECHO | EDIT_PREDICTIVE_ECHO_EDITING => {
            ("Keyboard", 1)
        }
        // Terminal › System: the renderer, replay and the focused shell's
        // priority, after the Shell.
        EDIT_FOCUS_BOOST | EDIT_GPU | EDIT_TEMPORAL_RECORDING => ("System", 3),
        // Security › aterm's own desktop notifications, under the permissions.
        EDIT_DESKTOP_ALERTS => ("Notifications", 2),
        // Packages › the ALab tools switches ride together.
        EDIT_PACKAGES_ENABLED | EDIT_PACKAGES_AUTO_INSTALL | EDIT_REROUTE_ANNOUNCE => {
            ("ALab Tools", 0)
        }
        EDIT_SCROLLBACK | EDIT_SEARCH_HISTORY_LINES => ("Scrollback", 0),
        EDIT_BIDI | EDIT_AMBIGUOUS_WIDTH => ("Text direction & width", 1),
        // Terminal › which program runs in the pane.
        EDIT_SHELL | EDIT_SHELL_ARGS => ("Shell", 2),
        _ => ("General", u8::MAX),
    }
}

/// The Sound page's master switch and volume notes: the one fact their rows
/// cannot state. Every row but [`EDIT_BELL_SOUND`] is a synth voice under
/// `trail_sounds`, scaled by `trail_sound_volume` (the voice table,
/// `aterm_effects::sound_policy::voice_gain`); the bell is the system alert
/// sound (`lib.rs::on_bell`), which reads neither. Each note names what it
/// covers, so it reads right under its row alone (a search hit, Modified).
/// Every other caveat is a per-row disclosure
/// (`native_settings::setting_effect_projection`).
pub(crate) const TRAIL_SOUNDS_NOTE: &str = "Off silences every aterm sound but the bell.";
/// [`TRAIL_SOUNDS_NOTE`]'s companion, for the volume's own row.
pub(crate) const TRAIL_SOUND_VOLUME_NOTE: &str = "How loud every aterm sound but the bell plays.";

/// What one Sound voice does, for its own row ([`footnote_of`]). `None` for
/// the master switch and the volume, which say what they cover
/// ([`TRAIL_SOUNDS_NOTE`], [`TRAIL_SOUND_VOLUME_NOTE`]).
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
/// What one program permission's Off refuses, for its own row
/// ([`footnote_of`]): a sentence about that one request, true under its row
/// alone (a search hit, Modified), where a sentence for the whole box would
/// read as the last row's. The switch says whether it is on.
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

/// The consequence note for one setting: what its own row cannot say, drawn
/// inside that row under its name ([`row_notes`]), so a note never sits under
/// another row, on a screen, a search result or a Modified list that lacks its
/// own. `None` for rows that need none — notes explain consequences, they never
/// restate the control (an empty color field already says "Theme default").
/// A note several rows share names what it covers ("that agent's sessions"),
/// never "these": it is drawn once, after the last of them, and may be the
/// only one of them a search or Modified lists.
pub(crate) fn footnote_of(key: &str) -> Option<&'static str> {
    if key == EDIT_BELL_SOUND {
        return Some(BELL_SOUND_NOTE);
    }
    // What each voice sounds like, in plain words, wherever its row shows: a
    // name like "Ambient sound bed" says nothing on its own.
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
        // What sets each toy off, wherever its switch shows: Top Settings,
        // search, Modified.
        "sparkle_words.profanity.enabled" => {
            "Sparkle words: swear words in output burst into sparkles."
        }
        "sparkle_words.feline.enabled" => {
            "Keyword kitties: a little cat peeks out by words like \u{201c}cat\u{201d} and \
             \u{201c}kitty\u{201d}."
        }
        // One note for both rows. Blank is each style's own colour under the
        // colour law (Laser's violet; Beam's, Comet's and Sparkle's hue until
        // a cursor colour is set; else the cursor's, the accent a brighter
        // one); the word is the cursor's colour chosen, which the trail
        // colour's picker offers as Cursor color.
        EDIT_CURSOR_TRAIL_COLOR | EDIT_CURSOR_TRAIL_ACCENT => {
            "Blank: Lightning keeps its violet, and Beam, Comet and Sparkle keep their own \
             color until you set a cursor color; other styles follow the cursor color. \
             Cursor color (or \u{201c}cursor\u{201d}) makes every style follow it."
        }
        EDIT_MINIMUM_CONTRAST => "1.0 leaves colors as the theme draws them.",
        // The one place the translucent contrast floor is stated.
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
        // Its segment says "Auto"; what that follows, in the words its
        // picker used ("Match macOS", "Match terminal").
        EDIT_WINDOW_THEME => {
            if cfg!(target_os = "macos") {
                "Auto matches the macOS appearance, light or dark."
            } else {
                "Auto matches the terminal theme, light or dark."
            }
        }
        // Always animate overrides it; the row says so while it does.
        EDIT_LOAD_ADAPTIVE_MOTION => {
            "Drops decorative effects while drawing falls behind. Scrolling stays smooth."
        }
        // The shimmer is the GPU's refraction pass at present time, which no
        // still raster draws, so the preview shows the fire without it; the
        // row says so rather than let the preview read as the terminal.
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
        // Short on purpose: at 2× text on a 320-wide page it shares the row. The revert
        // lines live in `aterm pkg machine` and `aterm explain-config`.
        EDIT_MACHINE_UNIVERSAL_CONTROL => {
            "Off keeps the pointer on this Mac: it won\u{2019}t move to a nearby Mac or iPad."
        }
        EDIT_DESKTOP_ALERTS => {
            "When off, aterm's alerts stay in the window, the menu bar and Messages."
        }
        // Live, and says so: the package loop re-reads `[packages] enabled` before
        // every pass. The note says only what is true wherever it shows; the Packages
        // page's own note says where Check & Update Now works.
        EDIT_PACKAGES_ENABLED => {
            "Keeps ALab tools and the coding agents aterm manages up to date, starting at once."
        }
        // One install lane: no install ships a copy to install from, so the switch is
        // consent to download. Off, nothing installs that nobody asked for
        // (`PackagesConfig::auto_install`).
        EDIT_PACKAGES_AUTO_INSTALL => {
            "Lets aterm download ALab tools (several GB) at the next check, and new ones as \
             they join. Off installs nothing you did not ask for."
        }
        EDIT_REROUTE_ANNOUNCE => {
            "Prints one line before cargo or rustc runs through the ALab toolchain."
        }
        // The Harness page's SUPERVISION notes: what supervision does, then what
        // each choice means in the words its rows show. One note per key, each
        // whole on its own, so a search or Modified that lists some of the
        // rows says only theirs and never points at rows it does not show.
        EDIT_HARNESS_ENABLED => HARNESS_NOTE,
        k if agent_switch(k).is_some() => HARNESS_AGENT_NOTE,
        EDIT_HARNESS_AUTOPILOT => HARNESS_AUTOPILOT_NOTE,
        EDIT_HARNESS_LAND_AT => HARNESS_LAND_AT_NOTE,
        EDIT_HARNESS_APPROVE => HARNESS_APPROVE_NOTE,
        EDIT_HARNESS_ANSWER_QUESTIONS | EDIT_HARNESS_QUESTION_PRIORITY => HARNESS_QUESTIONS_NOTE,
        // The AI CLIs page's notes: what each choice does, whole on its own.
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
/// What a CLI's own switch does, after the last of them: one note the CLIs
/// share, so it is drawn once.
pub(crate) const AI_USE_NOTE: &str = "Off, aterm never signs that CLI in here, never asks \
     it for a review and says nothing about it.";

/// What supervision does, under the SUPERVISION card's master switch.
pub(crate) const HARNESS_NOTE: &str = "aterm keeps your coding agents working by answering \
     their permission boxes and questions for you.";
/// What an agent's own switch does, after the last of them: one note the
/// agents share, so it is drawn once.
pub(crate) const HARNESS_AGENT_NOTE: &str = "Off, aterm leaves that agent's sessions alone; \
     Supervise coding agents turns supervision off for all of them.";
/// What the default autopilot's modes mean, for a session with none of its
/// own.
pub(crate) const HARNESS_AUTOPILOT_NOTE: &str = "For sessions with no autopilot of their own: \
     Cruise keeps the work going, and nothing the agent says ends it; Land lands all of the work \
     on the remote main branch, then stops; Off leaves the session alone.";
/// What the Land at time does: offered, never armed.
pub(crate) const HARNESS_LAND_AT_NOTE: &str = "A time, HH:MM, that each session's Autopilot \
     menu offers: cruise until then, then land. Nothing changes until you pick it.";
/// What the approval segments All, Safe and None mean.
pub(crate) const HARNESS_APPROVE_NOTE: &str = "All says yes to every permission box, for that \
     request only; Safe says yes only to requests aterm can tell are safe, such as reading a \
     file; None leaves every box to you. aterm answers no Codex box: codex-alab answers its own \
     under these same words.";
/// What the question priorities Recommended, Long-term and Short-term mean.
/// Recommended is the default, which the picker offers as "Use default".
pub(crate) const HARNESS_QUESTIONS_NOTE: &str = "With Answer questions on, Recommended (the \
     default) picks the answer the question marks as recommended; Long-term and Short-term let \
     the agent choose, for lasting quality or for the quickest finish.";

/// A row's note as its box draws it ([`row_notes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowNote<'a> {
    /// The row's own: drawn inside the row, under its name.
    Own(&'a str),
    /// One the row shares with rows above it in its box: drawn once, after
    /// the last of them, as the run's note, never inside that row's own
    /// lines (where it would read as that row's alone).
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

/// The note each of `keys` (a box's rows, top to bottom) draws: its own
/// [`footnote_of`], where no other row of the box has the same one; a note
/// several of them share (the coding agents' switches', the question rows'),
/// once, after the last of them ([`RowNote::Shared`]), so it never repeats
/// down the box and never sits inside one row's lines as if that row's
/// alone. A note only one listed row has is that row's own, wherever the
/// box is (a search hit, Modified).
pub(crate) fn row_notes(keys: &[&str]) -> Vec<Option<RowNote<'static>>> {
    keys.iter()
        .enumerate()
        .map(|(position, key)| {
            let note = footnote_of(key)?;
            let shares = |other: &&str| footnote_of(other) == Some(note);
            if keys[position + 1..].iter().any(shares) {
                None
            } else if keys[..position].iter().any(shares) {
                Some(RowNote::Shared(note))
            } else {
                Some(RowNote::Own(note))
            }
        })
        .collect()
}

/// WHEN A SAVED CHANGE TAKES EFFECT, where that is not at once: one typed
/// value per lifecycle, and one plain phrase for each ([`Self::words`]), so
/// a row's second line, its control's name, the status after a save and
/// Manual all say it in the same words (an exception's further facts are
/// its row's note). A key whose change takes effect when it is saved has no
/// timing ([`application_timing`] is `None`): the norm, which nothing states
/// — supervision, the default Autopilot, Matrix rain (its one exception, a
/// session's View menu override, is its note's) and every other live key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ApplicationTiming {
    /// Read once per process: the next launch (an update's relaunch keeps
    /// the window's current size, as its note says).
    AfterRestart,
    /// Read when a session starts.
    NewSessions,
    /// Session restore records when aterm closes and reads it at launch.
    WhenAtermClosesOrRestarts,
    /// The package tool's pass applies it ([`universal_control_timing`]
    /// for Universal Control).
    NextToolsCheck,
    /// Universal Control with Keep ALab tools up to date off: no package pass
    /// runs, so only the daily apply at a launch carries it
    /// (`atpkg::machine::launch_apply_due`).
    DailyLaunchApply,
    /// The reroute shim reads it on every run.
    NextCargoRun,
    /// New text only: cells already on screen keep their width.
    NewTextOnly,
    /// Off applies at once; on may need a window that can show it.
    EnablingNeedsNewWindow,
    /// A development build's update channel, read at its next update check;
    /// a shipped build reads its compiled one.
    DevelopmentBuildsOnly,
}

impl ApplicationTiming {
    /// The one plain phrase every surface says for this timing.
    pub(crate) fn words(self) -> &'static str {
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

    /// [`Self::words`] as the clause after a verb ("aterm turns it off at the
    /// next ALab tools check"): the phrase without its "Applies".
    pub(crate) fn clause(self) -> &'static str {
        let words = self.words();
        words.strip_prefix("Applies ").unwrap_or(words)
    }

    /// Whether saving under this timing still has an effect at once: new
    /// text takes the width at once; turning HDR glow off takes effect at
    /// once.
    pub(crate) fn has_live_effect(self) -> bool {
        matches!(self, Self::NewTextOnly | Self::EnablingNeedsNewWindow)
    }
}

/// When an authored value becomes effective ([`ApplicationTiming`]), where
/// that is not when it is saved. Keep this beside the complete preference
/// schema so Advanced, Modified, and the Manual language service cannot
/// disagree about a saved value's lifecycle. Universal Control's depends on
/// the saved value and the Keep switch ([`universal_control_timing`]); this
/// is its timing with Keep on, the default.
pub(crate) fn application_timing(key: &str) -> Option<ApplicationTiming> {
    use ApplicationTiming as T;
    Some(match key {
        // NOT `packages.enabled` (Phase 4): the window's package loop re-reads it (the
        // retired `auto_update` folded in) before every pass and every few seconds of a
        // park, so it is live — no timing to disclose. `[update] enabled` is read once per
        // process, so it applies next launch.
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
        // Universal Control too: the package tool applies `[machine]` on its
        // pass. Apply Now, where there is something to apply, sits under the
        // Security row and says what it does, so the phrase never names a
        // button the page may not show.
        EDIT_PACKAGES_AUTO_INSTALL
        | "packages.account"
        | "packages.exclude"
        | "packages.links"
        | EDIT_MACHINE_UNIVERSAL_CONTROL => T::NextToolsCheck,
        "update.owner" | "update.repo" => T::DevelopmentBuildsOnly,
        EDIT_AMBIGUOUS_WIDTH => T::NewTextOnly,
        // Live: the harness host applies supervision, Autopilot, the
        // question answer and each agent's switch on the config reload;
        // Install updates applies at once; Matrix rain is live.
        _ => return None,
    })
}

/// When a saved Universal Control `policy` takes effect, by whether Keep ALab
/// tools up to date is on (`[packages] enabled`, the retired `auto_update`
/// folded in, as `Config::packages_enabled` reads it). Leave writes nothing,
/// and every apply reads aterm.toml fresh, so it takes effect when saved
/// (`None`). Off waits for the package tool's pass, which runs only with Keep
/// on; with it off, only the daily apply at a launch carries it.
pub(crate) fn universal_control_timing(
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

/// Whether saving this key has any immediate effect in the running app: a
/// key with no [`application_timing`] (live), and the mixed timings
/// ([`ApplicationTiming::has_live_effect`]): ambiguous-width policy is live
/// for subsequent input while preserving existing cell geometry; disabling
/// HDR glow gates the next present immediately, while enabling may need a new
/// HDR-capable window.
pub(crate) fn application_has_live_effect(key: &str) -> bool {
    application_timing(key).is_none_or(ApplicationTiming::has_live_effect)
}

/// Ambient precedence documented beside the schema. Active values are resolved
/// by `app_config::active_environment_override`; this text also teaches Manual
/// users why a future launch may not use the TOML value they are editing.
pub(crate) fn environment_precedence(key: &str) -> Option<&'static str> {
    Some(match key {
        EDIT_COLUMNS => "a launch --columns overrides this value",
        EDIT_LINES => "a launch --lines overrides this value",
        EDIT_GPU => "a launch --cpu or --gpu overrides this value",
        EDIT_FONT_PX => "a launch --font-px overrides this value",
        EDIT_FONT_FAMILY => "a launch --font overrides this value",
        EDIT_SHELL => "a launch --shell overrides this value; -e / --command bypasses the shell",
        EDIT_SHELL_ARGS => "a launch -e / --command bypasses shell_args",
        // No `update.*`, `packages.*`, `reroute.*` or `harness.*` key, and
        // no window-theme, tab-strip or typography key, has an environment override
        // (2026-09-23/24, owner: "NOT ENV VARS those are for development"): the
        // setting is the one spelling.
        _ => return None,
    })
}

/// Human label for a security opt-in row (keyed by its `SECURITY_BOOL_KEYS` entry).
pub(crate) fn security_label(key: &str) -> &'static str {
    match key {
        // Plain words on the row; the escape sequences' names (OSC 52, XTWINOPS,
        // OSC 4/21) are search words (`keywords_of`) and Manual's help.
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
        // Its timing (new sessions) rides beside the name on every row, and the
        // compact large-text row paints it in parentheses (`setting_row`).
        EDIT_ALLOW_KITTY_FILE_TRANSFER => "Allow local files for Kitty graphics",
        _ => "Security option",
    }
}

/// Truthful copy for `motion = "auto"` differs with the shipping platform
/// seam: macOS observes Reduce Motion live; Windows samples its animations
/// switch, and Linux GNOME's `enable-animations` through the desktop's
/// Settings portal (`platform::gnome_reduce_motion`), when a window attaches;
/// other platforms have no OS query yet. Returns the Settings note under the
/// Motion row and Manual's help fragment.
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

/// Inclusive numeric bounds and step for a bounded numeric control, so the settings
/// UI draws a slider (and the a11y tree exposes min/max/value). [`range_of`] answers
/// `None` for an unbounded value, edited as a free-form numeric field instead; the
/// `range_of_covers_bounded_numerics_only` test makes every numeric key one or the
/// other.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Range {
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) step: f64,
}

/// THE UNIT A NUMERIC SETTING IS COUNTED IN, as the suffix its number takes
/// (" px", " ms", "%", "×"): the one table every reading of a number states
/// its unit from — the slider, the preview's badge, a status, Undo, Reset,
/// Modified's default, a refusal ([`numeric_reading`]) and Manual's range
/// and default — so no label carries "(px)" and no caller writes a unit by
/// hand. `None`: a bare count (columns, lines, a density).
pub(crate) fn numeric_unit(key: &str) -> Option<&'static str> {
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
        EDIT_CURSOR_TRAIL_LENGTH | EDIT_CURSOR_TRAIL_RADIUS => " cells",
        // Stored 0–100, a percent as written.
        "sparkle_words.profanity.supernova_chance" | "sparkle_words.custom.burst.chance" => "%",
        // Multiples: of the font's own line, of the renderer's bloom.
        EDIT_LINE_HEIGHT | EDIT_CURSOR_TRAIL_BLOOM_RADIUS => "\u{00d7}",
        _ => return None,
    })
}

/// HOW A NUMBER READS: a numeric setting's value as its slider, the
/// preview's badge, a status, Undo, Reset, its default and a refusal say it,
/// from the stored `value` and the number as written (`shown`, plain digits,
/// as the field takes it back): Bloom strength as a percentage of its scale
/// (0.85 is 85%), a 0–1 fraction as a whole percentage, and every other
/// counted value with its unit ([`numeric_unit`]). `None` keeps the bare
/// number.
pub(crate) fn numeric_reading(key: &str, value: f64, shown: &str) -> Option<String> {
    if key == EDIT_CURSOR_TRAIL_BLOOM_STRENGTH {
        return Some(format!("{:.0}%", value * 100.0));
    }
    if let Some(unit) = numeric_unit(key) {
        return Some(format!("{shown}{unit}"));
    }
    range_of(key)
        .is_some_and(|range| range.min == 0.0 && range.max == 1.0)
        .then(|| format!("{:.0}%", value * 100.0))
}

/// [`numeric_reading`] of a whole number, or the number itself.
pub(crate) fn integer_reading(key: &str, value: i64) -> String {
    let shown = value.to_string();
    #[allow(clippy::cast_precision_loss)] // only a fraction's percent reads `value`
    numeric_reading(key, value as f64, &shown).unwrap_or(shown)
}

pub(crate) fn range_of(key: &str) -> Option<Range> {
    let r = |min, max, step| Some(Range { min, max, step });
    match key {
        // The full runtime/CLI domain: a 6..=32 subset would make valid
        // high-DPI and accessibility sizes unauthorable.
        EDIT_FONT_PX => r(6.0, 200.0, 1.0),
        // The resolver clamps 30..=2000 (`cursor_trail_ms_or_default`), so the
        // floor is 30: a 0 would load as 30 (the trail turns off through the
        // style row). 10 ms steps keep 260 (the default), 30 and 2000 on the grid.
        EDIT_CURSOR_TRAIL_MS => r(30.0, 2000.0, 10.0),
        EDIT_CURSOR_TRAIL_LENGTH => r(1.0, 512.0, 1.0),
        EDIT_CURSOR_TRAIL_INTENSITY => r(0.0, 1.0, 0.05),
        EDIT_CURSOR_TRAIL_RADIUS => r(0.0, 2.0, 0.05),
        EDIT_CURSOR_TRAIL_BLOOM_STRENGTH => r(0.0, 3.0, 0.05),
        EDIT_CURSOR_TRAIL_BLOOM_RADIUS => r(0.5, 8.0, 0.1),
        EDIT_CURSOR_GLOW_SDR_BOOST => r(0.0, 1.0, 0.05),
        EDIT_TAB_STRIP_ROWS => r(0.0, 4.0, 1.0),
        // Tab status on the resolver's domain; 50 ms steps (anything finer is
        // below the observation budget).
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
        // Typography sliders: the resolvers' clamp bounds.
        EDIT_LINE_HEIGHT => r(0.8, 2.0, 0.05),
        EDIT_FONT_SIZE_SCALE => r(0.8, 1.25, 0.01),
        EDIT_MINIMUM_CONTRAST => r(1.0, 21.0, 0.5),
        EDIT_FAINT_OPACITY => r(0.0, 1.0, 0.05),
        EDIT_ADJUST_BASELINE => r(-32.0, 32.0, 1.0),
        // Underline nudges: the config's ±32 px clamp.
        EDIT_ADJUST_UNDERLINE_POSITION | EDIT_ADJUST_UNDERLINE_THICKNESS => r(-32.0, 32.0, 1.0),
        // Stem gamma: the renderer's clamp domain (<1 thicker, >1 thinner).
        EDIT_STEM_GAMMA => r(0.30, 3.0, 0.05),
        // Primary `wght`: the renderer's 1..=1000 clamp. Unit steps keep every
        // authored integer (400 included) on the grid.
        EDIT_FONT_WEIGHT => r(1.0, 1000.0, 1.0),
        // Each trail numeric mirrors its resolver's clamp domain, so the slider
        // cannot express a value load would rewrite.
        EDIT_TRAIL_SOUND_VOLUME => r(0.0, 1.0, 0.05),
        // W9 dark-theme nudge (config clamps 0..=300).
        EDIT_FONT_WEIGHT_DARK_NUDGE => r(0.0, 300.0, 10.0),
        // Window glass (resolver clamps 0..=1).
        EDIT_BACKGROUND_OPACITY => r(0.0, 1.0, 0.05),
        // Wallpaper legibility dim (resolver clamps 0..=1).
        EDIT_WALLPAPER_DIM => r(0.0, 1.0, 0.05),
        // Interior padding, logical px (resolver clamps 0..=64); the resolver
        // caps the top override at the base pad, so the slider spans the static
        // domain.
        EDIT_WINDOW_PADDING | EDIT_WINDOW_PADDING_TOP => r(0.0, 64.0, 1.0),
        // Nested sparkle-words numerics — the documented clamp domains of
        // `sparkle_deco_config` / `SparkleInkConfig`.
        "sparkle_words.profanity.supernova_chance" => r(0.0, 100.0, 5.0),
        "sparkle_words.profanity.density" => r(1.0, 12.0, 1.0),
        "sparkle_words.profanity.anim_ms" => r(350.0, 10000.0, 50.0),
        "sparkle_words.profanity.jitter" => r(0.0, 6.0, 1.0),
        "sparkle_words.profanity.intensity" | "sparkle_words.feline.intensity" => r(0.0, 1.0, 0.05),
        "sparkle_words.ink.strength" => r(0.0, 1.0, 0.05),
        "sparkle_words.ink.sweep_ms" => r(350.0, 6000.0, 50.0),
        // Nested matrix-rain numerics — `Config::matrix_rain_params`'s clamp domains.
        "matrix_rain.fps" => r(12.0, 60.0, 1.0),
        "matrix_rain.density" => r(1.0, 12.0, 1.0),
        "matrix_rain.speed" | "matrix_rain.trail" => r(1.0, 10.0, 1.0),
        "matrix_rain.alpha" | "matrix_rain.head_alpha" => r(16.0, 135.0, 1.0),
        "matrix_rain.mutation_ms" => r(80.0, 2000.0, 10.0),
        "matrix_rain.idle_secs" => r(2.0, 120.0, 1.0),
        // scrollback_lines / columns / lines / search_history_lines /
        // matrix_rain.seed are open-ended: free-form numeric fields.
        _ => None,
    }
}

/// Discovery aliases for the Shell row: the shells that run on this platform.
/// The Windows entries are the aliases the PTY resolves by name
/// (`aterm_pty::windows::shell::discover_shell`) plus the two PowerShell
/// spellings its default probes.
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

/// What the Shell field shows when nothing is configured; on Windows it names
/// the by-name aliases, which need no path.
#[cfg(windows)]
const SHELL_PLACEHOLDER: &str =
    "platform default — pwsh, powershell, cmd, bash, wsl, nu, or a path";
#[cfg(not(windows))]
const SHELL_PLACEHOLDER: &str = "platform default";

/// Keep ALab tools up to date's search words: what its card says, the
/// retired spellings a file may still hold, and every coding agent aterm
/// supervises (it keeps them up to date too), read from the agent roster so
/// a new agent is found here with no list to edit.
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

/// Extra search keywords so a fuzzy query finds a control by intent ("dark mode"
/// → `window_theme`, "history" → `scrollback_lines`). The label, key and section
/// name are always in the search corpus; an unknown key adds nothing.
pub(crate) fn keywords_of(key: &str) -> &'static [&'static str] {
    match key {
        // Match macOS follows the system too.
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
        // The theme row is also the Follow system light/dark pair: found by
        // its switch's name and by what people call a dark or light theme.
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
        // "blink" is what a user annoyed by the blinking cursor searches for; the
        // glow is what replaces it while they type.
        EDIT_CURSOR_MOMENTUM_GLOW => &["blink", "glow", "typing", "speed", "momentum", "effect"],
        // The shape is found by its options' names ("beam" is the old
        // spelling of Bar) and by what people call the cursor.
        EDIT_CURSOR_STYLE => &["block", "bar", "beam", "caret", "shape"],
        EDIT_CURSOR_TRAIL_MS
        | EDIT_CURSOR_TRAIL_LENGTH
        | EDIT_CURSOR_TRAIL_INTENSITY
        | EDIT_CURSOR_TRAIL_RADIUS
        | EDIT_CURSOR_TRAIL_RING => &["effect", "motion", "comet", "trail"],
        // The companion picker answers to the cat's own names.
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
        // The riff, found by what a user hears ("song", "loud").
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
        // PRISM WAKE's pip, found by what made it happen or what it sounds like,
        // never by the design's code name.
        EDIT_OUTPUT_STREAK_SOUND => &[
            "streak", "output", "prism", "comet", "rainbow", "pip", "chime", "sound", "sfx",
        ],
        // Likewise the bell: "beep" and "alert" are what the sound is called
        // outside this codebase.
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
        // A retired spelling still in a file finds the switch that reads it.
        EDIT_PACKAGES_ENABLED => packages_keywords(),
        // The two switches of Software Update's Automatic updates card, findable by
        // their names' words and the ones a person may use for them instead.
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
        // The Display faces card's toggles, by the faces' own names.
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
        // The escape sequences' names left the rows' labels, so they are
        // search words here (before the shared Security arm below).
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
        // The presence surfaces: a user hunting them knows the WORDS the band
        // prints ("driven", "hold", "rim", "band") or the feature's name.
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
        // Nested tables share one intent vocabulary per table.
        k if k.starts_with("update.") => &["update", "channel", "github", "release"],
        // The default Autopilot answers to the control's name, its three
        // modes and the owner's own words for them ("infinity mode", "wrap it
        // up mode", "off").
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
        // The landing time answers to the timer's words: when the work should
        // wrap up.
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
        // The approval policy answers to what a person reaches for when the
        // agent keeps stopping on "Do you want to proceed?" — and to the words
        // for turning the answering off.
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
        // What a person reaches for when the agent keeps stopping on a question.
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
        // How a question is answered: "priority", the two policy words, and
        // the words for having the worker decide.
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
        // An agent's switch answers to the words for leaving it alone; its
        // agent's own word is an own name ([`own_name_of`]), never a synonym.
        k if agent_switch(k).is_some() => &["harness", "agent", "supervise", "disable", "off"],
        // The AI CLIs' rows answer to the words a person reaches for to
        // choose among them or turn one away (its own names are its label
        // and key).
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
        // The wrapper's switch answers to the vendor's name as well as to
        // "harness", and to the words for turning it off.
        k if k.starts_with("harness.") => &[
            "harness",
            "wrapper",
            "claude",
            "agent",
            "disable",
            "off",
            "kill switch",
        ],
        // The bonk keys are sparkle-words leaves but SFX in the UI: they carry
        // the sound vocabulary too.
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
        // The two word effects answer to what they react to.
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

/// Build the editable field specs (label/key/kind/seed/placeholder) the Settings
/// overlay renders as widgets, in [`Section`]-grouped row order. PURE + TESTABLE: the
/// seeding logic (which keys start blank vs. populated, the bools' resolved state, and
/// the effective-value placeholder) is unit-tested; the overlay just maps each spec to
/// a widget.
///
/// The control is SEEDED with the CONFIGURED raw value only — an unset key seeds `None`
/// so the control is blank and a Save of an untouched blank field removes nothing
/// (rather than materialising the effective default). The `placeholder` carries the
/// EFFECTIVE value (configured, or the built-in default rendered explicitly) so a blank
/// control still tells the user what is in effect — fixing the all-rows-blank confusion.
/// [`editable_fields`] for a config whose theme names type settings: each
/// font row the config leaves unset and the theme fills reads
/// "<value> (from theme <label>)" (a checkbox shows the theme's state). A row
/// the config sets keeps its own value: the config wins.
pub(crate) fn editable_fields_with(
    cfg: &Config,
    typo: &crate::resolved_typography::ResolvedTypography,
    theme_label: &str,
) -> Vec<EditField> {
    let mut fields = editable_fields(cfg);
    apply_theme_typography(&mut fields, cfg, typo, theme_label);
    fields
}

/// The value a theme's typography gives `key` where `cfg` leaves it unset,
/// as the theme file writes it; `None` when the config sets the key, the
/// theme does not, or `key` is no typography key.
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
        // A theme's `display:` family names display faces, drawn as a layer
        // over the font (`Config::display_faces_request`): the font row's
        // placeholder stays the automatic face's.
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
    // `fallback_fonts` edits as one comma-separated string (the FontList form
    // the loader parses back).
    let fallback_fonts = cfg
        .fallback_fonts
        .as_ref()
        .filter(|l| !l.0.is_empty())
        .map(|l| l.0.join(", "));
    let symbol_font = configured_str(cfg.symbol_font.as_deref());
    let emoji_font = configured_str(cfg.emoji_font.as_deref());
    // The variable-font axes round-trip the same way.
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
    // The old `classic mono` spelling draws Classic in the cursor's colour.
    let classic_in_cursor_color = GlowStyle::style_names_classic_mono(cfg.cursor_trail_style_raw());
    let cursor_nyan_sprite = configured_str(cfg.cursor_nyan_sprite.as_deref());
    let foreground = configured_str(cfg.foreground.as_deref());
    let background = configured_str(cfg.background.as_deref());
    let cursor_color = configured_str(cfg.cursor_color.as_deref());
    let selection_color = configured_str(cfg.selection_color.as_deref());
    let selection_foreground = configured_str(cfg.selection_foreground.as_deref());
    let predictive_echo = configured_str(cfg.predictive_echo.as_deref());
    let mut fields = vec![
        // Family before size, the Typography "Font" box order.
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
            // Unset, the size is the platform's: the slider sits on it.
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
            // The resolved state (default ON: styles are synthesized when no real
            // face exists).
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
            // The resolved state (default ON); a checkbox needs no placeholder.
            seed: Some(cfg.ligatures.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Line height",
            key: EDIT_LINE_HEIGHT,
            kind: EditKind::Float,
            seed: cfg.line_height.map(|v| format!("{v}")),
            // The platform's default (1.1 on Windows, 1.0 elsewhere).
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
                None => "1.0 (default)".to_string(),
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
                // Unset, Windows holds text in theme colours to 4.5:1
                // (`Config::indexed_minimum_contrast_or_default`).
                None if cfg!(windows) => "auto (4.5:1 ANSI)".to_string(),
                None => "1.0 (off)".to_string(),
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
            // The resolved state (default ON).
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
                None => "1.0 (default)".to_string(),
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
            // One comma-joined string, like `fallback_fonts`.
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
            // Unset is Default on every appearance; following the OS is a
            // pair Settings writes when its Follow system switch is turned on.
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
            // Default ON; the checkbox shows the resolved state.
            seed: Some(cfg.cursor_blink.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            // The cursor glows with typing speed and cools when you stop; while
            // warm it does not blink.
            label: "Typing momentum glow",
            key: EDIT_CURSOR_MOMENTUM_GLOW,
            kind: EditKind::Bool,
            seed: Some(cfg.cursor_momentum_glow_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // The Cursor Styles page lists every style; elsewhere the row
            // opens that page.
            label: "Cursor style",
            key: EDIT_CURSOR_TRAIL_STYLE,
            kind: EditKind::Enum {
                options: CURSOR_TRAIL_STYLES,
            },
            // Seeded with what draws: a retired `cursor_trail` in the file
            // folds in (`false` is Off).
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
            // Written by the writer beside an Off, never drawn: no row of its
            // own (Cursor Styles marks the style), a key only in aterm.toml
            // on Modified, with Reset to forget it.
            label: "Cursor style before Off",
            key: EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF,
            // A style by its name in the list, so Manual offers the styles
            // and checks the value as it does the style's.
            kind: EditKind::Enum {
                options: CURSOR_TRAIL_STYLES,
            },
            seed: None,
            placeholder: String::new(),
        },
        EditField {
            // The master switch for every synth sound: the typing notes, the ambient
            // bed, the song, the output streak's note, the build verdict, the bonk and
            // the supervisor chime. Default ON. Only the typing notes follow the trail's
            // light (silent with the Off style, reduced motion or an unfocused window);
            // the other sounds keep their own gates.
            label: "Music effects",
            key: EDIT_TRAIL_SOUNDS,
            kind: EditKind::Bool,
            seed: Some(cfg.trail_sounds_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // Built right after the master switch it scales: rows sort by
            // (group order, build order), so the Sound box reads as a menu.
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
            // The melody leans with the inferred mood of the typed line (an
            // on-device classifier over typed input only, never screen content).
            // Subtle: uncertain or technical lines sound as with it off.
            label: "Tone-adaptive melody",
            key: EDIT_TONE_MELODY,
            kind: EditKind::Bool,
            seed: Some(cfg.tone_melody_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // The ambient bed behind fast typing, default ON; off gates the synth's bed
            // mixer entirely and leaves the other voices untouched.
            label: "Ambient sound bed",
            key: EDIT_TRAIL_SOUND_BED,
            kind: EditKind::Bool,
            seed: Some(cfg.trail_sound_bed_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // Which instrument the keystrokes speak with (see
            // [`TRAIL_SOUND_STYLES`]); picking a voice auditions one keystroke
            // of it (`App::audition_typing_sound`).
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
            // The sing-along riff's own switch (the loudest voice), default ON.
            // Sound only; the celebration's visuals keep running.
            label: "Sing-along riff",
            key: EDIT_TRAIL_SOUND_RIFF,
            kind: EditKind::Bool,
            seed: Some(cfg.trail_sound_riff_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // The BEL beep: an OS alert sound (NSBeep / MessageBeep), not a synth
            // voice, so outside `trail_sound_volume`. Off silences only the beep.
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
                Some(n) => format!("{n} ms"),
                None => "260 ms (default)".to_string(),
            },
        },
        EditField {
            label: "Trail length",
            key: EDIT_CURSOR_TRAIL_LENGTH,
            kind: EditKind::Integer,
            seed: cfg.cursor_trail_length.map(|n| n.to_string()),
            placeholder: match cfg.cursor_trail_length {
                Some(n) => format!("{n} cells"),
                None => "24 cells (default)".to_string(),
            },
        },
        EditField {
            label: "Trail intensity",
            key: EDIT_CURSOR_TRAIL_INTENSITY,
            kind: EditKind::Float,
            seed: cfg.cursor_trail_intensity.map(|v| v.to_string()),
            placeholder: match cfg.cursor_trail_intensity {
                Some(v) => v.to_string(),
                // Unset, the configured style's own brightness
                // (`Config::cursor_trail_intensity_for`): Classic's is the
                // v0.28 release's 0.7.
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
                Some(v) => format!("{v} cells"),
                None => "0.6 cells (default)".to_string(),
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
            // Unset, the trail draws the saved style's own colour where the
            // colour law keeps one (or Classic's shifting colours), else the
            // live cursor colour, as the old `classic mono` spelling draws
            // Classic; `cursor` is that colour chosen.
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
                // The style's own default.
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
            // Sheds decorative effects under sustained render overload, default
            // ON; off (or Motion = full) keeps them regardless. Smooth scrolling
            // and the scroll pill are never shed (`App::effect_policy`).
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
            // The resolved state (default ON).
            seed: Some(cfg.copy_on_select_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Foreground",
            key: EDIT_FOREGROUND,
            kind: EditKind::Color,
            placeholder: foreground
                .clone()
                .unwrap_or_else(|| "Theme default".to_string()),
            seed: foreground,
        },
        EditField {
            label: "Background",
            key: EDIT_BACKGROUND,
            kind: EditKind::Color,
            placeholder: background
                .clone()
                .unwrap_or_else(|| "Theme default".to_string()),
            seed: background,
        },
        EditField {
            label: "Cursor color",
            key: EDIT_CURSOR_COLOR,
            kind: EditKind::Color,
            placeholder: cursor_color
                .clone()
                .unwrap_or_else(|| "Theme default".to_string()),
            seed: cursor_color,
        },
        EditField {
            label: "Selection color",
            key: EDIT_SELECTION_COLOR,
            kind: EditKind::Color,
            placeholder: selection_color
                .clone()
                .unwrap_or_else(|| "Theme default".to_string()),
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
            // Unset follows the system's selected-tab pill: plain words, in
            // the "(default)" form every other row uses, never a config word.
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
            // The optional floating top-right pill (the version is also in the
            // menu bar's About); default OFF.
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
            // The background package lane's switch, seeded resolved (the retired
            // `auto_update` included) and live: the package loop re-reads it before
            // every pass. Update Now / Install still work.
            label: "Keep ALab tools up to date",
            key: EDIT_PACKAGES_ENABLED,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_enabled().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // The install consent, seeded resolved (the retired `seed_install`
            // included); the co-located atpkg reads the same bit.
            label: "Install ALab tools",
            key: EDIT_PACKAGES_AUTO_INSTALL,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_auto_install().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // Seeds the configured raw value (blank when unset), the effective default
            // in the placeholder, like every non-Bool row. Its "This Mac" group names the
            // host, so the label need not.
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
            placeholder: "off".to_string(),
        },
    ];
    // The remaining scalar keys, the list rows and the nested-table leaves, so
    // Manual and `settings set` cover every setting. Same seeding law; list rows
    // seed the comma-joined form their Save re-splits.
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
            // The resolved config state (default ON with CPU fallback); the
            // process-fixed env/CLI overrides are deliberately not folded in.
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
        // (The Sound-box rows and the trail colour, sprite, geometry, bloom,
        // shimmer and glow rows are built in the core list above.)
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
                None => "1.0 (solid)".to_string(),
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
            // The resolved state (default OFF).
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
    // One row per registered nested leaf, from the one `NESTED_LEAVES` registry.
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
    // Permissions (fail-closed: default OFF, except the palette, default ON). One
    // Bool per key, sourced from SECURITY_BOOL_KEYS so `edit_kind` agrees; an
    // absent key seeds that key's own default.
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
    // Group the rows by section last, stably, so build order holds within a
    // section; this is the one ordering the painter, scroll and hit-test see.
    // Cached: `section_of` is a linear scan, and `sort_by_key` would run it twice
    // per comparison.
    fields.sort_by_cached_key(|f| section_of(f.key).order_index());
    fields
}

/// The result of a Preferences save, for the window's status feedback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveOutcome {
    /// The file's content changed and was written; the caller should post a reload.
    Saved,
    /// Nothing actually changed (every edit was a no-op); skip the write + reload.
    Unchanged,
    /// Another disk generation won. The caller must present a conflict rather
    /// than collapsing it into generic validation or I/O failure.
    Conflict {
        expected: crate::native_document_io::ContentFingerprint,
        observed: crate::native_document_io::ObservedFileVersion,
        message: String,
    },
    /// Replacement may already be visible, but its durability/content proof
    /// could not be completed. Reload/reconcile before issuing another write.
    PublishedUnverified {
        stage: crate::native_document_io::AtomicSaveStage,
        observed: Option<crate::native_document_io::ObservedFileVersion>,
        /// Why, in a person's words ([`crate::save_words`]).
        message: String,
    },
    /// The save failed before publication: why, in a person's words
    /// ([`crate::save_words`]).
    Error(String),
}

/// Settings-worker persistence result with the exact generation proof needed
/// to construct a [`ConfigDiskObservation`](crate::native_config_service::ConfigDiskObservation)
/// without reopening `aterm.toml` on the event loop.
#[derive(Clone, Debug)]
pub(crate) struct ConfigSnapshotSaveResult {
    pub(crate) outcome: SaveOutcome,
    /// Present when the save/no-op result itself proves the complete target
    /// generation. Conflicts and pre-publication failures require a fresh
    /// bounded worker observation instead.
    pub(crate) observed: Option<crate::native_document_host::AtomicFileBaseline>,
}

/// Atomically persist a complete snapshot already produced by the versioned
/// native-config service. The service, not this function, performs semantic
/// edits and OCC; this seam only commits its non-destructive TOML projection.
#[cfg(test)]
pub(crate) fn save_prefs_snapshot(
    plan: &crate::native_config_service::ConfigPersistencePlan,
) -> SaveOutcome {
    save_prefs_snapshot_observed(plan).outcome
}

/// Worker-facing form of [`save_prefs_snapshot`] that retains the committed
/// proof. Callers can hand exact bytes + this baseline to the event loop; no
/// completion-side pathname read is needed.
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
        // The log keeps the step's own account; the outcome carries the
        // words a person reads ([`crate::save_words`]).
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

/// The words a save the filesystem refuses says, through the real writer
/// (`amacs_files::save_words` words every step and kind on its own).
#[cfg(all(test, unix))]
mod save_words_tests {
    use crate::native_document_io::{AtomicSaveStage, SaveCause, SaveTouch};
    use crate::save_words::{Failure, Speaker, words};
    use std::io::ErrorKind;
    use std::path::Path;

    /// Save `theme = "Nord"` over `theme = "Default"` in `path` through the
    /// real writer, with `before` run first and `after` once it returns (to
    /// put the permissions back): the outcome, and the file after.
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

    /// A SAVE THE FILESYSTEM REFUSES SAYS WHERE AND WHY, through the real
    /// writer: a config folder the person cannot write gives the folder's
    /// words, at whichever step meets the refusal first (the save lock while
    /// it has never been made, the new file beside aterm.toml once it has);
    /// an aterm.toml the person cannot read gives the file's; a save lock the
    /// person cannot open names the lock to delete. Each leaves aterm.toml as
    /// it was. The same saves used to say "…/aterm.toml save failed at
    /// CreateTemporary (Permission denied (os error 13))", and the last two
    /// "…isn't writable", naming a folder that was.
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
        // The folder, with no lock made yet, then with one.
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
        // The file itself, unreadable.
        let (outcome, after) = save_with(&path, || mode(&path, 0o200), || mode(&path, 0o600));
        let unreadable = expect(AtomicSaveStage::Preflight, SaveTouch::Read);
        assert!(unreadable.contains("isn\u{2019}t readable"), "{unreadable}");
        assert_eq!(
            outcome,
            crate::prefs::SaveOutcome::Error(unreadable.clone())
        );
        assert_eq!(after, "theme = \"Default\"\n");
        // A save lock this user cannot open.
        let (outcome, after) = save_with(&path, || mode(&lock, 0o400), || mode(&lock, 0o600));
        let locked = expect(AtomicSaveStage::Preflight, SaveTouch::Lock);
        assert!(locked.contains("delete "), "{locked}");
        assert_eq!(outcome, crate::prefs::SaveOutcome::Error(locked.clone()));
        assert_eq!(after, "theme = \"Default\"\n");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The trail-style and typing-sound tables against the engine's parser and
/// predicates.
#[cfg(test)]
mod trail_style_tests {
    use super::{CURSOR_TRAIL_STYLES, Config, apply_prefs_edits};
    use crate::cursor_glow::GlowStyle;
    use aterm_effects::cursor_fx::GlowSpelling;
    use aterm_effects::kitty_pet::PetSpecies;

    /// `Some(v)` helper to keep the edit lists terse.
    fn set(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    /// What one spelling draws: the engine style, the companion and the
    /// spelling forks every host derives (`beam` is the additive beam).
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

    /// Every look a spelling drew at origin/main 5e5f7cb17f (its 15-row
    /// `CURSOR_TRAIL_STYLES`, plus the old `classic mono` face its aliases
    /// kept), read off that commit's tables and
    /// `resolve_trail_presentation_from_style`. Frozen: this is history, not
    /// the current table.
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

    /// Every alias at 5e5f7cb17f and the look it drew there (the `classic
    /// mono` spellings drew its one-colour face).
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

    /// What the native host draws for one spelling, and whether it reported
    /// the spelling as unknown.
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

    /// What a page's `set_cursor_glow` derives from the same spelling.
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

    /// NO SPELLING CHANGES WHAT IT DRAWS. Every spelling that loaded at
    /// origin/main 5e5f7cb17f, in any case and padding, still loads without a
    /// warning and resolves natively to exactly the look it had there, and a
    /// web page's reading of the same spelling agrees with the native one.
    /// `lightning` and `rainbow dog` (the Cursor Styles names of two rows) are
    /// new and draw what their rows draw.
    ///
    /// The merge that moved the vocabulary into one engine table
    /// (`aterm_effects::cursor_glow::spellings`) and reordered the list is
    /// presentation only; this is the pin that it is.
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
        // Controls. The fixture tells companions and bodies apart, so the
        // equalities above are not vacuous…
        assert_ne!(before("rainbow kitty flying"), before("rainbow kitty pet"));
        assert_ne!(before("rainbow kitty flat"), before("rainbow kitty pet"));
        // …and an unknown word is still reported natively (and drawn as the
        // default), while a page keeps its documented Lumen fallback.
        let (typo, unknown) = native_look("rainbow kity");
        assert!(unknown);
        assert_eq!(typo, before(super::DEFAULT_CURSOR_TRAIL_STYLE));
        assert_eq!(web_look("rainbow kity").style, Some(GlowStyle::Lumen));
    }

    /// `--validate-config` accepts the aliases with no warning (the Cursor
    /// Styles names among them, and the old `classic mono` face); a retired
    /// `… v2` spelling and a near miss still warn.
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

    /// The unset default resolves to a picker option that draws the full-body pet
    /// (what `app_render::trail_is_kitty_pet` asks) on the rainbow ribbon: the
    /// destination, not just the constant.
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
        // The flying head is one explicit spelling away; `rainbow kitty` and
        // `kitty` draw the pet, the historical aliases the flying head.
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

    /// The typing-sound resolver: canonical spellings resolve to themselves
    /// (case- and whitespace-insensitively), every documented synth alias
    /// projects onto its canonical row, and an unknown spelling is refused —
    /// while the WRITER accepts only canonical spellings (aliases are load-
    /// only here; `cursor_trail_style`'s writer canonicalises them instead)
    /// and the loader (`Config::trail_sound_voice`) accepts both and falls
    /// back to `auto`.
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
            // Aliases are accepted at load…
            let cfg = Config {
                trail_sound_style: Some(alias.to_string()),
                ..Config::default()
            };
            assert_eq!(cfg.trail_sound_voice(), voice, "loader: {alias}");
            // …and rejected by the writer (never offered, never persisted).
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
        // The writer takes every canonical spelling verbatim (case-folded).
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

    /// `Some(v)` helper to keep the edit lists terse.
    fn set(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    /// The dotted `matrix_rain.enabled` key is typed Bool by `edit_kind` —
    /// falling to Text would write a TOML string for a serde `Option<bool>`,
    /// silently corrupting `[matrix_rain]` on reload — and `typed_item`
    /// rejects garbage instead of writing it.
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

    /// Blank = revert: removing the dotted child deletes only that key while
    /// siblings remain. A table the removal leaves bare goes with it, and one
    /// that keeps a comment stays: an empty table parses to the same defaults
    /// as an absent one, so only the comment is worth keeping.
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

        // Absent already ⇒ no-op (mirrors the flat-key contract).
        let out = apply_prefs_edits(
            "font_px = 13.0\n",
            &[(super::EDIT_MATRIX_RAIN_ENABLED, None)],
        )
        .expect("remove-from-absent is a no-op");
        assert_eq!(out, "font_px = 13.0\n");
    }

    /// The Settings row itself: registered exactly ONCE (the NESTED_LEAVES
    /// entry — Appearance ▸ Matrix rain), grouped, searchable, and seeded with
    /// the RESOLVED bool so the switch starts in the right position.
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

    /// PRISM WAKE'S SWITCH IS A ROW: Appearance's Effects box, seeded where the
    /// engine starts (the platform-split default, or the file's `false`), and
    /// written as the `[output_streak]` leaf the resolver reads, so turning
    /// the comet off is a switch, not a hand edit.
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

    /// THE `[harness]` ROWS (design `docs/DESIGN-aterm-wrapper-2026-09-17.md`
    /// §4.6.2; owner, 2026-09-24 and 2026-09-25; the Autopilot design,
    /// 2026-10-15): the master switch, the default Autopilot, the approval
    /// level, the question answer and its priority are ordinary rows of the
    /// Harness section, reachable from Search, the Modified review and
    /// `settings set`, that no environment variable bypasses (2026-09-23), each
    /// applied live (the host follows the reload). SET writes `[harness]`
    /// without touching the rest of the file, the SUPERVISOR's policy reads it
    /// (and only it: a limit is not the kill switch, HOW is never WHETHER), the
    /// row reads it back (a priority's case normalised), and UNSET is full power
    /// again. The kill switch lives in `aterm.toml` so it never depends on the
    /// thing it kills. NEGATIVE CONTROLS (owner ruling of 2026-09-25: only an
    /// explicit setting takes power away): an off spelling seeds the key's
    /// limit, and a value that is neither the key's own nor an off is no
    /// setting — the policy unchanged, the row seeding the default.
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
        // (key, its seed while absent, the value typed, the seed read back,
        // what the policy took)
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
            // Applied when saved, which nothing states, and the card's
            // notes say what its choices mean.
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

        // Negative controls: (the line, its row, the seed, whether it limits).
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

    /// Each AI CLI's own switch (`docs/DESIGN-ai-connections-2026-10-16.md` §10):
    /// every connection (`aterm_agent::ai::connector::ids`) has its `ai.<id>.use`
    /// row, labelled "Use <product> on this machine" (never the harness's
    /// "Supervise"), read back by [`super::ai_use_switch`], in the Harness section
    /// and live. SET `false` writes that CLI's `[ai.<id>]` table, which the AI
    /// layer's reader takes as that CLI unwanted and only that one; UNSET leaves no
    /// table and the row on again. The preferred CLI and the look's switch
    /// round-trip the same way, and every row seeds from the resolved value, a
    /// fleet document beside `aterm.toml` included, this machine's own key over
    /// it. Negative controls: every other CLI's row stays on, and no other key
    /// reads as an AI CLI's switch.
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

        // The preferred CLI: an Enum of every connection, the built-in
        // default its placeholder and no seed until something chooses.
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
        // The look's switch.
        let written =
            apply_prefs_edits("", &[(super::EDIT_AI_ENABLED, set("false"))]).expect("set");
        let cfg = Config::parse(&written).expect("re-parses as Config");
        assert!(!cfg.ai.enabled());
        assert_eq!(seed(&cfg, super::EDIT_AI_ENABLED).as_deref(), Some("false"));
        assert_eq!(
            seed(&Config::default(), super::EDIT_AI_ENABLED).as_deref(),
            Some("true")
        );

        // Seeds from the resolved value: a fleet document beside aterm.toml chooses
        // the preferred CLI and turns Muse Code off; this machine's own key outranks
        // it, and Reset (the key removed) follows the fleet again.
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

    /// Each agent's own switch (`docs/DESIGN-harness-plugins-2026-10-15.md` §3,
    /// §4): every supervisable agent ([`Program::ALL`]) has its
    /// `harness.<agent>.enabled` row in the Harness section, labelled with its
    /// product name and read back by [`super::agent_switch`]. SET `false` writes
    /// that agent's `[harness.<agent>]` table, which the supervisor's reader takes
    /// as that agent switched off and only that agent; UNSET is full power again.
    /// Negative controls: every other agent's row stays on, the master switch off
    /// leaves every agent row on (each row is its agent's table, not `on_for`),
    /// and no other key reads as an agent's switch.
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

        // A saved table seeds its agent's row off and the other's on, in
        // either spelling the reader takes as an off.
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

    /// The two `[packages]` switches are Bool-typed dotted keys in the search-only
    /// Packages section, grouped, searchable and seeded resolved (both default ON,
    /// the retired spellings folded in); neither retired key is a row.
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

    /// `[update] enabled` and `[reroute] announce` are Bool rows, default ON, with
    /// no environment precedence; the development `[update]` owner/repo are not
    /// rows.
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

    /// Dotted writes into `[packages]` keep the keys the co-located atpkg reads
    /// from the same table: a switch replaces only its own child and the retired
    /// spelling it replaced, and a blank-revert keeps the siblings and the table.
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

        // Blank = revert removes only the child; the table (with atpkg keys)
        // stays.
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

        // Bad value never reaches the file (typed_item fail-closed).
        assert!(matches!(
            apply_prefs_edits("", &[(super::EDIT_PACKAGES_ENABLED, set("yes"))]),
            Err(PrefsEditError::BadValue { .. })
        ));
    }

    /// A removal takes the key's own line, its same-line comment included, and
    /// nothing else: the comment and blank lines above it are the person's, and
    /// stay where they were (above the next line, or below a table's last
    /// remaining line).
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
            // The same key as a dotted line among the top-level ones.
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

    /// Settings ▸ Reset and then Undo, through the service that drives both: the
    /// comment lines above the reset key survive the reset, and the undo puts
    /// the file back as it was — the key in its own place, under them.
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

    /// Comments and unrelated keys survive an edit: the write goes through the
    /// aterm-toml DOM, not a re-serialize.
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
        // The header comment, the inline comment, the unrelated `gpu`, and the whole
        // `[keybindings]` table all survive verbatim.
        assert!(out.contains("# my aterm config"), "{out}");
        assert!(out.contains("# cozy"), "{out}");
        assert!(out.contains("gpu = true"), "{out}");
        assert!(out.contains("[keybindings]"), "{out}");
        assert!(out.contains("\"cmd+shift+t\" = \"new_tab\""), "{out}");
        // And the new key landed + re-parses.
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.theme.as_deref(), Some("Nord"));
        assert_eq!(c.font_px, Some(12.0));
    }

    /// The edited key's own inline comment and `=`-spacing survive a value
    /// replacement, top-level and dotted alike.
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
        // The edited keys keep their exact line shape: value swapped, the
        // double-space alignment and inline comment intact on the same line.
        assert!(out.contains("font_px = 18  # cozy size"), "{out}");
        assert!(out.contains("auto_apply = false # hands off"), "{out}");
        // Untouched decor still survives too (header + unrelated key's comment).
        assert!(out.contains("# header comment stays"), "{out}");
        assert!(
            out.contains("scrollback_lines = 5000 # deep history"),
            "{out}"
        );
        // And the round trip stays serde-clean with the new values.
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.font_px, Some(18.0));
        assert_eq!(c.update.as_ref().and_then(|u| u.auto_apply), Some(false));
        assert_eq!(c.scrollback_lines, Some(5000));
    }

    /// W2/W9 glyph-appearance knobs + the W11 `motion` policy: each was
    /// functional (reached the renderer/engine) but had no Settings row, so it
    /// was only settable by hand-editing TOML. Prove the round-trip through the
    /// REAL editor AND that each key exercised here now surfaces in the schema
    /// rows (present, sectioned, searchable). (`font_weight_dark_nudge` and
    /// `font_features`, once the two TOML-only stragglers this note tracked,
    /// are now registered too — the full-coverage batch; see
    /// `registry_conformance_tests`.)
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
        // The load-adaptive toggle is typed Bool and opts out of the overload
        // heuristic.
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
        // The comma-string font_variation splits into the ordered axis list...
        assert_eq!(
            c.font_variation.as_ref().map(|l| l.0.clone()),
            Some(vec!["wght=450".to_string(), "opsz=14".to_string()])
        );
        // ...the TOML-array spelling parses to the same value...
        let arr: Config =
            aterm_toml::from_str("font_variation = [\"wght=450\", \"opsz=14\"]").unwrap();
        assert_eq!(
            arr.font_variation.map(|l| l.0),
            c.font_variation.as_ref().map(|l| l.0.clone())
        );
        // ...and it reaches the engine with font_weight winning (appended last).
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
        // The variable-font row seeds the comma-joined string the loader parses back.
        let row = fields
            .iter()
            .find(|f| f.key == super::EDIT_FONT_VARIATION)
            .unwrap();
        assert_eq!(row.seed.as_deref(), Some("wght=450, opsz=14"));
    }

    /// Each key is typed per its `Config` field (float, int, bool, string), so a
    /// Save round-trips through serde for every field at once.
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
        // The numeric fields are not quoted strings in the output.
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

    /// A non-numeric font size is rejected (`BadValue`) so Save never writes a value
    /// the reload parser would choke on; the caller leaves the file untouched.
    #[test]
    fn bad_numeric_value_is_rejected() {
        let err = apply_prefs_edits("", &[(EDIT_FONT_PX, set("not-a-number"))]).unwrap_err();
        match err {
            PrefsEditError::BadValue { key, .. } => assert_eq!(key, EDIT_FONT_PX),
            other => panic!("expected BadValue, got {other:?}"),
        }
    }

    /// A `font_px` the resolver would ignore at load is refused with the range,
    /// the file untouched; the bounds and the other float keys still save.
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
        // Another float key keeps its own (clamp-at-load) contract.
        assert!(apply_prefs_edits("", &[(super::EDIT_LINE_HEIGHT, set("0"))]).is_ok());
    }

    /// A non-integer scrollback (a float string) is rejected — `scrollback_lines` is a
    /// usize, so "1.5" must not silently truncate or write a float.
    #[test]
    fn float_for_integer_field_is_rejected() {
        for raw in ["1.5", "1.0000000000000001", "0.99999999999999999", "1e-400"] {
            let err = apply_prefs_edits("", &[(EDIT_SCROLLBACK, set(raw))]).unwrap_err();
            assert!(
                matches!(err, PrefsEditError::BadValue { .. }),
                "{raw}: {err:?}"
            );
        }
        // Decimal/exponent input remains useful, without losing any digit.
        // This signed setting admits the full i64 range on every host.
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

    /// The written spelling of `key` in `toml`, as the file holds it.
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

    /// A NUMBER IS WRITTEN AS ITS SETTING'S KIND TYPES IT, at the one place
    /// values are serialised ([`typed_item`]): a whole value of a fractional
    /// setting carries no `.0` (`window_padding = 20` and `font_px = 15` were
    /// written `20.0` and `15.0`, by a slider, `settings set` and every other
    /// writer alike), a fraction keeps its digits, an Integer setting writes an
    /// integer however the value was spelled, and each reads back through
    /// `Config` as the value written. A file that still says `20.0` reads as
    /// before, and an edit of another key leaves that line as it is.
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

        // Every fractional setting: a whole value is an integer, a fraction
        // the shortest decimal that is exactly it — the kind decides, never
        // the caller.
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
            // The middle of the range and a hundredth: inside it, never whole.
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
                // …and reads back as the value written, not merely as a file
                // that loads.
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

        // A file written before reads as it always did; an edit of another
        // key does not re-spell its lines.
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

    /// A NUMBER SETTING TAKES ONLY A FINITE NUMBER. `nan`, `inf` and a decimal
    /// past f64 (`1e400`) all parse as f64, and `settings set window_padding
    /// nan` wrote `window_padding = nan` — a value no reader can use (each maps
    /// it to its default or carries the NaN on) that Modified then listed as a
    /// change. Refused like any other value that is not a number (`wide`), for
    /// every fractional setting and every Integer one, and a fractional one
    /// says what it takes in its row's words ("it must be a number").
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
                // A non-finite `font_px` keeps its own refusal, with its range
                // (`font_px_outside_the_resolvers_range_is_rejected_with_the_range`).
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
                // What it takes, in the words the row says.
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

    /// A malformed existing file is refused (`Parse`) rather than overwritten, so a
    /// hand-corrupted `aterm.toml` is never clobbered by a Save.
    #[test]
    fn malformed_existing_file_is_refused() {
        let err = apply_prefs_edits("this = = broken", &[(EDIT_THEME, set("Nord"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::Parse(_)));
    }

    /// Multiple edits in one batch (set + update + remove) all land together and
    /// nothing else moves.
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

    /// A `dark:…,light:…` split theme (a string with a comma + colons) round-trips as a
    /// single string value, not mangled into a TOML structure.
    #[test]
    fn split_theme_string_round_trips() {
        let out =
            apply_prefs_edits("", &[(EDIT_THEME, set("dark:Dracula,light:GitHub Light"))]).unwrap();
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.theme.as_deref(), Some("dark:Dracula,light:GitHub Light"));
    }

    /// An Enum value is validated against its options and written in its
    /// canonical spelling ("Phaser" → "phaser").
    #[test]
    fn enum_value_is_validated_and_canonicalised() {
        let out = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("Phaser"))]).unwrap();
        assert!(
            out.contains("cursor_trail_style = \"phaser\""),
            "canonical lower-case spelling written: {out}"
        );
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.cursor_trail_style.as_deref(), Some("phaser"));
        // `fire` likewise.
        let out = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("fire"))]).unwrap();
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.cursor_trail_style.as_deref(), Some("fire"));
    }

    /// A colour control validates as hex on Save: a good `#RRGGBB` round-trips as a
    /// string, a malformed colour is rejected (BadValue) rather than written.
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

    /// An absent permission seeds its own default: the palette row ON, every
    /// other permission OFF; an explicit `false` seeds the palette row OFF.
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

    /// Security toggles classify Bool and geometry Integer, and both round-trip as
    /// the right TOML type.
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

    /// P1.4c enum-domain keys: cursor_style/window_theme/bidi/ambiguous_width classify as
    /// Enum with the canonical option lists, and each option writes back as itself (the
    /// spelling the config loader accepts — the loader mappings are tested in app_config).
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
                // A dotted key (`machine.universal_control`) writes its leaf under
                // its table header; a top-level key writes itself.
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

    /// Legacy comma-separated list replacements may begin with a bracketed
    /// path or argument; only a parsed TOML array takes the structured lane.
    #[test]
    fn bracket_prefixed_comma_lists_keep_their_literal_members() {
        let out =
            apply_prefs_edits("", &[(super::EDIT_SHELL_ARGS, set("[working], --flag"))]).unwrap();
        let config: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(
            config.shell_args,
            Some(vec!["[working]".to_string(), "--flag".to_string()])
        );
        // A real TOML array still preserves a member containing a comma.
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

    /// WINDOWS PATH PARITY: a native path is BACKSLASHED, and a backslash is
    /// TOML's escape character — `C:\Users\…` written into a basic string is
    /// the invalid escape `\U`, which fails the whole file. A Windows user's
    /// font and shell rows are all path-valued, so the writer owes
    /// them a representation that survives: `toml_edit` picks a LITERAL
    /// (single-quoted) string, and the contract asserted here is the outcome,
    /// not the spelling. Both writer branches are covered — the scalar one
    /// (`font_family_bold`, `shell`, and the comma-joined `fallback_fonts`
    /// `FontList`) and the [`LIST_KEYS`] array one (`shell_args`).
    ///
    /// Two properties, because a raw write bites twice: the emitted file must
    /// re-parse to the authored path VERBATIM, and it must survive being fed
    /// back in as the existing document — an unescaped path would not merely
    /// lose the value, it would wedge every later Save behind a parse error on
    /// the user's own config.
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

        // The saved file is a valid existing document for the next Save.
        let again = apply_prefs_edits(&out, &[(super::EDIT_FONT_PX, set("14.0"))])
            .expect("a Windows-path config is editable again, not wedged");
        let cfg: Config = aterm_toml::from_str(&again).expect("second Save re-parses");
        assert_eq!(cfg.font_family_bold.as_deref(), Some(bold));
        assert_eq!(cfg.font_px, Some(14.0));
    }

    /// `editable_fields` seeds configured values only, with the right keys and
    /// kinds.
    #[test]
    fn editable_fields_seed_from_config() {
        let c: Config = aterm_toml::from_str(
            "font_px = 13.0\ntheme = \"Nord\"\nscrollback_lines = 4000\ncopy_on_select = true\n",
        )
        .unwrap();
        let fields = editable_fields(&c);
        let keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
        // No duplicate keys.
        let mut uniq: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for k in &keys {
            assert!(uniq.insert(k), "duplicate key {k} in editable_fields");
        }
        // Rows are grouped by section: the order index never decreases.
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
        // The colour rows are present somewhere in the list.
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
        // The bool seeds its resolved state so the checkbox starts in the right spot.
        assert_eq!(seed(EDIT_COPY_ON_SELECT).as_deref(), Some("true"));
        // Ligatures defaults ON, so an unset config seeds the checkbox "true".
        assert_eq!(seed(EDIT_LIGATURES).as_deref(), Some("true"));
        // Unset keys seed None (a blank control), not the effective default.
        assert_eq!(seed(EDIT_FONT_FAMILY), None);
        assert_eq!(seed(EDIT_CURSOR_STYLE), None);
    }

    /// On a fully-unset config every text field seeds blank and each bool its
    /// effective default, so an unchanged Save writes nothing.
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
        // Bools seed their resolved platform default: copy_on_select is OFF on
        // Linux, where a selection owns PRIMARY (`Config::copy_on_select_or_default`).
        assert_eq!(
            seed(EDIT_COPY_ON_SELECT).as_deref(),
            Some(if cfg!(target_os = "linux") {
                "false"
            } else {
                "true"
            })
        );
        assert_eq!(seed(EDIT_LIGATURES).as_deref(), Some("true"));
        // The [machine] Enum seeds the configured value (blank; "off" is only the
        // placeholder).
        assert_eq!(seed(super::EDIT_MACHINE_UNIVERSAL_CONTROL), None);
        let placeholder = fields
            .iter()
            .find(|f| f.key == super::EDIT_MACHINE_UNIVERSAL_CONTROL)
            .map(|f| f.placeholder.as_str());
        assert_eq!(placeholder, Some("off"));
    }

    /// Every numeric control is deliberately a bounded slider (`range_of` is
    /// `Some`) or a known open-ended field, and no non-numeric control has a range.
    #[test]
    fn range_of_covers_bounded_numerics_only() {
        for f in editable_fields(&Config::default()) {
            match f.kind {
                EditKind::Float | EditKind::Integer => {
                    let bounded = range_of(f.key).is_some();
                    // The deliberate free-form numerics: the open-ended sizes a
                    // slider can't span, plus the matrix-rain seed (any u64 is
                    // valid; 0 = the stable per-window sentinel).
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
        // A bounded range is internally sane.
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

    /// A whitespace-only configured string seeds blank (None), matching the display
    /// fallback, so re-saving an untouched window does not materialise a "   " value.
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
    /// Each row draws its own note: Alt/Option as Meta carries none and
    /// Predictive echo its own, and rows that share a sentence draw it once,
    /// after the last of them, as the run's note ([`super::RowNote::Shared`]),
    /// never inside that row's lines.
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
        // Every row of the Sound box says what it covers in its own row.
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
        // EVERY PERMISSION SAYS WHAT ITS OWN OFF REFUSES: no two share a
        // sentence, so the box draws one under each and none under the last
        // as the box's. Negative control: the two question rows share theirs,
        // drawn once, after the second.
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
            "the field says Theme default"
        );
    }

    /// OFF NEVER OVERWRITES THE STYLE (lead-known-open #29): the writer every
    /// lane meets keeps the style an edit turned Off beside it, whatever wrote
    /// the Off — a Settings row, Serious Mode's exit, `settings set`, the
    /// retired master's fold over an older file — and an edit that makes the
    /// style draw again takes it out. An edit that changed nothing about the
    /// style leaves it alone, and one that names it is written as it says.
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
        // A person's own Off, and Serious Mode's exit pin (the same write).
        let off = write(
            "cursor_trail_style = \"comet\"\n",
            &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))],
        );
        assert_eq!(read(&off, EDIT_CURSOR_TRAIL_STYLE).as_deref(), Some("off"));
        assert_eq!(read(&off, BEFORE_OFF).as_deref(), Some("comet"));
        // Unset, the style drew the platform's default: that is what Off
        // replaced (none where the default is Off already).
        let from_default = write("", &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
        assert_eq!(
            read(&from_default, BEFORE_OFF),
            crate::app_config::DEFAULT_DECORATIVE_EFFECTS
                .then(|| super::DEFAULT_CURSOR_TRAIL_STYLE.to_string())
        );
        // Off to Off, and an edit of another key, leave it.
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
        // A drawn style takes it out, chosen or reset to a default that draws.
        let water = write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, set("water"))]);
        assert_eq!(read(&water, BEFORE_OFF), None);
        let reset = write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, None)]);
        assert_eq!(
            read(&reset, BEFORE_OFF).is_none(),
            crate::app_config::DEFAULT_DECORATIVE_EFFECTS
        );
        // Named by the edit (Undo, its Reset): written as it says.
        let forgot = write(
            &off,
            &[(EDIT_CURSOR_TRAIL_STYLE, set("off")), (BEFORE_OFF, None)],
        );
        assert_eq!(read(&forgot, BEFORE_OFF), None);
        // AN OLDER FILE: `cursor_trail = false` held Fire off. A write of the
        // typing glow drops the old switch and pins the style Off (the fold),
        // and Fire is kept rather than lost.
        let held = "cursor_trail = false\ncursor_trail_style = \"fire\"\n";
        let glow = write(held, &[(EDIT_CURSOR_MOMENTUM_GLOW, set("true"))]);
        assert_eq!(read(&glow, RETIRED_CURSOR_TRAIL), None);
        assert_eq!(read(&glow, EDIT_CURSOR_TRAIL_STYLE).as_deref(), Some("off"));
        assert_eq!(read(&glow, BEFORE_OFF).as_deref(), Some("fire"));
        // Choosing Off over it keeps Fire too; choosing a style needs none.
        let chose_off = write(held, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
        assert_eq!(read(&chose_off, BEFORE_OFF).as_deref(), Some("fire"));
        let chose_water = write(held, &[(EDIT_CURSOR_TRAIL_STYLE, set("water"))]);
        assert_eq!(read(&chose_water, BEFORE_OFF), None);
        // Kept by its name in the style list, a value the key's own check
        // takes: an old alias by the style it names, a spelling aterm does not
        // know by the default it drew.
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
        // A kept style never draws: the style alone decides.
        let config: Config = aterm_toml::from_str(&off).unwrap();
        assert_eq!(config.cursor_trail_style_raw(), "off");
    }

    /// AN OLD `classic mono` KEEPS ITS COLOUR WHEN THE WRITER REWRITES IT. The
    /// spelling says Classic and the cursor's colour; the style list knows
    /// only Classic, so the Off that keeps the style before it (`classic`)
    /// — and any other edit that takes the spelling out — writes the colour
    /// as the trail colour (`cursor`) in the same write, and Classic brought
    /// back draws its one-colour face in the live cursor colour, as the old
    /// spelling did. A file that names a trail colour already says it, and an
    /// edit that names the colour is written as it says. Every other spelling
    /// the style reader takes says the style alone, so its list name draws
    /// what it drew and the writer adds nothing. Without the rule the Off
    /// kept `classic` alone and Classic came back in shifting colours.
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
        // What a window draws from the file: the style, its one-colour face,
        // and the colour the trail follows (the live cursor's, per frame).
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
            // Off over it (Serious Mode's exit pin, a Settings row, `settings
            // set`): Classic kept, its colour beside it, and nothing drawn.
            let off = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
            assert_eq!(
                read(&off, BEFORE_OFF).as_deref(),
                Some("classic"),
                "{mono:?}"
            );
            assert_eq!(read(&off, COLOR).as_deref(), Some("cursor"), "{mono:?}");
            assert_eq!(drawn(&off).0, None, "{mono:?}");
            // Brought back, Classic draws in the cursor's colour again.
            let back = write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, set("classic"))]);
            assert_eq!(drawn(&back), classic_in_cursor_color, "{mono:?}");
            assert_eq!(read(&back, BEFORE_OFF), None, "{mono:?}");
            // Another style keeps the colour too, as it keeps any trail colour.
            let comet = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, set("comet"))]);
            assert_eq!(read(&comet, COLOR).as_deref(), Some("cursor"), "{mono:?}");
            // Classic chosen over it (its list name) draws what it drew; the
            // colour is now on its own row, whose Reset gives the shifting
            // colours.
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
            // The retired master's fold over an older file.
            let held = format!("cursor_trail = false\n{file}");
            let glow = write(&held, &[(EDIT_CURSOR_MOMENTUM_GLOW, set("true"))]);
            assert_eq!(
                read(&glow, BEFORE_OFF).as_deref(),
                Some("classic"),
                "{mono:?}"
            );
            assert_eq!(read(&glow, COLOR).as_deref(), Some("cursor"), "{mono:?}");
            // A trail colour in the file already says it: kept as it is.
            let tinted = format!("{file}cursor_trail_color = \"#50FA7B\"\n");
            let tinted_off = write(&tinted, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
            assert_eq!(read(&tinted_off, COLOR).as_deref(), Some("#50FA7B"));
            // One the loader ignores (a typo) drew the cursor's colour: the
            // cursor's takes its place, so Classic comes back as it drew.
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
            // An edit that names the colour is written as it says (Reset All).
            let reset = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, None), (COLOR, None)]);
            assert_eq!(read(&reset, COLOR), None, "{mono:?}");
            // An edit of another key leaves the old spelling as it is.
            let other = write(&file, &[(EDIT_FONT_PX, set("15"))]);
            assert_eq!(read(&other, COLOR), None, "{mono:?}");
            assert_eq!(read(&other, EDIT_CURSOR_TRAIL_STYLE).as_deref(), Some(mono));
        }
        // Every other spelling the reader takes says the style alone.
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
        // The colour's word is a value the trail colour takes, as written,
        // and no other colour key takes it.
        let chosen = write("", &[(COLOR, set(" CURSOR "))]);
        assert_eq!(read(&chosen, COLOR).as_deref(), Some("cursor"));
        assert!(apply_prefs_edits("", &[(EDIT_FOREGROUND, set("cursor"))]).is_err());
        assert!(
            apply_prefs_edits("", &[(super::EDIT_CURSOR_TRAIL_ACCENT, set("cursor"))]).is_err()
        );
    }
}

/// Registry conformance behind Manual and `settings set|unset`: the dotted-key
/// writer and the list-row round-trips.
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

    /// Every registered nested leaf has a row whose kind matches the registry and
    /// `edit_kind`, follows the seeding law (which proves
    /// `nested_seed_placeholder` has an arm for it), sits in a real group, and
    /// writes a value serde re-parses.
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
            // The section router must have a prefix arm (never the Terminal
            // catch-all a top-level unknown falls to — sparkle/matrix are
            // Appearance, update is Terminal BY ARM; asserting
            // group != General proves the group arm too).
            let (group, order) = super::group_of(leaf.key);
            assert_ne!(group, "General", "{} fell to the catch-all group", leaf.key);
            assert!(order < u8::MAX, "{} group order unset", leaf.key);
            let _ = section_of(leaf.key); // total (never panics) — routing pinned above via group
            // Written through the real editor, the result must re-parse as Config.
            let sample = match leaf.kind {
                EditKind::Bool => "true",
                EditKind::Integer => "5",
                EditKind::Float => "0.5",
                EditKind::Enum { options } => options[0],
                EditKind::Color => "#123456",
                // A time is the one text the writer checks (`[harness] land_at`).
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

    /// THE SESSION LOG'S TWO SWITCHES (session log Phase 6, step 6B-3; D3 and
    /// D6, ruled 2026-10-19) are Bool rows in the group and section of
    /// "Restore session at launch", seeded on from the resolved getters, and
    /// the writer types them as TOML booleans, like the `[packages]` switches:
    /// writing `false` leaves a `[session_log]` table that re-parses with the
    /// switch off and seeds the row off, and writing `true` over it turns it
    /// back on. NEGATIVE CONTROL: a dotted key that falls through to Text
    /// (the warning on `edit_kind`'s `[packages]` arm) writes
    /// `journal = "false"`, a string, and the boolean assertion fails before
    /// the kind assertions would.
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
            // The writer first, so a key typed as Text shows as the string
            // it writes.
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

    /// AN INTEGER REFUSAL NAMES THE BOUND IT BROKE, in the words the Settings
    /// row uses, so `settings set` says what to type instead of only "invalid
    /// value": plain digits, as the field takes them back, with the key's
    /// unit. A whole number too long for i64 is still a whole number: its
    /// sign says which end it passed.
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

    /// Every [`EditKind::Integer`] key enforces its [`integer_domain`] at Save:
    /// both boundaries write files `Config` re-parses, one past each is rejected,
    /// `-1` saves only for a signed domain, and `u64::MAX + 1` never does.
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
        // An accepted value must re-parse through serde; a rejection must be a
        // BadValue.
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
            // The boundaries save and re-parse (an arm wider than its field type
            // fails the re-parse).
            assert!(saves(key, lo), "{key}: domain floor {lo} must save");
            assert!(saves(key, hi), "{key}: domain ceiling {hi} must save");
            // One past each writable boundary is refused at Save.
            if lo > i128::from(i64::MIN) {
                assert!(!saves(key, lo - 1), "{key}: {} must be a BadValue", lo - 1);
            }
            if hi < i128::from(i64::MAX) {
                assert!(!saves(key, hi + 1), "{key}: {} must be a BadValue", hi + 1);
            }
            // `-1` saves only for signed fields…
            assert_eq!(
                saves(key, -1),
                lo < 0,
                "{key}: -1 acceptance must match the writable domain's signedness"
            );
            // …and an integer beyond u64::MAX never saves.
            assert!(
                !saves(key, i128::from(u64::MAX) + 1),
                "{key}: u64::MAX + 1 must be a BadValue"
            );
        }
    }

    /// Dotted writes create nested tables on an empty file and edit deep leaves
    /// in existing ones, keeping every comment and sibling.
    #[test]
    fn dotted_key_writes_create_and_edit_nested_tables_preserving_comments() {
        // CREATE: two depths on an empty file, then serde reads them back.
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

        // EDIT: an existing commented table keeps everything but the leaf.
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

        // The INLINE-table spelling (`update = { … }`) edits in place too.
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

    /// Dotted-key REMOVAL reverts exactly the leaf (siblings + comments stay),
    /// removing through a missing table or a scalar is a clean no-op, and a
    /// NON-table intermediate refuses the edit (Parse error, file untouched)
    /// instead of clobbering user data.
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
        // Missing table ⇒ no-op, byte-identical.
        let untouched = "font_px = 12.0\n";
        assert_eq!(
            apply_prefs_edits(untouched, &[("update.auto_apply", None)]).unwrap(),
            untouched
        );
        // A scalar squatting on the table name refuses the write (never doc[..] panics),
        // naming the conflict, and a removal through it leaves the scalar alone.
        let err =
            apply_prefs_edits("update = 5\n", &[("update.auto_apply", set("false"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::Parse(_)), "{err}");
        assert!(err.to_string().contains("not a table"), "{err}");
        assert_eq!(
            apply_prefs_edits("update = 5\n", &[("update.auto_apply", None)]).unwrap(),
            "update = 5\n"
        );
        // And a scalar write over an existing TABLE leaf is refused too.
        let err = apply_prefs_edits(
            "[update.auto_apply]\nx = 1\n",
            &[("update.auto_apply", set("true"))],
        )
        .unwrap_err();
        assert!(matches!(err, PrefsEditError::Parse(_)), "{err}");
    }

    /// LIST rows (`palette`/`shell_args`/`font_features`):
    /// the comma form writes a REAL TOML array serde's `Vec<String>` accepts,
    /// the seed re-joins it (the round trip), a malformed palette entry is
    /// rejected at Save, and an entries-free value is rejected (clear the row to
    /// unset instead of writing `[]`).
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
        // Seeds re-join with ", " — the exact text a re-Save re-splits.
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
        // A typo'd palette colour is rejected (it would be silently ignored at load).
        let err =
            apply_prefs_edits("", &[(super::EDIT_PALETTE, set("#123456, nope"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
        // All-empty split ⇒ rejected, not an empty array.
        let err = apply_prefs_edits("", &[(super::EDIT_SHELL_ARGS, set(", ,"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
    }

    /// The remaining top-level scalars: the Bool arms, the numeric, enum and
    /// colour typings, and a serde round-trip through the real editor.
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
        // Every canonical enum spelling is accepted by its consuming parser.
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
        // The ambient-bed switch writes a real TOML bool.
        assert_eq!(c.trail_sound_bed, Some(true));
        assert!(c.trail_sound_bed_or_default());
        assert_eq!(c.trail_sound_volume, Some(0.2));
        assert_eq!(c.cursor_trail_length, Some(48));
        // Enum values canonicalise (case-variant input → canonical spelling).
        assert_eq!(c.window_colorspace.as_deref(), Some("display-p3"));
        assert_eq!(c.background_material.as_deref(), Some("under-window"));
        assert_eq!(c.cursor_trail_color.as_deref(), Some("#50FA7B"));
        assert_eq!(c.shell.as_deref(), Some("bash"));
        // The padding pair reaches the resolvers the window metrics read.
        assert_eq!(c.window_padding, Some(20.0));
        assert_eq!(c.window_padding_top, Some(6.0));
        assert_eq!(c.window_padding_or_default(), 20.0);
        assert_eq!(c.window_padding_top_or_default(), 6.0);
        // Bad enum + bad colour are rejected, never written.
        let err =
            apply_prefs_edits("", &[(super::EDIT_WINDOW_COLORSPACE, set("rec2020"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
        let err =
            apply_prefs_edits("", &[(super::EDIT_CURSOR_TRAIL_ACCENT, set("teal"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
        // Numeric rows exercised here carry their resolver-domain sliders.
        assert!(range_of(super::EDIT_TRAIL_SOUND_VOLUME).is_some());
        assert!(range_of(super::EDIT_WINDOW_PADDING).is_some());
        assert!(range_of(super::EDIT_WINDOW_PADDING_TOP).is_some());
    }

    /// Predictive editing's kill switch is a Bool row beside Predictive echo
    /// (Keyboard & Input), seeded with the resolved default (on): a Save writes
    /// a TOML bool its serde `Option<bool>` reads back, and that turns it off.
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

    /// FONT-DISPLAY: the two keys round-trip through the real TOML writer with
    /// their typed validation (a bad face id / a non-hex tab color is rejected,
    /// never written), and the `display:` resolver reads what they wrote.
    /// THE CHROME ROWS ARE WIRED: each of the three chrome colour rows is a
    /// Color row in Selected tab color's card whose Save writes its own key and
    /// whose Reset clears it, and the written key reaches the resolution.
    /// NEGATIVE CONTROL: a value that is not a colour is refused by the writer.
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
        // "off" behaves exactly like an unset key in the resolver.
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
        // A display face is a layer over the font, never the font: the
        // family request is still the (unset) font family.
        assert_eq!(c.font_family_request(), None);
        assert_eq!(c.display_faces_request(), ["pixel"]);
        // A MIX round-trips through the writer in canonical joined form, and
        // the layer takes its faces in registry order.
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
        // Unknown face id / bad mixes / non-hex color: rejected by the writer.
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
        // Clearing the display face clears the layer.
        let mut cleared = c.clone();
        cleared.display_font = None;
        assert_eq!(cleared.font_family_request(), cleared.font_family);
        assert!(cleared.display_faces_request().is_empty());
        // A typo'd id that somehow reaches config falls back fail-open.
        let mut typo = c;
        typo.display_font = Some("dooom".to_string());
        assert_eq!(typo.font_family_request(), typo.font_family);
        assert!(typo.display_faces_request().is_empty());
    }
}
