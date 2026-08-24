// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The settings registry and non-destructive `aterm.toml` writer shared by the native
//! Settings tab and Manual editor (App ▸ Settings…, ⌘,). The separate native
//! Preferences window this module once built is retired.
//!
//! aterm's settings live in `~/.config/aterm/aterm.toml` (hot-reloading — see
//! [`crate::app_config`]). Everything here is PURE and platform-independent:
//!   * [`editable_fields`] — the shared config-control registry
//!     (label/key/kind/seed/placeholder, grouped by [`Section`]) from which the
//!     curated native tab and Manual schema select their surfaces;
//!   * [`apply_prefs_edits`] / [`save_prefs_edits`] — write edited values back
//!     NON-DESTRUCTIVELY (preserving the user's other keys, comments, and formatting
//!     via `toml_edit`; atomic temp-write + rename). The serialized native config
//!     worker returns the exact committed bytes and post-publication proof for
//!     direct admission; the watcher remains an independent external-edit source.
//!
//! Clearing a field to blank REMOVES that key (reverting it to its built-in default)
//! rather than writing an empty string. Save is best-effort: a missing config file is
//! created, an unwritable one is logged, never a panic.

use crate::app_config::{
    Config, DEFAULT_TAB_STATUS_DWELL_MS, DEFAULT_TAB_STATUS_QUIET_AFTER_MS,
    DEFAULT_TITLE_SUMMARY_CONTEXT_LINES, DEFAULT_TITLE_SUMMARY_INTERVAL_SECONDS,
    DEFAULT_TITLE_SUMMARY_MODEL, DEFAULT_TITLE_SUMMARY_TIMEOUT_SECONDS,
    EXAMPLE_EXPLICIT_OLLAMA_TITLE_SUMMARY_ENDPOINT, MAX_TAB_STATUS_DWELL_MS,
    MAX_TAB_STATUS_QUIET_AFTER_MS, MAX_TITLE_SUMMARY_CONTEXT_LINES,
    MAX_TITLE_SUMMARY_INTERVAL_SECONDS, MAX_TITLE_SUMMARY_TIMEOUT_SECONDS, MIN_TAB_STATUS_DWELL_MS,
    MIN_TAB_STATUS_QUIET_AFTER_MS, MIN_TITLE_SUMMARY_CONTEXT_LINES,
    MIN_TITLE_SUMMARY_INTERVAL_SECONDS, MIN_TITLE_SUMMARY_TIMEOUT_SECONDS,
};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::trail_sound::SoundVoice;

/// The TOML keys the settings model edits, paired with how each should be TYPED
/// when written back ([`apply_prefs_edits`]). The order matches the on-screen row order
/// (see [`editable_fields`]). These are the exact `Config` field names so a Save
/// followed by a reload round-trips through serde (see [`crate::app_config::Config`]).
pub(crate) const EDIT_FONT_PX: &str = "font_px";
pub(crate) const EDIT_FONT_FAMILY: &str = "font_family";
/// The bundled display-face selector (`display_font`) — one of
/// [`DISPLAY_FONT_OPTIONS`], cleared = the normal font selection. Driven by the
/// Settings "Display Faces" page's mutually-exclusive toggles.
pub(crate) const EDIT_DISPLAY_FONT: &str = "display_font";

/// The DEPRECATED spelling of [`EDIT_DISPLAY_FONT`]. It is still parsed (a
/// `#[serde(alias)]` on `Config::display_font`), still accepted by the config
/// language, and still writable by hand — a key that shipped cannot be deleted
/// without turning correct old configs into complaints. Settings writes the
/// current spelling; this constant exists so every surface that has to
/// RECOGNIZE the old one names it from one place.
pub(crate) const LEGACY_EDIT_DISPLAY_FONT: &str = "game_font";
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
pub(crate) const EDIT_CURSOR_TRAIL: &str = "cursor_trail";
pub(crate) const EDIT_CURSOR_TRAIL_STYLE: &str = "cursor_trail_style";
pub(crate) const EDIT_TRAIL_SOUNDS: &str = "trail_sounds";
/// Tone-melody toggle (`Config::tone_melody`, default ON): the trail-sound
/// melody leans with the typed line's inferred mood (the tiny on-device
/// `aterm_effects::tone` classifier); OFF pins today's neutral melody and
/// stops the classifier entirely.
pub(crate) const EDIT_TONE_MELODY: &str = "tone_melody";
/// Robi the helper robot (`Config::robi`, default OFF — opt-in): the
/// chrome-walking tip-sharing robot show (see `app_config`'s field doc).
pub(crate) const EDIT_ROBI: &str = "robi";
/// Rainbow sparkles on the post-update celebration notice (`Config::notice_sparkle`,
/// default ON — user-facing features ship enabled; this is an opt-OUT). The card that
/// says "Updated — now on vX" wears a hue-cycling badge and a ring of twinkling
/// sparkles instead of the flat cursor-coloured badge. Decorative only: it never
/// changes the wording, the timing, or what the notice is for, and reduced motion
/// holds the colours still.
pub(crate) const EDIT_NOTICE_SPARKLE: &str = "notice_sparkle";
/// Progress-card party trim (`Config::pkg_progress_effects`, default ON —
/// user-facing features ship enabled; this is an opt-OUT). The toolchain
/// install's progress card wears the rainbow-filled bar, per-program completion
/// sparkles, and the cursor kitty riding the bar's leading edge. Decorative
/// only: OFF keeps the card fully functional as a plain themed accent bar —
/// the numbers, phases, queue order, and honest failure states never change.
/// Reduced motion and serious mode strip the same trim without touching this
/// preference.
pub(crate) const EDIT_PKG_PROGRESS_EFFECTS: &str = "pkg_progress_effects";
/// Ambient-bed toggle (`Config::trail_sound_bed`, default OFF — the owner
/// dislikes the drone): ON re-enables the continuous per-style background
/// texture behind the trail notes; OFF gates the synth's bed mixer entirely
/// (zero bed samples — the notes, brrrring, bonk and melody are untouched).
pub(crate) const EDIT_TRAIL_SOUND_BED: &str = "trail_sound_bed";
/// Typing-sound picker (`Config::trail_sound_style`, default `auto`): `auto`
/// follows the visual trail style's signature palette; every other value
/// ([`TRAIL_SOUND_STYLES`]) names an instrument — one of the nine palettes by
/// what it sounds like (`glass bell`, `droplet`, …), the `mechanical`
/// keyboard, or a sound-only voice (`typewriter`, `marimba`, `felt`) —
/// spoken by every keystroke whatever the trail looks like.
pub(crate) const EDIT_TRAIL_SOUND_STYLE: &str = "trail_sound_style";
/// SING-ALONG RIFF toggle (`Config::trail_sound_riff`, default ON).
///
/// Owner ask (Sound menu audit): the held-key sing-along riff is TIER 5 — the
/// loudest thing the engine emits — and had NO independent switch. The only
/// ways to quiet it were the master "Music effects" toggle (which kills every
/// sound, including the keystroke palette the owner wants to keep) or
/// `trail_sound_volume` (which turns the keystrokes down with it). This key is
/// the riff's own gate: OFF keeps every other voice at its configured level and
/// simply schedules no `Celebration(RiffBar)` bar. It is a SOUND gate only —
/// the sing-along's VISUALS (ribbon saturation, star shower, the dancing cat and
/// its singing face) are untouched, because they are a motion contract and the
/// owner asked to quiet the song, not to cancel the celebration.
///
/// Named into the `trail_sound_*` family because that is exactly what it gates:
/// the riff rides the same `trail_audio` synth and is already subordinate to
/// `trail_sounds` × `trail_sound_volume` × raw window focus.
pub(crate) const EDIT_TRAIL_SOUND_RIFF: &str = "trail_sound_riff";
/// AUDIBLE TERMINAL BELL toggle (`Config::bell_sound`, default ON).
///
/// Owner ask (Sound menu audit): the BEL (0x07) beep — `NSBeep` on macOS,
/// `MessageBeep` on Windows — had NO config key at all, and is NOT covered by
/// `trail_sound_volume` because it is an OS alert sound rather than a synth
/// voice, so it was the one genuinely unreachable sound in the product. OFF
/// suppresses only the AUDIBLE beep; the visual bell flash and the
/// urgent-window/Dock-bounce attention request are unaffected, since those are
/// how a muted terminal still surfaces activity.
///
/// PLATFORM: implemented on macOS and Windows only. Other platforms have no
/// beep call at all, so the key is parsed and preserved there but inert —
/// mirrored by [`super::native_settings`]' Advanced gate and by the
/// [`crate::diagnostics`] capability matrix.
pub(crate) const EDIT_BELL_SOUND: &str = "bell_sound";
/// The curse BONK's two gates, named so the Sound menu, the section router and
/// the visibility allowlist all spell them once. They are `[sparkle_words]`
/// leaves in the FILE (that is where the feature's table lives and the spelling
/// must never change), but they are SFX in the UI.
pub(crate) const EDIT_SPARKLE_BONK: &str = "sparkle_words.profanity.bonk";
pub(crate) const EDIT_SPARKLE_BONK_DETONATION: &str = "sparkle_words.profanity.bonk_detonation";

/// THE SOUND MENU (owner ask: "add the volume and SFX menu to settings").
///
/// Every config key that changes what aterm SOUNDS like, and nothing else. One
/// list drives [`section_of`] (they all land in the same pane), [`group_of`]
/// (they all land in the same "Sound" box), and the native Advanced audit, so a
/// new audible knob cannot be added to one and forgotten in the others — the
/// exact drift that left `tone_melody`, `trail_sound_bed` and both bonk keys
/// reachable only by hand-editing `aterm.toml`.
///
/// DELIBERATELY ABSENT — and each exclusion is a JUDGEMENT, recorded here so a
/// later reader can check the reasoning rather than guess at an omission:
///
///   * `serious_mode`. It genuinely IS an audio gate, and not a marginal one:
///     `SeriousEffect::TerminalSound` is read at all three `app_render` sound
///     seams (the single-pane cursor-fx tick, the single-pane present and the
///     split-pane compose) and again at `lib.rs::on_bell`, so with it on every
///     row in this box is silent. It is still excluded, for two reasons that
///     are about the CONTROL, not the effect. First, it is a whole-product
///     policy — it suppresses trails, matrix rain, sparkle words and stream
///     fade too — so a copy of it inside a Sound box would misdescribe its
///     blast radius; it keeps its own "Effect policy" home. Second, it is a
///     Top Effect control with ESCAPE semantics (turning a playful control on
///     while it is set CLEARS it), and a plain Advanced Bool row cannot express
///     that; duplicating one key into two controls that disagree is the exact
///     hazard that also keeps [`EDIT_TRAIL_SOUNDS`] in Top Settings. What the
///     box owes the user instead is a per-row DISCLOSURE, and every audible row
///     carries one — pinned by
///     `native_settings::…::serious_mode_is_disclosed_on_the_sound_rows_it_silences`.
///   * `matrix_rain.bell_alert` — the BEL's amber hue ramp is a VISUAL bell;
///     only [`EDIT_BELL_SOUND`] is audible.
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
];
pub(crate) const EDIT_CURSOR_TRAIL_COLOR: &str = "cursor_trail_color";
pub(crate) const EDIT_CURSOR_TRAIL_ACCENT: &str = "cursor_trail_accent";
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
pub(crate) const EDIT_CURSOR_TRAIL_WAKE_MS: &str = "cursor_trail_wake_ms";
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
// W5 typography/appearance knobs.
pub(crate) const EDIT_LINE_HEIGHT: &str = "line_height";
pub(crate) const EDIT_ADJUST_BASELINE: &str = "adjust_baseline";
pub(crate) const EDIT_MINIMUM_CONTRAST: &str = "minimum_contrast";
pub(crate) const EDIT_SELECTION_INACTIVE: &str = "selection_inactive";
pub(crate) const EDIT_CURSOR_BREAK_LIGATURES: &str = "cursor_break_ligatures";
/// M4 — admit Cascadia N:1 MERGED ligatures (`Config::merged_ligatures`), a Bool,
/// default OFF. Registered so `edit_kind` types it correctly (a serde
/// `Option<bool>` written as a TOML string would corrupt the config on reload).
pub(crate) const EDIT_MERGED_LIGATURES: &str = "merged_ligatures";
pub(crate) const EDIT_BOLD_IS_BRIGHT: &str = "bold_is_bright";
pub(crate) const EDIT_FAINT_OPACITY: &str = "faint_opacity";
// W7 font-metric decoration knobs.
pub(crate) const EDIT_ADJUST_UNDERLINE_POSITION: &str = "adjust_underline_position";
pub(crate) const EDIT_ADJUST_UNDERLINE_THICKNESS: &str = "adjust_underline_thickness";
pub(crate) const EDIT_UNDERLINE_SKIP_DESCENDERS: &str = "underline_skip_descenders";
// W2 glyph-appearance knobs + W9 variable-font instantiation.
pub(crate) const EDIT_TEXT_BLENDING: &str = "text_blending";
pub(crate) const EDIT_FONT_THICKEN: &str = "font_thicken";
pub(crate) const EDIT_STEM_GAMMA: &str = "stem_gamma";
pub(crate) const EDIT_FONT_VARIATION: &str = "font_variation";
pub(crate) const EDIT_FONT_WEIGHT: &str = "font_weight";
// W11 motion / Reduce-Motion accessibility policy.
pub(crate) const EDIT_MOTION: &str = "motion";
/// Process-wide serious-mode overlay. A Bool, default OFF. It suppresses
/// nonessential sound and decoration without rewriting any individual effect
/// preference, so clearing it restores those settings exactly.
pub(crate) const EDIT_SERIOUS_MODE: &str = "serious_mode";
/// Load-adaptive effect shedding toggle (Change #1). A Bool, default ON; OFF opts out
/// of the render-overload heuristic so animations follow `motion` / the OS flag alone.
pub(crate) const EDIT_LOAD_ADAPTIVE_MOTION: &str = "load_adaptive_motion";

/// The hex-colour keys (theme colors plus cursor-trail overrides) — edited as a
/// `RRGGBB`/`#RRGGBB`
/// hex string ([`EditKind::Color`]). Listed once so `edit_kind` and the
/// schema rows agree on which keys are colours.
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
/// The selected-tab color override (`active_tab_color`, `#RRGGBB`), cleared =
/// today's translucent system pill ("Transparent white" on the Tab Color page).
pub(crate) const EDIT_ACTIVE_TAB_COLOR: &str = "active_tab_color";

/// The `display_font` Enum options: `"off"` (the default font — what clearing
/// the key also means) followed by the bundled ids in the toggles' display
/// order. The id tail is pinned against [`aterm_render::DISPLAY_FACES`] by test
/// so the registry, the resolver, and the toggles can never disagree about the
/// set; `"off"` exists so the plain popup control (Compact / search results) can
/// turn the display face off without a separate reset affordance —
/// `font_family_request` treats it exactly like an unset key.
///
/// The retired game ids are deliberately NOT here: this list is what Settings
/// OFFERS, and offering `minecraft` again would re-create the name the rename
/// removed. They remain ACCEPTED — see
/// [`aterm_render::DISPLAY_FACE_LEGACY_IDS`] and the config language's
/// deprecation diagnostic — so a hand-written config keeps loading.
pub(crate) const DISPLAY_FONT_OPTIONS: &[&str] = &["off", "chunky", "pixel", "engraved", "bubble"];

/// The pre-rename `display_font` ids, in registry order. ACCEPTED everywhere a
/// value is validated (the config language, the TOML writer) and OFFERED
/// nowhere, so a hand-written config keeps loading without Settings ever
/// re-suggesting a game's name. Pinned against
/// [`aterm_render::DISPLAY_FACE_LEGACY_IDS`] by test.
///
/// Includes `mariokart`, whose face was deleted outright: a value that was
/// valid yesterday must not become a config-language complaint today. It
/// resolves to nothing, so the primary font is used — see
/// `Config::font_family_request`.
pub(crate) const LEGACY_DISPLAY_FONT_IDS: &[&str] = &[
    "roblox",
    "minecraft",
    "zelda",
    "mariokart",
    "animal-crossing",
];

/// Whether one `display_font` id is accepted: a current registry id, or one of
/// the [`LEGACY_DISPLAY_FONT_IDS`]. `"off"` is deliberately NOT accepted here —
/// this answers "does this name a face", and an off-mix is just a cleared key.
#[must_use]
pub(crate) fn display_font_id_is_accepted(id: &str) -> bool {
    let id = id.trim();
    aterm_render::display_face_canonical_id(id).is_some() || LEGACY_DISPLAY_FONT_IDS.contains(&id)
}
/// Smart-title controls. These names deliberately match the public `aterm.toml`
/// schema exactly; in particular, credentials are represented only by a FILE PATH.
/// The Settings surface deliberately defines no separate raw-token value.
pub(crate) const EDIT_DESCRIPTIVE_TITLES: &str = "descriptive_titles";
pub(crate) const EDIT_TITLE_SUMMARY_PROVIDER: &str = "title_summary_provider";
pub(crate) const EDIT_TITLE_SUMMARY_MODEL: &str = "title_summary_model";
pub(crate) const EDIT_TITLE_SUMMARY_ENDPOINT: &str = "title_summary_endpoint";
pub(crate) const EDIT_TITLE_SUMMARY_TOKEN_FILE: &str = "title_summary_token_file";
pub(crate) const EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS: &str = "title_summary_timeout_seconds";
pub(crate) const EDIT_TITLE_SUMMARY_PROXY_MODE: &str = "title_summary_proxy_mode";
pub(crate) const EDIT_TITLE_SUMMARY_CA_FILE: &str = "title_summary_ca_file";
pub(crate) const EDIT_TITLE_SUMMARY_INTERVAL_SECONDS: &str = "title_summary_interval_seconds";
pub(crate) const EDIT_TITLE_SUMMARY_CONTEXT_LINES: &str = "title_summary_context_lines";
pub(crate) const EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT: &str = "title_summary_include_output";
pub(crate) const EDIT_TITLE_SUMMARY_ALLOW_REMOTE: &str = "title_summary_allow_remote";
pub(crate) const EDIT_TAB_TITLE_FORMAT: &str = "tab_title_format";
pub(crate) const EDIT_WINDOW_TITLE_FORMAT: &str = "window_title_format";
/// Complete Smart Titles settings roster. Consumers that need to invalidate a
/// runtime-health observation use this list rather than maintaining another
/// copy that can drift from the Settings group.
pub(crate) const SMART_TITLE_KEYS: &[&str] = &[
    EDIT_DESCRIPTIVE_TITLES,
    EDIT_TITLE_SUMMARY_PROVIDER,
    EDIT_TITLE_SUMMARY_MODEL,
    EDIT_TITLE_SUMMARY_ENDPOINT,
    EDIT_TITLE_SUMMARY_TOKEN_FILE,
    EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS,
    EDIT_TITLE_SUMMARY_PROXY_MODE,
    EDIT_TITLE_SUMMARY_CA_FILE,
    EDIT_TITLE_SUMMARY_INTERVAL_SECONDS,
    EDIT_TITLE_SUMMARY_CONTEXT_LINES,
    EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT,
    EDIT_TITLE_SUMMARY_ALLOW_REMOTE,
    EDIT_TAB_TITLE_FORMAT,
    EDIT_WINDOW_TITLE_FORMAT,
];
/// Tab Subject & Status (RFC §12). `EDIT_TAB_STATUS` is the master switch; the
/// other three are inert while it is off, which [`crate::native_settings`]
/// discloses rather than leaving the reader to infer.
pub(crate) const EDIT_TAB_STATUS: &str = "tab_status";
pub(crate) const EDIT_TAB_STATUS_QUIET_AFTER_MS: &str = "tab_status_quiet_after_ms";
pub(crate) const EDIT_TAB_STATUS_DWELL_MS: &str = "tab_status_dwell_ms";
pub(crate) const EDIT_TAB_STATUS_BADGE: &str = "tab_status_badge";
/// The §4 connection mark's opt-out (Session Connections). Independent of the
/// status classifier quartet above: it gates only the fourth tab mark, never
/// the connections themselves.
pub(crate) const EDIT_TAB_CONNECTION_BADGE: &str = "tab_connection_badge";
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

/// The PHOSPHOR matrix-rain master switch — the `[matrix_rain]` table's `enabled`
/// key. The Settings ROW comes from its [`NESTED_LEAVES`] registry entry
/// (Appearance ▸ Matrix rain, Bool, default OFF); this named constant exists for
/// the code that addresses the key directly — the semantic-snapshot dotted
/// projection, `raw_bool_value`, and the tests. Runtime per-session overrides
/// (View ▸ Matrix Rain, `aterm-ctl rain`) win over this durable bit until the
/// session ends.
pub(crate) const EDIT_MATRIX_RAIN_ENABLED: &str = "matrix_rain.enabled";

/// The `[packages]` toolchain-manager maintenance switches, addressed as
/// dotted keys (the same nested writer as [`EDIT_MATRIX_RAIN_ENABLED`]). All
/// are Bools. The background-service master and `auto_update` default ON (the
/// pre-config 6h `atpkg update` cadence), `auto_install` defaults OFF —
/// installing the multi-GB default toolset needs explicit consent, and this
/// Settings switch IS the consent click (`docs/TOOLCHAIN-PACKAGE-MANAGER.md`
/// §11). Housed in the search-only Packages section: the rows render on the
/// special Settings ▸ Packages page (which no `section()` registry page owns),
/// while Search and Modified still find them through the ordinary registry.
pub(crate) const EDIT_PACKAGES_ENABLED: &str = "packages.enabled";
/// See [`EDIT_PACKAGES_ENABLED`]; default ON.
pub(crate) const EDIT_PACKAGES_AUTO_UPDATE: &str = "packages.auto_update";
/// See [`EDIT_PACKAGES_AUTO_UPDATE`]; default OFF (consent-gated).
pub(crate) const EDIT_PACKAGES_AUTO_INSTALL: &str = "packages.auto_install";
/// `[packages] seed_install` — lay the BUNDLED toolchain down on first launch.
/// Default ON: those bytes already shipped inside the app, so installing the app is
/// the consent and the remaining cost is extraction, not download. Off turns the
/// first run into an announced offer instead. Distinct from
/// [`EDIT_PACKAGES_AUTO_INSTALL`], which governs NETWORK installs.
pub(crate) const EDIT_PACKAGES_SEED_INSTALL: &str = "packages.seed_install";

/// The security opt-in toggles — ALL fail-closed (default OFF). Grouped so `edit_kind`
/// and the schema rows agree they are booleans, and so the Settings UI can label them
/// together as "Security" controls.
pub(crate) const SECURITY_BOOL_KEYS: &[&str] = &[
    EDIT_ALLOW_OSC52_QUERY,
    EDIT_ALLOW_WINDOW_OPS,
    EDIT_ALLOW_NOTIFICATIONS,
    EDIT_ALLOW_PALETTE_RECONFIGURE,
    EDIT_ALLOW_KITTY_FILE_TRANSFER,
    // Not an allow_* PTY power — the one PROTECTION in the set (the five
    // above grant programs abilities; this one takes an ability away from
    // every OTHER process). Same fail-closed default-off contract.
    EDIT_SECURE_KEYBOARD_ENTRY,
];

pub(crate) const EDIT_WINDOW_THEME: &str = "window_theme";
pub(crate) const EDIT_BIDI: &str = "bidi";
pub(crate) const EDIT_AMBIGUOUS_WIDTH: &str = "ambiguous_width";

// Enum option domains — the CANONICAL spellings each config loader accepts (aliases like
// beam/on/off/single/double still parse from a hand-edited file, but the picker only
// offers — and `typed_item` only writes — these canonical values). Keep in sync with the
// parsers in `app_config.rs` (a hand-typed alias outside this set resolves at load).
// The underline option is retired (owner: block + "|" only; DECSCUSR still honored).
pub(crate) const CURSOR_STYLES: &[&str] = &["block", "bar"];
pub(crate) const WINDOW_THEMES: &[&str] = &["auto", "light", "dark"];
pub(crate) const BIDI_MODES: &[&str] = &["implicit", "disabled", "explicit"];
pub(crate) const AMBIGUOUS_WIDTHS: &[&str] = &["narrow", "wide"];
/// Closed domain for the live title summarizer. `builtin` never sends terminal
/// contents over the network; the network providers remain subject to the separate
/// `title_summary_allow_remote` privacy gate in the runtime.
pub(crate) const TITLE_SUMMARY_PROVIDERS: &[&str] =
    &["builtin", "ollama", "openai-compatible", "off"];
/// Closed remote-provider proxy policy. An attested, aterm-owned Ollama child always
/// bypasses proxies; OpenAI-compatible providers apply this exact choice.
pub(crate) const TITLE_SUMMARY_PROXY_MODES: &[&str] = &["environment", "direct"];
/// Closed domain for how the program-supplied title and live description are composed
/// in the tab strip and native window title.
pub(crate) const TITLE_FORMATS: &[&str] = &[
    "title",
    "description",
    "title-description",
    "description-title",
];
/// Closed domain for `predictive_echo` (the canonical names; `PredictMode::parse`
/// also accepts aliases like auto/on/force from a hand-edited file). Exposing it as
/// an Enum means the overlay cycles a valid value and Save rejects a typo, instead
/// of writing free Text that `parse` silently maps to Off.
pub(crate) const PREDICTIVE_ECHO_MODES: &[&str] = &["off", "adaptive", "always"];
/// Closed domain for `text_blending` (W2 anti-aliasing weight); the canonical
/// spellings `text_blending_or_default` accepts (the `linear_corrected`
/// underscore alias still parses from a hand-edited file).
pub(crate) const TEXT_BLENDINGS: &[&str] = &["linear-corrected", "linear"];
/// Closed domain for the `motion` accessibility policy (W11); these are the
/// exact spellings `MotionMode::parse` accepts and the picker writes.
pub(crate) const MOTION_MODES: &[&str] = &["auto", "full", "reduced"];
/// Closed domain for `window_colorspace` (M3 phase A) — the canonical spellings
/// of [`crate::app_config::WindowColorspace::parse`] (the `displayp3`/`p3`
/// aliases still parse from a hand-edited file).
pub(crate) const WINDOW_COLORSPACES: &[&str] = &["srgb", "display-p3"];
/// Closed domain for `background_material` (M5 vibrancy) — the canonical
/// spellings of [`crate::app_config::BackgroundMaterial::parse`] (the
/// `underwindow`/`under_window` aliases still parse from a hand-edited file).
pub(crate) const BACKGROUND_MATERIALS: &[&str] = &["none", "hud", "sidebar", "under-window"];
/// Closed domain for `sparkle_words.profanity.style` — the spellings
/// `sparkle_deco_config` classifies (unknown values fall back to `rainbow`).
pub(crate) const SPARKLE_PROFANITY_STYLES: &[&str] = &["rainbow", "nova", "sparkle"];
/// Closed domain for `sparkle_words.feline.style` (unknown → `cat`).
pub(crate) const SPARKLE_FELINE_STYLES: &[&str] = &["cat", "paw"];
pub(crate) const EDIT_PREDICTIVE_ECHO: &str = "predictive_echo";
/// Focus-linked shell priority boost (Windows QoS; no-op elsewhere).
pub(crate) const EDIT_FOCUS_BOOST: &str = "focus_boost";

// ---- The FULL-COVERAGE batch (AI-driveability): every remaining `Config` scalar
// key gets a registry row so Manual/schema discovery and `settings set|unset`
// can address it. Each key's TYPE arm in `edit_kind` matters — a missed Bool falls to
// Text and a Save then writes a TOML string a serde `Option<bool>` rejects on
// reload (the corruption class the schema guard documents).

/// GPU rendering master switch (`Config::gpu`, default ON with CPU fallback).
/// RESTART-ONLY: the backend is built at launch — `restart_notices` surfaces the
/// "applies on next launch" banner when it changes.
pub(crate) const EDIT_GPU: &str = "gpu";
/// Indexed ANSI palette (`Config::palette`, `Vec<String>` of `#RRGGBB` by 0-based
/// index) — a [`LIST_KEYS`] row (comma-separated text ⇄ TOML array; entries
/// hex-validated at Save like the [`EditKind::Color`] rows).
pub(crate) const EDIT_PALETTE: &str = "palette";
/// Trail Pack manifest paths (`Config::cursor_trail_packs`) — a [`LIST_KEYS`] row.
pub(crate) const EDIT_CURSOR_TRAIL_PACKS: &str = "cursor_trail_packs";
/// Trail sound level 0..=1 (`Config::trail_sound_volume`, default 0.4).
pub(crate) const EDIT_TRAIL_SOUND_VOLUME: &str = "trail_sound_volume";
// (The trail colour/accent, kitty sprite, comet-geometry, bloom, shimmer and
// HDR/SDR glow keys are declared once with the core cursor-trail keys above.)
/// M2 "ink that dries" stream fade master (`stream_fade`, default OFF).
pub(crate) const EDIT_STREAM_FADE: &str = "stream_fade";
/// Stream-fade window in ms (`stream_fade_ms`, default 90, clamp 16..=1000).
pub(crate) const EDIT_STREAM_FADE_MS: &str = "stream_fade_ms";
/// Interactive shell program (`Config::shell`; discovery-resolved name or path).
pub(crate) const EDIT_SHELL: &str = "shell";
/// Extra shell argv (`Config::shell_args`) — a [`LIST_KEYS`] row.
pub(crate) const EDIT_SHELL_ARGS: &str = "shell_args";
/// GPU-present colour-space tag (`window_colorspace`: srgb | display-p3).
pub(crate) const EDIT_WINDOW_COLORSPACE: &str = "window_colorspace";
/// RESTORE-1 session restore at launch (`restore_session`, default ON).
pub(crate) const EDIT_RESTORE_SESSION: &str = "restore_session";
/// OpenType feature requests (`Config::font_features`) — a [`LIST_KEYS`] row.
pub(crate) const EDIT_FONT_FEATURES: &str = "font_features";
/// Extra `wght` on dark themes (`font_weight_dark_nudge`, clamp 0..=300).
pub(crate) const EDIT_FONT_WEIGHT_DARK_NUDGE: &str = "font_weight_dark_nudge";
/// M5 window glass opacity (`background_opacity`, default 1.0 solid, clamp 0..=1).
pub(crate) const EDIT_BACKGROUND_OPACITY: &str = "background_opacity";
/// M5 macOS vibrancy material (`background_material`).
pub(crate) const EDIT_BACKGROUND_MATERIAL: &str = "background_material";
/// Terminal-tab backdrop image path (`wallpaper`). Unlike the other
/// filesystem-backed asset keys this one stays STRUCTURED-writable: the
/// Settings ▸ Wallpaper file picker writes it, and the versioned service
/// re-resolves the image inline on that patch (see
/// `VersionedConfigService::apply` — the wallpaper re-admit arm).
pub(crate) const EDIT_WALLPAPER: &str = "wallpaper";
/// Wallpaper legibility dim (`wallpaper_dim`, default 0.3, clamp 0..=1).
pub(crate) const EDIT_WALLPAPER_DIM: &str = "wallpaper_dim";
/// Backdrop-hue glyph tint while a wallpaper is attached
/// (`wallpaper_text_tint`, default ON).
pub(crate) const EDIT_WALLPAPER_TEXT_TINT: &str = "wallpaper_text_tint";
/// B.9 per-session temporal recorder opt-in (`temporal_recording`, default OFF).
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
pub(crate) const LIST_KEYS: &[&str] = &[
    EDIT_PALETTE,
    EDIT_SHELL_ARGS,
    EDIT_CURSOR_TRAIL_PACKS,
    EDIT_FONT_FEATURES,
];

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
    matches!(key, EDIT_CURSOR_NYAN_SPRITE | EDIT_CURSOR_TRAIL_PACKS)
}

/// Values intentionally edited only in Manual: lossless collections,
/// host-backed asset sources that require off-thread preparation, and legacy
/// feline controls that have no honest native choice surface. `style = "paw"`
/// remains a supported compatibility mode, but it is ink-only in cat-art v4;
/// the other four feline keys are parsed no-effect compatibility data.
pub(crate) fn manual_only_key(key: &str) -> bool {
    manual_collection_key(key)
        || config_asset_source_key(key)
        || matches!(
            key,
            "sparkle_words.feline.style"
                | "sparkle_words.feline.idle"
                | "sparkle_words.feline.gaze"
                | "sparkle_words.feline.color"
                | "sparkle_words.feline.intensity"
        )
}

/// One SCALAR leaf of a nested config table (`[net]` / `[update]` /
/// `[sparkle_words]` (+ sub-tables) / `[matrix_rain]`), addressed by its DOTTED
/// TOML path (`"net.listen"`, `"sparkle_words.profanity.enabled"`). The writer
/// ([`apply_prefs_edits`]) walks/creates the table chain non-destructively, so
/// these edit exactly like top-level keys; `settings set net.listen …` works.
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
/// (`net.connections`, `sparkle_words.languages`/`toy_packs`/`deny`, the per-class
/// `extra_words`/`ignore_words`/`palette`, `[[sparkle_words.custom]]`) are
/// deliberately NOT here: only scalar leaves are registered, and those tables'
/// arrays remain hand-edit/TOML-native (see `DEFERRED_CONFIG_KEYS`' rationale
/// class — no faithful single-text shape).
pub(crate) const NESTED_LEAVES: &[NestedLeaf] = &[
    // [net] — the L3 opt-in TLS drive listener (env vars still override).
    NestedLeaf {
        key: "net.listen",
        label: "Net: listener numeric IP and port",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: "net.cert",
        label: "Net: server certificate path",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: "net.key",
        label: "Net: server key path",
        kind: EditKind::Text,
    },
    // [update] — the self-update channel + apply policy.
    NestedLeaf {
        key: "update.owner",
        label: "Update: GitHub owner",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: "update.repo",
        label: "Update: GitHub repo",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: "update.auto_apply",
        label: "Update: apply immediately",
        kind: EditKind::Bool,
    },
    // [sparkle_words] — master + top-level scalars.
    NestedLeaf {
        key: "sparkle_words.enabled",
        label: "Sparkle words",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.reduced_motion",
        label: "Sparkle: force static (no twinkle)",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.suppress_in_alt_screen",
        label: "Sparkle: suppress in alt screen",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.lexicon",
        label: "Sparkle: extra lexicon path",
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
        label: "Profanity supernova chance (%)",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.magic",
        label: "Profanity rare variants (magic)",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.density",
        label: "Profanity spark density",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.anim_ms",
        label: "Profanity sparkle duration (ms)",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.jitter",
        label: "Profanity jitter (px)",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "sparkle_words.profanity.intensity",
        label: "Profanity opacity",
        kind: EditKind::Float,
    },
    // The bonk's two gates keep their `[sparkle_words.profanity]` FILE spelling
    // (renaming a config key silently orphans every line already authored), but
    // their LABELS are written for the Sound menu they now appear in — the row
    // has to say what it sounds like, because "Profanity opacity" is two boxes
    // and one pane away from it.
    NestedLeaf {
        key: EDIT_SPARKLE_BONK,
        label: "Curse bonk (typed)",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_SPARKLE_BONK_DETONATION,
        label: "Curse bonk on supernova",
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
        label: "Feline graphic mode (Manual only)",
        kind: EditKind::Enum {
            options: SPARKLE_FELINE_STYLES,
        },
    },
    NestedLeaf {
        key: "sparkle_words.feline.idle",
        label: "Retired feline idle animation",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.gaze",
        label: "Retired feline gaze tracking",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.magic",
        label: "Feline rare variants (magic)",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.color",
        label: "Retired feline tint",
        kind: EditKind::Color,
    },
    NestedLeaf {
        key: "sparkle_words.feline.intensity",
        label: "Retired feline opacity",
        kind: EditKind::Float,
    },
    NestedLeaf {
        key: "sparkle_words.feline.allow_bare_cat",
        label: "Feline: decorate bare \"cat\"",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.cjk_single_char",
        label: "Feline: decorate lone CJK cat",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "sparkle_words.feline.log",
        label: "Feline: record Kitty Log",
        kind: EditKind::Bool,
    },
    // [sparkle_words.orca] / [sparkle_words.ink] / [sparkle_words.emphasis]
    NestedLeaf {
        key: "sparkle_words.orca.enabled",
        label: "Orca splash",
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
        label: "Ink sweep window (ms)",
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
    // [matrix_rain] — every scalar knob (the `enabled` master included; the
    // v1-inert keys are still real settings the loader parses + persists).
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
        // `matrix`/`theme`/`#RRGGBB` — an OPEN domain (arbitrary hex), so Text,
        // not Enum; a malformed hex fails CLOSED to matrix green at load.
        key: "matrix_rain.hue",
        label: "Rain hue",
        kind: EditKind::Text,
    },
    NestedLeaf {
        key: "matrix_rain.mutation_ms",
        label: "Rain glyph mutation (ms)",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.idle_secs",
        label: "Rain idle drain (s)",
        kind: EditKind::Integer,
    },
    NestedLeaf {
        key: "matrix_rain.suppress_in_alt_screen",
        label: "Rain: suppress in alt screen",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.materialize",
        label: "Rain materialize (v1 inert)",
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
        key: "matrix_rain.ink_text",
        label: "Rain ink text (v1 inert)",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.phosphor",
        label: "Rain phosphor layer (v1 inert)",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "matrix_rain.seed",
        label: "Rain field seed",
        kind: EditKind::Integer,
    },
];

/// Registry lookup for a dotted nested key — `None` for top-level keys and
/// unregistered paths.
pub(crate) fn nested_leaf(key: &str) -> Option<&'static NestedLeaf> {
    NESTED_LEAVES.iter().find(|l| l.key == key)
}

/// The EXPLICIT allowlist of `Config` serde fields deliberately NOT in the
/// settings registry, each with its rationale. The
/// `every_config_field_is_registered_or_deferred` exhaustiveness test fails any
/// field that is neither registered (directly or via dotted-leaf rows) nor
/// listed here — so a future `Config` field cannot silently skip the
/// introspection surface; deferring it requires writing the reason down.
#[cfg_attr(not(test), allow(dead_code))] // consumed by the exhaustiveness gate
pub(crate) const DEFERRED_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "keybindings",
        "a chord→action TABLE, not a scalar: no faithful single-field shape exists, and a \
         lossy string encoding could corrupt user bindings on a Save round-trip; edit the \
         [keybindings] table in aterm.toml (validated by --validate-config)",
    ),
    (
        "key_sequences",
        "a chord→raw-bytes TABLE whose values carry escape sequences that require TOML \
         literal-string quoting; the same no-lossy-encoding rationale as keybindings",
    ),
    (
        "right_click",
        "the grid right-button gesture (copy_paste|off, platform-defaulted: the conhost/WT \
         copy-if-selection-else-paste convention on Windows, off elsewhere); Settings has no \
         Mouse section yet, and seeding this one key as an orphan enum row would misfile a \
         platform-semantics choice under an unrelated page — it joins the registry with the \
         Mouse section (wheel bypass, link modifier, middle-click) as one coherent block",
    ),
    (
        "tab_menu_chord",
        "the Windows-only keyboard spelling of the tab context menu (on|menu_key|off): a \
         key-OWNERSHIP escape hatch, not a preference — its whole purpose is to hand \
         Menu / Shift+F10 back to a terminal application, which is a Keyboard-section \
         concern the registry has no page for yet; it joins the registry with that section \
         rather than sitting as an orphan enum row on an unrelated page",
    ),
    (
        "font_hinting",
        "the Linux glyph grid-fitting mode (full|light|native|off, R2) — inert on \
         macOS/Windows, and this registry has no platform-gated-row precedent yet: a knob \
         that visibly does nothing on two of three platforms would read as broken. It is a \
         full config key (aterm.toml, $ATERM_FONT_HINTING alias, hot-reload); it joins the \
         Typography section when per-platform row visibility exists",
    ),
    (
        "font_subpixel",
        "the Linux subpixel-RGB text mode (off|rgb|bgr, RFC-linux-subpixel-text stage 1) — \
         the same platform-gated-row deferral as font_hinting, compounded: this stage is \
         CPU-compositor-only (the default GPU backend renders grayscale regardless), so a \
         Settings row would visibly do nothing for most users on ALL THREE platforms. It \
         is a full config key (aterm.toml, $ATERM_FONT_SUBPIXEL alias, hot-reload); it \
         joins the Typography section with font_hinting when per-platform (and \
         per-backend) row visibility exists",
    ),
    (
        "tab_band_height",
        "the in-grid tab band's height policy (compact|standard, platform-defaulted: standard \
         on Windows, compact elsewhere). It is INERT on macOS — the platform this Settings \
         surface is authored and screenshotted on puts tabs in the native toolbar and never \
         paints the band — so a live row here would read as a control that does nothing, the \
         exact dishonesty the Updates page is already criticised for; it joins the registry \
         with the Windows chrome block (caption tint, band height, strip focus dim) once \
         Settings has a page whose rows are all live on the reader's platform",
    ),
    (
        "windowing_behavior",
        "where a NEW terminal opens when one is already running (new_window|attach); this is \
         the only Config key no running window ever reads — the FRONT DOOR \
         (crates/aterm/src/main.rs) consults it before a window exists, and the Windows jump \
         list reads it to decide whether a 'New Tab' taskbar row would tell the truth. \
         Settings' whole contract is live, previewable, this-window state (uncommitted values \
         project into the workbench scene), and a launch-routing choice has neither a preview \
         nor any effect on the window you changed it in; Settings has no Launch section to \
         file it under, so seeding it as an orphan enum row on the Window page would promise \
         an in-window effect it cannot have. It joins the registry with a Launch/Startup \
         section. Reachable meanwhile from Manual (it is in native_config_language's \
         MANUAL_SCHEMA), from aterm.toml, and from $ATERM_WINDOWING_BEHAVIOR",
    ),
];

/// Every setting whose uncommitted value must have a renderer-native visual
/// projection. This production registry is deliberately explicit: the Settings
/// coverage gate compares it with the editable-field sections, so adding a new
/// visual field without a preview mapping fails tests instead of silently
/// shipping a dead control.
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
    // Cursor Kitty (3) — the cat's own page; every one previews through the
    // SAME cursor scene as the Cursor & Motion block below it.
    EDIT_CURSOR_TRAIL_STYLE,
    EDIT_CURSOR_TRAIL_WAKE_MS,
    EDIT_CURSOR_NYAN_SPRITE,
    // Cursor & Motion (17)
    EDIT_CURSOR_STYLE,
    EDIT_CURSOR_BLINK,
    EDIT_CURSOR_TRAIL,
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
    // Window & Tabs (4)
    EDIT_COLUMNS,
    EDIT_LINES,
    EDIT_TAB_STRIP_ROWS,
    EDIT_SHOW_BUILD_BADGE,
];

/// Visual-section registry rows WITHOUT a renderer-native preview projection —
/// the explicit allowlist twin of [`VISUAL_PREVIEW_KEYS`], same discipline as
/// [`DEFERRED_CONFIG_KEYS`]: the preview-coverage gate fails any visual-section
/// field that is in neither list, so a new field cannot silently ship a dead
/// preview. Every entry documents why no projection exists yet:
///   * `EDIT_TRAIL_SOUNDS` / `EDIT_TRAIL_SOUND_VOLUME` / `EDIT_TONE_MELODY`
///     / `EDIT_TRAIL_SOUND_BED` — aural, no pixels;
///   * the decorative overlay tables (`sparkle_words.*`, `matrix_rain.*`) —
///     live full-screen effects previewed by the effects themselves, not the
///     Settings workbench scene;
///   * list-valued rows (`palette`, `cursor_trail_packs`, `font_features`) —
///     no single-scene projection for an open-ended list (the pack CHOICES do
///     preview via the dynamic trail-style options);
///   * `window_colorspace` / `background_opacity` / `background_material` —
///     compositor/present-path facts a virtual preview cannot honestly show;
///   * `restore_session` / `window_padding` / `window_padding_top` — session
///     and window-geometry knobs pending the workbench geometry rework;
///   * Smart Titles — live runtime/provider state has its own truthful status
///     card; a synthetic terminal preview cannot attest network locality,
///     installed models, errors, or a session-authored Description;
///   * `font_weight_dark_nudge` / `stream_fade` / `stream_fade_ms` — theme-
///     conditional / temporal effects deferred with the preview-matrix
///     campaign (KNOWN INCOMPLETE, documented at the migration commit).
#[cfg_attr(not(test), allow(dead_code))] // consumed by the preview-coverage gate
pub(crate) const VISUAL_PREVIEW_EXEMPT_KEYS: &[&str] = &[
    // A process-wide suppression policy, not a single preview-scene property.
    // Its shipping gates are exercised by the app/render conformance tests.
    EDIT_SERIOUS_MODE,
    // The display-face toggle re-renders the WHOLE terminal in the chosen face on
    // save (hot-reload) — the grid itself is the truthful preview; a workbench
    // specimen projection joins the preview-matrix campaign with the other
    // font-identity rows.
    EDIT_DISPLAY_FONT,
    // A native-toolbar-pill (window chrome) fact like the deferred chrome knobs:
    // the strip the user is looking at recolors on save; the workbench scene
    // draws no native toolbar to project it into.
    EDIT_ACTIVE_TAB_COLOR,
    EDIT_TRAIL_SOUNDS,
    EDIT_TRAIL_SOUND_VOLUME,
    EDIT_TONE_MELODY,
    EDIT_TRAIL_SOUND_BED,
    EDIT_TRAIL_SOUND_STYLE,
    // A live full-screen show (walk-in, ladder, monkey bars) previewed by the
    // effect itself, not the Settings workbench scene — the decorative-table
    // rationale (`sparkle_words.*`) for a single top-level key.
    EDIT_ROBI,
    // Decorative, previewed by the thing itself (the celebration card appears after
    // an update) rather than by the Settings workbench — same rationale as `robi`.
    EDIT_NOTICE_SPARKLE,
    // Same rationale again: the progress card appears during a real toolchain
    // install, not in the workbench scene.
    EDIT_PKG_PROGRESS_EFFECTS,
    // Aural, no pixels — the same rationale as their five siblings above.
    EDIT_TRAIL_SOUND_RIFF,
    EDIT_BELL_SOUND,
    EDIT_PALETTE,
    EDIT_CURSOR_TRAIL_PACKS,
    EDIT_FONT_FEATURES,
    EDIT_FONT_WEIGHT_DARK_NUDGE,
    EDIT_WINDOW_COLORSPACE,
    EDIT_BACKGROUND_OPACITY,
    EDIT_BACKGROUND_MATERIAL,
    // The wallpaper recolors the LIVE grid on save (the same truthful-preview
    // rationale as the display faces); the dim and glyph tint ride the same
    // repaint. A workbench backdrop projection joins the preview-matrix
    // campaign.
    EDIT_WALLPAPER,
    EDIT_WALLPAPER_DIM,
    EDIT_WALLPAPER_TEXT_TINT,
    EDIT_RESTORE_SESSION,
    EDIT_WINDOW_PADDING,
    EDIT_WINDOW_PADDING_TOP,
    EDIT_DESCRIPTIVE_TITLES,
    EDIT_TITLE_SUMMARY_PROVIDER,
    EDIT_TITLE_SUMMARY_MODEL,
    EDIT_TITLE_SUMMARY_ENDPOINT,
    EDIT_TITLE_SUMMARY_TOKEN_FILE,
    EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS,
    EDIT_TITLE_SUMMARY_PROXY_MODE,
    EDIT_TITLE_SUMMARY_CA_FILE,
    EDIT_TITLE_SUMMARY_INTERVAL_SECONDS,
    EDIT_TITLE_SUMMARY_CONTEXT_LINES,
    EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT,
    EDIT_TITLE_SUMMARY_ALLOW_REMOTE,
    EDIT_TAB_TITLE_FORMAT,
    EDIT_WINDOW_TITLE_FORMAT,
    // Tab-chrome state (a busy dot, an attention mark) has no workbench scene to
    // project into: the preview draws a grid, not a tab strip, and the bits come
    // from a live classifier rather than from any property of a rendered cell.
    EDIT_TAB_STATUS,
    EDIT_TAB_STATUS_QUIET_AFTER_MS,
    EDIT_TAB_STATUS_DWELL_MS,
    EDIT_TAB_STATUS_BADGE,
    // The connection mark shares the tab-chrome rationale above: it lives on
    // the tab strip, not in any workbench grid cell.
    EDIT_TAB_CONNECTION_BADGE,
    EDIT_STREAM_FADE,
    EDIT_STREAM_FADE_MS,
];

/// Default scrollback line cap when `scrollback_lines` is unset (mirrors the engine
/// `TerminalConfig.scrollback_limit` default). Shown as the [`EditField::placeholder`]
/// for the scrollback row so an unset control reads as the real resolved value, not a
/// confusing blank.
const DEFAULT_SCROLLBACK_LINES: usize = 100_000;

/// The default cursor style when `cursor_style` is unset (the engine default is a block
/// cursor). The placeholder hint for the cursor-style row.
const DEFAULT_CURSOR_STYLE: &str = "block";

/// The default `cursor_trail_style` when unset — the RAINBOW KITTY PET: the same
/// banded rainbow ribbon, trailed by the full-body cat that WALKS, runs and pounces
/// along the line instead of the flying head (the owner's own machine has run this
/// spelling for weeks, and shipping anything else made the default a stranger to the
/// product). The name has changed three times: the original single-word `nyan` became
/// the two-word `nyan rainbow`, then `rainbow kitty` (the owner's name for the ribbon —
/// it says what you SEE), and the default now names the PET companion on top of it.
/// Every historical spelling still resolves via [`CURSOR_TRAIL_STYLE_ALIASES`], so old
/// configs keep working — and a config that says `rainbow kitty` explicitly still gets
/// the flying head, untouched. The placeholder hint for the row.
///
/// This is the SINGLE definition of the default: `app_config`, `native_settings` and
/// `settings_preview` read it rather than re-typing the literal, so the next rename is
/// one line.
pub(crate) const DEFAULT_CURSOR_TRAIL_STYLE: &str = "rainbow kitty pet";

/// The selectable values for `cursor_trail_style`: the additive `phaser` sweep (a
/// full-spectrum hue streak), `rainbow kitty` (the banded rainbow ribbon under the
/// flying head), `rainbow kitty pet` (the DEFAULT — that ribbon with the walking cat),
/// the native cadence-`comet` (a directional fading comet under the light
/// crown), the other additive LUMEN-wake looks
/// (lumen / sparkle / fire / laser / water), the `beam` style (a clean steady TUBE of
/// cool light that powers down — promoted from a bloom-free preset to its own
/// [`crate::cursor_glow::GlowStyle::Beam`]), and `off`. This is the SINGLE source of
/// truth for the [`EditKind::Enum`] control's options, the save-time domain
/// validation in [`typed_item`], AND the `controls settings` introspection options
/// dump — so the three can never disagree. Mirrors the documented set in the starter
/// config (`cli.rs`) and the cases of [`crate::cursor_glow::GlowStyle::parse`]
/// (whose extra alias spellings resolve through [`CURSOR_TRAIL_STYLE_ALIASES`]).
pub(crate) const CURSOR_TRAIL_STYLES: &[&str] = &[
    "phaser",
    "rainbow kitty",
    // The same banded-ribbon trail, with the full-body PET companion instead of
    // the flying kitty (`GlowStyle::style_names_kitty_pet`) — and the SHIPPED
    // DEFAULT ([`DEFAULT_CURSOR_TRAIL_STYLE`]). A first-class option rather than
    // an alias: it is a real choice the picker must offer, even though both
    // spellings resolve to one `GlowStyle`.
    "rainbow kitty pet",
    // The same banded-ribbon trail and the same full-body pet, drawn as a DOG
    // (`PetSpecies::Dog`). A first-class option beside the kitty pet for the
    // same reason that one is: it is a real choice the picker must offer, and
    // all three spellings still resolve to one `GlowStyle`.
    "rainbow dog pet",
    "comet",
    "lumen",
    "sparkle",
    "fire",
    "laser",
    "water",
    "beam",
    "off",
];

/// `trail_sound_style` options — THE TYPING-SOUND PICKER (Settings ▸ Sound ▸
/// "Typing sound"). Same single-source-of-truth law as
/// [`CURSOR_TRAIL_STYLES`]: the picker, the save-time domain validation, the
/// a11y option list and the introspection dump all read this — and the
/// strings themselves are spelled ONCE, in the synth
/// ([`aterm_effects::trail_sound::SoundVoice::name`]); this list is built
/// from the roster ([`SoundVoice::ALL`]) in picker order, so a voice cannot
/// join the synth without joining the picker (`trail_sound_styles_are_the_
/// synth_roster` pins the bijection).
///
/// `auto` = follow the visual trail's own palette (the default, today's
/// sound bit for bit); the next nine are the shipped palettes selectable by
/// what they SOUND like whatever the trail looks like (`glass bell` = the
/// rainbow kitty's bell, `droplet` = water's plip, …); `mechanical` is the
/// keyboard; `typewriter` / `marimba` / `felt` are the sound-only
/// instruments. Alias spellings (the trail-style names, `mech`, `thock`, …)
/// resolve through [`trail_sound_style_canonical`] /
/// `Config::trail_sound_voice` — accepted on load, never offered.
pub(crate) const TRAIL_SOUND_STYLES: &[&str] = &[
    SoundVoice::Style.name(),
    SoundVoice::Of(GlowStyle::RainbowKitty).name(),
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
/// `None` for a spelling the runtime would silently play as `auto` — the
/// case the validator flags and the Settings row must not mistake for a
/// custom entry. The [`cursor_trail_style_canonical`] twin, answered by the
/// synth's own parser so the UI, the validator and the engine share one
/// vocabulary.
pub(crate) fn trail_sound_style_canonical(token: &str) -> Option<&'static str> {
    SoundVoice::parse(token).map(SoundVoice::name)
}

/// The Settings picker's option list for `cursor_trail_style`: the built-in
/// [`CURSOR_TRAIL_STYLES`] plus one `pack:<id>` entry per LOADED Trail Pack
/// (ids sorted for a stable order). This is the dynamic twin of the static
/// `CURSOR_TRAIL_STYLES` used by the `EditKind::Theme`→`builtin_names()`
/// pattern — handed the loaded-pack ids (from `SettingsState::trail_pack_ids`,
/// seeded from the config's Trail Packs) so the popup, the ←/→ cycler, the
/// accessibility tree, and the `controls`/`inspect` domain dump all list the
/// same options. The built-in list is returned verbatim when no packs are
/// loaded (byte-identical picker).
pub(crate) fn cursor_trail_style_options<'a>(
    pack_ids: impl Iterator<Item = &'a str>,
) -> Vec<String> {
    let mut out: Vec<String> = CURSOR_TRAIL_STYLES
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let mut packs: Vec<String> = pack_ids.map(|id| format!("pack:{id}")).collect();
    packs.sort();
    out.extend(packs);
    out
}

/// The documented ALIAS spellings `GlowStyle::parse` (and the `glow_config`
/// enablement gate) accepts for `cursor_trail_style`, mapped to their canonical
/// [`CURSOR_TRAIL_STYLES`] option. NOTE `nyan rainbow`/`nyan`/`rainbow` →
/// `rainbow kitty`: the banded-ribbon style's canonical (displayed) name is now
/// `rainbow kitty`, and EVERY name it has ever shipped under keeps resolving to
/// it, so a config written against any past release still selects the same
/// effect. `rainbow` maps to the ACTUAL banded rainbow ribbon, not the old
/// laser-like sweep (which lives on as the explicit `phaser`). The single alias
/// source shared by the Settings panel (`enum_alias`), `--validate-config`, and
/// the load-time unknown-style warning — so the UI, the validator, and the
/// engine can never disagree about which spellings are real.
pub(crate) const CURSOR_TRAIL_STYLE_ALIASES: &[(&str, &str)] = &[
    ("nyan rainbow", "rainbow kitty"),
    ("nyan", "rainbow kitty"),
    ("rainbow", "rainbow kitty"),
    ("kitty pet", "rainbow kitty pet"),
    ("pet kitty", "rainbow kitty pet"),
    ("dog pet", "rainbow dog pet"),
    ("pet dog", "rainbow dog pet"),
    ("rainbow puppy pet", "rainbow dog pet"),
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

/// Resolve a `cursor_trail_style` spelling (canonical or documented alias,
/// case-insensitive, pre-trimmed by the caller) to its canonical option, or
/// `None` for a spelling the engine's enablement gate would silently disable —
/// the case the validator and the load-time warning must flag.
pub(crate) fn cursor_trail_style_canonical(token: &str) -> Option<&'static str> {
    CURSOR_TRAIL_STYLES
        .iter()
        .find(|o| token.eq_ignore_ascii_case(o))
        .copied()
        .or_else(|| {
            CURSOR_TRAIL_STYLE_ALIASES
                .iter()
                .find(|(a, _)| token.eq_ignore_ascii_case(a))
                .map(|&(_, canonical)| canonical)
        })
}

/// How a Preferences key should be TYPED in the written TOML, so a Save round-trips
/// through `Config`'s serde types (font_px float, scrollback_lines int, copy_on_select
/// bool, the rest strings). Drives [`apply_prefs_edits`]'s value construction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EditKind {
    /// A floating-point number (`font_px`). Written as a TOML float.
    Float,
    /// An integer (`scrollback_lines`). Written as a TOML integer. [`typed_item`]
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
    /// A string constrained to a fixed set of allowed values (`cursor_trail_style`).
    /// Written as a TOML basic string like [`EditKind::Text`], but [`typed_item`]
    /// REJECTS a value outside `options` (case-insensitive) and normalises it to the
    /// canonical spelling, so a bad enum can never be saved. `controls settings`
    /// advertises `options` so the value domain is machine-readable.
    Enum { options: &'static [&'static str] },
    /// The `theme` key: a colour-scheme NAME picked from the built-in registry
    /// plus valid user-installed `<name>.conf` themes, resolved dynamically so
    /// the picker tracks both. Written as a TOML string like [`EditKind::Text`]
    /// — NOT domain-rejected, because the value can also be the
    /// `dark:X,light:Y` split form. Cycling it live re-themes the terminal (the preview).
    Theme,
    /// An `RRGGBB`/`#RRGGBB` colour string (`foreground`, `cursor_color`, …). Edited via
    /// the in-panel text editor like [`EditKind::Text`], but [`typed_item`] REJECTS a
    /// value that doesn't parse as a hex colour ([`crate::app_config::parse_hex_color`]),
    /// so a malformed colour can't be saved (it would otherwise be ignored at load).
    /// The overlay opens the COLOUR WHEEL popover on these rows instead of the
    /// free-text editor (design §7); commits still funnel through this validation.
    Color,
}

/// The TOML type of each editable key, so [`apply_prefs_edits`] can parse the raw
/// control text into a correctly-typed `toml_edit` value. The single source of truth
/// shared by the window (which builds the controls) and the writer (which types them).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn edit_kind(key: &str) -> EditKind {
    if COLOR_KEYS.contains(&key) {
        return EditKind::Color;
    }
    if SECURITY_BOOL_KEYS.contains(&key) {
        return EditKind::Bool;
    }
    // Nested dotted-key leaves carry their type in the ONE registry row
    // (`NESTED_LEAVES`), so the writer and the UI can never disagree.
    if let Some(leaf) = nested_leaf(key) {
        return leaf.kind;
    }
    // List-valued keys EDIT as Text (the one-line comma form, the
    // `fallback_fonts` precedent) — `typed_item` writes them as TOML ARRAYS.
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
        // selected-tab override is the same shape in the Window section.
        EDIT_CURSOR_TRAIL_COLOR | EDIT_CURSOR_TRAIL_ACCENT | EDIT_ACTIVE_TAB_COLOR => {
            EditKind::Color
        }
        EDIT_DISPLAY_FONT => EditKind::Enum {
            options: DISPLAY_FONT_OPTIONS,
        },
        EDIT_TITLE_SUMMARY_PROVIDER => EditKind::Enum {
            options: TITLE_SUMMARY_PROVIDERS,
        },
        EDIT_TITLE_SUMMARY_PROXY_MODE => EditKind::Enum {
            options: TITLE_SUMMARY_PROXY_MODES,
        },
        EDIT_TAB_TITLE_FORMAT | EDIT_WINDOW_TITLE_FORMAT => EditKind::Enum {
            options: TITLE_FORMATS,
        },
        EDIT_FONT_PX
        | EDIT_LINE_HEIGHT
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
        | EDIT_CURSOR_TRAIL_WAKE_MS
        | EDIT_STREAM_FADE_MS
        | EDIT_COLUMNS
        | EDIT_LINES
        | EDIT_TAB_STRIP_ROWS
        | EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS
        | EDIT_TITLE_SUMMARY_INTERVAL_SECONDS
        | EDIT_TITLE_SUMMARY_CONTEXT_LINES
        | EDIT_TAB_STATUS_QUIET_AFTER_MS
        | EDIT_TAB_STATUS_DWELL_MS
        | EDIT_SEARCH_HISTORY_LINES
        | EDIT_ADJUST_BASELINE
        | EDIT_ADJUST_UNDERLINE_POSITION
        | EDIT_ADJUST_UNDERLINE_THICKNESS
        | EDIT_FONT_WEIGHT => EditKind::Integer,
        EDIT_COPY_ON_SELECT
        | EDIT_LIGATURES
        | EDIT_CURSOR_TRAIL
        | EDIT_TRAIL_SOUNDS
        | EDIT_TONE_MELODY
        | EDIT_ROBI
        | EDIT_NOTICE_SPARKLE
        | EDIT_PKG_PROGRESS_EFFECTS
        | EDIT_TRAIL_SOUND_BED
        | EDIT_TRAIL_SOUND_RIFF
        | EDIT_BELL_SOUND
        | EDIT_CURSOR_BLINK
        | EDIT_OPTION_AS_META
        | EDIT_CONFIRM_MULTILINE_PASTE
        | EDIT_SELECTION_INACTIVE
        | EDIT_CURSOR_BREAK_LIGATURES
        | EDIT_MERGED_LIGATURES
        | EDIT_FONT_SYNTHETIC_STYLE
        | EDIT_BOLD_IS_BRIGHT
        | EDIT_FONT_THICKEN
        | EDIT_SHOW_BUILD_BADGE
        | EDIT_DESCRIPTIVE_TITLES
        | EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT
        | EDIT_TITLE_SUMMARY_ALLOW_REMOTE
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
        | EDIT_STREAM_FADE
        | EDIT_RESTORE_SESSION
        | EDIT_TEMPORAL_RECORDING
        // CRITICAL: the dotted [packages] maintenance switches must type as Bool —
        // falling to Text would write a TOML string for a serde `Option<bool>`,
        // silently corrupting a table the CO-LOCATED atpkg also parses (the
        // matrix_rain.* leaves are typed by their NESTED_LEAVES rows before this
        // match is consulted).
        | EDIT_PACKAGES_ENABLED
        | EDIT_PACKAGES_AUTO_UPDATE
        | EDIT_PACKAGES_AUTO_INSTALL
        | EDIT_PACKAGES_SEED_INSTALL
        | EDIT_WALLPAPER_TEXT_TINT => EditKind::Bool,
        EDIT_CURSOR_TRAIL_STYLE => EditKind::Enum {
            options: CURSOR_TRAIL_STYLES,
        },
        EDIT_TRAIL_SOUND_STYLE => EditKind::Enum {
            options: TRAIL_SOUND_STYLES,
        },
        _ => EditKind::Text,
    }
}

/// An error from [`apply_prefs_edits`]: either the existing file is not valid TOML
/// (so a non-destructive edit can't be performed safely), or a supplied value does not
/// parse as the key's declared type. Both are surfaced (logged) by the caller, which
/// then leaves the file untouched rather than risk clobbering it.
#[derive(Debug)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum PrefsEditError {
    /// The current `aterm.toml` text failed to parse as TOML (`toml_edit` error). The
    /// edit is refused so a malformed file is never overwritten.
    Parse(String),
    /// A control value did not parse as its key's declared [`EditKind`] (e.g. a
    /// non-numeric font size). Carries the offending `(key, raw)` for the message.
    BadValue { key: String, raw: String },
    /// A credential-adjacent control contained an invalid value. The raw input is
    /// deliberately not retained: this error is logged and shown in Settings, so
    /// carrying a pasted secret here would turn a successful rejection into a
    /// diagnostic disclosure.
    SensitiveBadValue { key: String, expected: &'static str },
}

impl std::fmt::Display for PrefsEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrefsEditError::Parse(e) => write!(f, "existing aterm.toml is not valid TOML: {e}"),
            PrefsEditError::BadValue { key, raw } => {
                write!(f, "invalid value for {key}: {raw:?}")
            }
            PrefsEditError::SensitiveBadValue { key, expected } => {
                write!(f, "invalid value for {key}: expected {expected}")
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
///   * `Some(raw)` — SET `key` to `raw`, typed per [`edit_kind`] (`font_px` → float,
///     `scrollback_lines` → integer, `copy_on_select` → bool, the rest → string). An
///     existing key is UPDATED in place: its surrounding formatting/comments survive,
///     INCLUDING the key's own same-line inline comment and `=`-spacing, which live
///     on the replaced value node and are copied across ([`adopt_inline_decor`]);
///     a new key is appended.
///   * `None` — REMOVE `key` (revert to its built-in default). Absent already ⇒ no-op.
///
/// DOTTED KEYS (`"table.key"`, e.g. `matrix_rain.enabled`, or deeper like
/// `sparkle_words.profanity.enabled`): the edit walks the table chain to the
/// leaf — missing tables are created, an existing table is edited in place
/// (comments + sibling keys survive). Removal removes ONLY the leaf: parent
/// tables are left in place even when emptied (deleting `[matrix_rain]`
/// outright would also take its comments, and an empty table parses to the
/// same defaults). A dotted edit REFUSES (fail-closed `Parse` error) when any
/// step of the chain is not a table (e.g. `matrix_rain = 5`): silently
/// clobbering a hand-authored scalar would destroy user data.
///
/// Every OTHER key, every comment, and the document's formatting are PRESERVED, because
/// the edit goes through `toml_edit`'s format-preserving DOM, not a re-serialize. Only
/// the listed keys change.
///
/// Errors ([`PrefsEditError`]): the existing text isn't valid TOML, or a value doesn't
/// parse as its key's type — in both cases the caller leaves the file untouched.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn apply_prefs_edits(
    existing_toml: &str,
    edits: &[(&str, Option<String>)],
) -> Result<String, PrefsEditError> {
    let mut doc = existing_toml
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| PrefsEditError::Parse(e.to_string()))?;

    for (key, value) in edits {
        // DOTTED keys edit a nested-table leaf (`net.listen`,
        // `sparkle_words.profanity.enabled`): the writer walks the table chain
        // — creating missing tables, refusing to clobber a non-table — instead
        // of the top-level `doc[key]` path, which would write a literal
        // `"net.listen"` ROOT key serde ignores.
        if key.contains('.') {
            let parts: Vec<&str> = key.split('.').collect();
            match value {
                None => remove_nested_key(&mut doc, &parts),
                Some(raw) => {
                    let item = typed_item(key, raw)?;
                    set_nested_key(&mut doc, &parts, item)?;
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
                // The key's own SAME-LINE decor (the space run after `=`, any
                // trailing inline `# comment`) lives on the OLD value node, so
                // a plain replace would silently delete the user's annotation.
                // Copy it onto the replacement first — the "its surrounding
                // formatting/comment survives" contract covers the EDITED key,
                // not just its neighbours.
                adopt_inline_decor(doc.get(key), &mut item);
                doc[*key] = item;
            }
        }
    }

    Ok(doc.to_string())
}

/// Copy the SAME-LINE decor of the value being replaced — the whitespace run
/// between `=` and the value, and the trailing inline `# comment` — onto its
/// replacement. `toml_edit` stores that decor ON THE VALUE NODE, so replacing the
/// `Item` wholesale (both the `doc[key] = …` top-level path and the dotted-leaf
/// `TableLike::insert`) discards it: `font_px = 12.0  # cozy` edited to `18` must
/// serialize as `font_px = 18.0  # cozy`, never lose the annotation. (Full-line
/// comments ABOVE the key ride the Key's decor and survive an item replacement on
/// their own.) A no-op when either side isn't a plain value — a fresh key has no
/// decor to inherit, and a table squatting on the name is refused upstream.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn adopt_inline_decor(old: Option<&toml_edit::Item>, new: &mut toml_edit::Item) {
    let Some(old_value) = old.and_then(toml_edit::Item::as_value) else {
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
/// through BOTH table forms — `[net]` header tables and `net = { … }` inline
/// tables (`as_table_like_mut` spans both). Refuses (a [`PrefsEditError::Parse`],
/// file untouched) when an intermediate exists as a NON-table value (`net = 5`)
/// or when the leaf itself is currently a table — overwriting either would
/// destroy user structure this editor has no business rewriting.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn set_nested_key(
    doc: &mut toml_edit::DocumentMut,
    parts: &[&str],
    mut item: toml_edit::Item,
) -> Result<(), PrefsEditError> {
    let not_a_table = |part: &str| {
        PrefsEditError::Parse(format!(
            "config key {part:?} exists but is not a table; refusing to overwrite it"
        ))
    };
    let (leaf, tables) = parts.split_last().expect("dotted key has segments");
    let mut cur: &mut toml_edit::Item = doc.as_item_mut();
    for part in tables {
        let table = cur.as_table_like_mut().ok_or_else(|| not_a_table(part))?;
        if table.get(part).is_none_or(toml_edit::Item::is_none) {
            let mut fresh = toml_edit::Table::new();
            fresh.set_implicit(true);
            table.insert(part, toml_edit::Item::Table(fresh));
        }
        cur = table.get_mut(part).expect("just ensured present");
    }
    let table = cur
        .as_table_like_mut()
        .ok_or_else(|| not_a_table(parts[parts.len() - 2]))?;
    if table.get(leaf).is_some_and(toml_edit::Item::is_table_like) {
        return Err(not_a_table(leaf));
    }
    // Same-line decor survives a leaf replacement, exactly like the top-level
    // path — the dotted writer is the same non-destructive contract one table
    // down (`listen = "…" # local only` keeps its comment through an edit).
    adopt_inline_decor(table.get(leaf), &mut item);
    table.insert(leaf, item);
    Ok(())
}

/// REMOVE a nested leaf at dotted path `parts` (revert to its built-in
/// default). Any missing/non-table step along the way means the leaf is not
/// set ⇒ a clean no-op, mirroring the top-level remove. The PARENT tables are
/// left in place even when emptied — deleting `[net]` outright would also take
/// its comments, and an empty table parses to the same defaults.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn remove_nested_key(doc: &mut toml_edit::DocumentMut, parts: &[&str]) {
    let (leaf, tables) = parts.split_last().expect("dotted key has segments");
    let mut cur: &mut toml_edit::Item = doc.as_item_mut();
    for part in tables {
        let Some(table) = cur.as_table_like_mut() else {
            return;
        };
        let Some(next) = table.get_mut(part) else {
            return;
        };
        cur = next;
    }
    if let Some(table) = cur.as_table_like_mut() {
        table.remove(leaf);
    }
}

/// The WRITABLE domain of an [`EditKind::Integer`] key. This is normally the exact
/// serde-representable range of the key's `Config` field type, not the resolver's
/// semantic clamp range ([`range_of`] mirrors those for sliders; an in-domain value
/// beyond a resolver clamp still saves and is clamped at load, unchanged behaviour).
/// Smart-title request controls are the deliberate exception: their operational
/// bounds are part of the Settings/control-socket contract, so their writable domain
/// is the narrower resolver range and a saved value always equals the worker's value.
/// [`typed_item`] rejects a write outside this domain because it would either be valid
/// TOML that serde rejects wholesale, or a Smart Title value the resolver immediately
/// replaces. In the first case the live reload only warns and keeps the old config
/// (the Save looks like it succeeded), and the NEXT launch falls back to
/// `Config::default()`, silently discarding the user's entire configuration until the
/// line is hand-fixed.
///
/// The catch-all arm is the `u64`/`usize` shape (`0..=i64::MAX` — TOML integers top
/// out at `i64::MAX`, so that is the writable ceiling for any unsigned width ≥ 64):
/// most Integer fields are that shape, and a FUTURE signed key that forgets its arm
/// fails LOUD at Save (a spurious `BadValue` on its first negative write, caught by
/// `integer_keys_enforce_their_serde_domain`) instead of corrupting the file.
/// Signed/narrow fields get explicit arms; that conformance test walks every
/// registered Integer key and re-parses each boundary through the real serde model,
/// so an arm can never drift from its field type.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn integer_domain(key: &str) -> std::ops::RangeInclusive<i64> {
    match key {
        // Smart-title request policy: unlike ordinary sliders, these bounds are
        // enforced by every Settings writer so reload never silently substitutes a
        // different timeout, cadence, or context size than the value just saved.
        EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS => {
            i64::try_from(MIN_TITLE_SUMMARY_TIMEOUT_SECONDS)
                .expect("smart-title timeout floor fits i64")
                ..=i64::try_from(MAX_TITLE_SUMMARY_TIMEOUT_SECONDS)
                    .expect("smart-title timeout ceiling fits i64")
        }
        EDIT_TITLE_SUMMARY_INTERVAL_SECONDS => {
            i64::try_from(MIN_TITLE_SUMMARY_INTERVAL_SECONDS)
                .expect("smart-title interval floor fits i64")
                ..=i64::try_from(MAX_TITLE_SUMMARY_INTERVAL_SECONDS)
                    .expect("smart-title interval ceiling fits i64")
        }
        EDIT_TITLE_SUMMARY_CONTEXT_LINES => {
            i64::try_from(MIN_TITLE_SUMMARY_CONTEXT_LINES)
                .expect("smart-title context floor fits i64")
                ..=i64::try_from(MAX_TITLE_SUMMARY_CONTEXT_LINES)
                    .expect("smart-title context ceiling fits i64")
        }
        // Tab-status policy: same rule as the smart-title trio — the writer
        // enforces the RESOLVER's domain, so a saved value is exactly the value
        // the next reload classifies with.
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
        // cursor_trail_length, stream_fade_ms, sparkle anim_ms, matrix_rain
        // mutation_ms/idle_secs/seed) + the fail-safe default for future keys
        // (see above).
        _ => 0..=i64::MAX,
    }
}

fn title_summary_path_has_url_authority(value: &str) -> bool {
    // Reject an authority-bearing URI without banning Windows drive paths or
    // ordinary relative filenames containing a colon.
    value.split_once("://").is_some_and(|(scheme, rest)| {
        let mut chars = scheme.chars();
        chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && scheme.len() >= 2
            && !rest.is_empty()
    })
}

/// Lexical fail-closed guard for the Smart Titles bearer-token FILE field.
///
/// This is deliberately not a filesystem existence check: Settings must accept a
/// path before its file is provisioned, and relative paths are part of the public
/// config contract. It only rejects values that are unmistakably pasted credential
/// material rather than paths (authorization headers, common API-key prefixes,
/// compact JWTs, and URL-shaped values). Keeping the check here, immediately before
/// [`typed_item`] constructs TOML, covers the overlay, native Settings service, and
/// control-socket writers that all converge on [`apply_prefs_edits`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn title_summary_token_file_looks_like_path(value: &str) -> bool {
    if value.is_empty() || value.chars().any(char::is_control) {
        return false;
    }

    let lower = value.to_ascii_lowercase();
    if lower.starts_with("bearer ")
        || lower.starts_with("authorization:")
        || lower.starts_with("authorization=")
    {
        return false;
    }

    if title_summary_path_has_url_authority(value) {
        return false;
    }

    // A compact JWT is three base64url segments. Requiring the canonical JSON
    // header prefix avoids mistaking a legitimate dotted relative filename for a
    // token (for example `provider-token.with-dots.jwt`).
    let mut jwt = value.split('.');
    let looks_like_jwt = match (jwt.next(), jwt.next(), jwt.next(), jwt.next()) {
        (Some(header), Some(payload), Some(signature), None) => {
            let base64url = |segment: &str| {
                !segment.is_empty()
                    && segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            };
            header.starts_with("eyJ")
                && header.len() >= 8
                && signature.len() >= 8
                && base64url(header)
                && base64url(payload)
                && base64url(signature)
        }
        _ => false,
    };
    if looks_like_jwt {
        return false;
    }

    // Shared prompt/output detection covers fixed provider prefixes, compact JWTs,
    // and long opaque token shapes. Apply it to single-component values only:
    // paths such as `secrets/sk_prod-token` remain valid relative paths.
    let is_single_component = !value.contains('/') && !value.contains('\\');
    if is_single_component && crate::title_summary::looks_like_raw_credential(value) {
        return false;
    }

    true
}

/// Lexical guard for the custom CA-bundle FILE field. A path may be provisioned
/// after Settings is saved, so this deliberately performs no filesystem check. It
/// rejects controls, URLs, and unmistakable inline PEM armor before any certificate
/// or private-key material can be persisted or copied into a diagnostic.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn title_summary_ca_file_looks_like_path(value: &str) -> bool {
    if value.is_empty()
        || value.chars().any(char::is_control)
        || title_summary_path_has_url_authority(value)
    {
        return false;
    }
    let upper = value.to_ascii_uppercase();
    !upper.contains("-----BEGIN ") && !upper.contains("-----END ")
}

/// Build the correctly-TYPED `toml_edit` item for `key` from its raw control text,
/// per [`edit_kind`]. A malformed numeric/bool — or an integer outside the key's
/// serde-representable domain ([`integer_domain`]) — is a
/// [`PrefsEditError::BadValue`] so a Save never writes a value the reload parser
/// would reject.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn typed_item(key: &str, raw: &str) -> Result<toml_edit::Item, PrefsEditError> {
    use toml_edit::{Item, Value};
    let bad = || PrefsEditError::BadValue {
        key: key.to_string(),
        raw: raw.to_string(),
    };
    let trimmed = raw.trim();
    if key == EDIT_TITLE_SUMMARY_TOKEN_FILE && !title_summary_token_file_looks_like_path(trimmed) {
        return Err(PrefsEditError::SensitiveBadValue {
            key: key.to_string(),
            expected: "a file path",
        });
    }
    if key == EDIT_TITLE_SUMMARY_CA_FILE && !title_summary_ca_file_looks_like_path(trimmed) {
        return Err(PrefsEditError::SensitiveBadValue {
            key: key.to_string(),
            expected: "a file path",
        });
    }
    if key == EDIT_TITLE_SUMMARY_ENDPOINT
        && !crate::title_summary::endpoint_is_credential_free_absolute_url(trimmed)
    {
        return Err(PrefsEditError::SensitiveBadValue {
            key: key.to_string(),
            expected: "a credential-free absolute HTTP(S) URL without a query or fragment",
        });
    }
    // LIST-valued keys (see [`LIST_KEYS`]): the one-line comma form is split
    // into a real TOML ARRAY — these fields deserialize as `Vec<String>`, so a
    // comma-joined STRING would fail the whole config parse on reload. Entries
    // are trimmed; empties dropped (the FontList split law); an all-empty value
    // is rejected (clear the row to unset instead). `palette` entries must each
    // parse as hex, or a typo'd colour would be silently ignored at load.
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
                .any(|e| crate::app_config::parse_hex_color(e).is_none())
        {
            return Err(bad());
        }
        let array: toml_edit::Array = entries.into_iter().collect();
        return Ok(Item::Value(Value::Array(array)));
    }
    let value = match edit_kind(key) {
        EditKind::Float => Value::from(trimmed.parse::<f64>().map_err(|_| bad())?),
        // Sign/width enforcement AFTER the numeric parse: `-1` for a `usize`
        // field or `70000` for a `u16` is perfectly parseable i64 AND valid
        // TOML, but the reload's serde model rejects the whole file over it —
        // the exact corruption this function's contract forbids — so an
        // out-of-domain integer dies here as a BadValue, never in the file.
        EditKind::Integer => {
            let n = trimmed.parse::<i64>().map_err(|_| bad())?;
            if !integer_domain(key).contains(&n) {
                return Err(bad());
            }
            Value::from(n)
        }
        EditKind::Bool => Value::from(trimmed.parse::<bool>().map_err(|_| bad())?),
        // An enum value must be one of the allowed options (case-insensitive). Store the
        // CANONICAL spelling from `options` so the file + introspection read cleanly; an
        // out-of-domain value is rejected (BadValue) rather than written as a style the
        // reload parser would silently fall back to (e.g. a typo'd "rainbwo").
        EditKind::Enum { options } => {
            // The `cursor_trail_style` row ALSO accepts a `pack:<id>` selection (a
            // loaded Trail Pack from the dynamic picker) — stored verbatim like a
            // free string, since the engine resolves it against the loaded registry
            // and fail-closes on an unknown id. Every other enum value must be one
            // of the canonical options.
            if key == EDIT_CURSOR_TRAIL_STYLE
                && trimmed
                    .strip_prefix("pack:")
                    .is_some_and(|id| !id.trim().is_empty())
            {
                Value::from(trimmed)
            } else if key == EDIT_DISPLAY_FONT && trimmed.contains('+') {
                // The `display_font` row ALSO accepts a MIX — 2..=3 distinct
                // bundled ids joined by `+` ("pixel+engraved"), authored by
                // the Text & Fonts toggles. Each part must be a real bundled
                // id (never "off" — an off mix is just a cleared key); the
                // canonical form is the CANONICALIZED parts re-joined in order,
                // so a legacy spelling is rewritten to the current one on save
                // rather than being copied back into the file.
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
                // A legacy id is AUTHORED as its current spelling, so saving a
                // pre-rename config migrates the value instead of copying the
                // old name back into the file. `mariokart` has no canonical
                // form and therefore falls through to the options check below
                // and is rejected: the WRITER is the authoring path, and
                // authoring a face aterm no longer ships should fail loudly.
                // The LOADING path still accepts it — see
                // `LEGACY_DISPLAY_FONT_IDS`.
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
        // free string) so the `dark:…,light:…` split form survives; the picker only ever
        // supplies a valid built-in name, but a hand-typed split theme is preserved too.
        EditKind::Text | EditKind::Theme => Value::from(trimmed),
        // A colour must parse as RRGGBB/#RRGGBB; reject a malformed value at Save
        // rather than write a string the config loader would silently ignore.
        EditKind::Color => {
            if crate::app_config::parse_hex_color(trimmed).is_none() {
                return Err(bad());
            }
            Value::from(trimmed)
        }
    };
    Ok(Item::Value(value))
}

/// One editable field of Settings: a human `label`, the `Config`/TOML
/// `key` it edits, its [`EditKind`] (so the overlay builds the right widget and the
/// writer types the value), the field's CURRENT raw value from the config (`seed`), and
/// the EFFECTIVE-value `placeholder` shown greyed when the control is blank.
///
/// `seed` is the user's CONFIGURED value (NOT the effective default): `None` for an
/// unset key so the control starts BLANK — clearing it back to blank then removes the
/// key on Save. For the bool field `seed` is `Some("true")`/`Some("false")` reflecting
/// the resolved state so the checkbox starts in the right position.
///
/// `placeholder` is the EFFECTIVE value (the configured value, or the built-in default
/// rendered explicitly) shown as the field's greyed placeholder text. This is the fix
/// for the "every row is blank" confusion: an UNSET key seeds a blank control (so an
/// untouched Save doesn't materialise the default), but the placeholder still tells the
/// user what value is actually in effect (e.g. `block (default)`) instead of nothing.
///
pub(crate) struct EditField {
    /// The on-screen row label.
    pub(crate) label: &'static str,
    /// The `aterm.toml` / `Config` key this row edits.
    pub(crate) key: &'static str,
    /// How the value is typed when written ([`apply_prefs_edits`]).
    pub(crate) kind: EditKind,
    /// The configured raw value to seed the control with (`None` = unset = blank).
    pub(crate) seed: Option<String>,
    /// The EFFECTIVE value shown as greyed placeholder text when the control is blank
    /// (the configured value, or the built-in default rendered explicitly).
    pub(crate) placeholder: String,
}

/// A configured string field, trimmed, with whitespace-only treated as unset (`None`).
/// Shared by `editable_fields` so the seed + placeholder agree on what counts as "set".
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
/// consuming resolvers (`sparkle_deco_config` / `matrix_rain_config` /
/// `update_auto_apply` / `net_listen`); the conformance test
/// `nested_leaves_all_have_rows_with_seed_conventions` forces an arm for every
/// registered leaf, so a new leaf cannot silently fall to the blank fallback.
fn nested_seed_placeholder(cfg: &Config, key: &str) -> (Option<String>, String) {
    let sw = cfg.sparkle_words.as_ref();
    let prof = sw.and_then(|s| s.profanity.as_ref());
    let fel = sw.and_then(|s| s.feline.as_ref());
    let orca = sw.and_then(|s| s.orca.as_ref());
    let ink = sw.and_then(|s| s.ink.as_ref());
    let emph = sw.and_then(|s| s.emphasis.as_ref());
    let mr = cfg.matrix_rain.as_ref();
    let net = cfg.net.as_ref();
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
        "net.listen" => txt(net.and_then(|n| n.listen.as_deref()), "off (no listener)"),
        "net.cert" => txt(net.and_then(|n| n.cert.as_deref()), "none"),
        "net.key" => txt(net.and_then(|n| n.key.as_deref()), "none"),
        // DERIVED, never a literal: these placeholders show the compiled-in default
        // update channel, which moved to the public mirror when
        // `[workspace.metadata.aterm] update_channel` landed. A hard-coded owner here
        // silently became a lie the moment the channel was repointed, telling the
        // user their updates come from a repo they are not actually reading.
        "update.owner" => txt(
            upd.and_then(|u| u.owner.as_deref()),
            &format!("{} (default)", aterm_update_core::DEFAULT_OWNER),
        ),
        "update.repo" => txt(
            upd.and_then(|u| u.repo.as_deref()),
            &format!("{} (default)", aterm_update_core::DEFAULT_REPO),
        ),
        "update.auto_apply" => boolean(upd.and_then(|u| u.auto_apply), true),
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
        "sparkle_words.feline.idle" => boolean(fel.and_then(|f| f.idle), true),
        "sparkle_words.feline.gaze" => boolean(fel.and_then(|f| f.gaze), true),
        "sparkle_words.feline.magic" => boolean(fel.and_then(|f| f.magic), true),
        "sparkle_words.feline.color" => {
            txt(fel.and_then(|f| f.color.as_deref()), "#f7a8b8 (default)")
        }
        "sparkle_words.feline.intensity" => num(fel.and_then(|f| f.intensity), "0.7 (default)"),
        "sparkle_words.feline.allow_bare_cat" => boolean(fel.and_then(|f| f.allow_bare_cat), true),
        "sparkle_words.feline.cjk_single_char" => {
            boolean(fel.and_then(|f| f.cjk_single_char), false)
        }
        "sparkle_words.feline.log" => boolean(fel.and_then(|f| f.log), true),
        "sparkle_words.orca.enabled" => boolean(orca.and_then(|o| o.enabled), true),
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
        "matrix_rain.hue" => txt(mr.and_then(|m| m.hue.as_deref()), "matrix (default)"),
        "matrix_rain.mutation_ms" => num(mr.and_then(|m| m.mutation_ms), "133 (default)"),
        "matrix_rain.idle_secs" => num(mr.and_then(|m| m.idle_secs), "8 (default)"),
        "matrix_rain.suppress_in_alt_screen" => {
            boolean(mr.and_then(|m| m.suppress_in_alt_screen), false)
        }
        "matrix_rain.materialize" => boolean(mr.and_then(|m| m.materialize), false),
        "matrix_rain.output_material" => boolean(mr.and_then(|m| m.output_material), true),
        "matrix_rain.turn_wave" => boolean(mr.and_then(|m| m.turn_wave), true),
        "matrix_rain.bell_alert" => boolean(mr.and_then(|m| m.bell_alert), true),
        "matrix_rain.exit_tint" => boolean(mr.and_then(|m| m.exit_tint), true),
        "matrix_rain.ink_text" => boolean(mr.and_then(|m| m.ink_text), false),
        "matrix_rain.phosphor" => boolean(mr.and_then(|m| m.phosphor), false),
        "matrix_rain.seed" => num(mr.and_then(|m| m.seed), "0 (stable per-window)"),
        // Unreachable for registered leaves — the conformance test fails any
        // NESTED_LEAVES entry that lands here (blank seed AND blank placeholder).
        _ => (None, String::new()),
    }
}

/// A settings SECTION — controls are grouped under these headers in the overlay for
/// legibility. Derived from each row's key by [`section_of`], so adding a control only
/// needs a `section_of` arm, not a per-row field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    Appearance,
    Cursor,
    /// The CURSOR KITTY's own pane (owner ask, 2026-08-10: "fix the settings and
    /// add a page for this"). The companion that walks the line is the shipped
    /// default now ([`DEFAULT_CURSOR_TRAIL_STYLE`]), and it was reachable only
    /// through a ten-option popup buried in a page that also owns bloom radius
    /// and the whole Sound menu. This section holds the keys that belong to the
    /// CAT and nothing else: which companion you get, how long its rainbow wake
    /// runs, and the sprite art it wears.
    CursorKitty,
    Typography,
    /// Window sizing, tabs, smart titles, and chrome.
    Window,
    /// Keyboard & clipboard behavior (copy-on-select, paste safety, Option-as-Meta,
    /// predictive echo).
    Input,
    Performance,
    Terminal,
    Security,
    /// The `[packages]` toolchain-manager maintenance switches. No ordinary
    /// registry page owns this section (the native Packages route is a SPECIAL
    /// page that renders these rows itself), so it surfaces through Search and
    /// the Modified review only.
    Packages,
    /// The read-only Kitty Log collection book (§F4.6): no editable keys ever
    /// map here ([`section_of`] never returns it), so the content pane paints
    /// the book instead of group-boxes and every row is non-activatable.
    KittyLog,
}

impl Section {
    /// Display order of the section headers (top → bottom).
    pub(crate) const ORDER: [Section; 11] = [
        Section::Appearance,
        Section::Cursor,
        Section::CursorKitty,
        Section::Typography,
        Section::Window,
        Section::Input,
        Section::Terminal,
        Section::Performance,
        Section::Security,
        Section::Packages,
        Section::KittyLog,
    ];

    /// The header label shown above the section's controls.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Cursor => "Cursor",
            Section::CursorKitty => "Cursor Kitty",
            Section::Typography => "Typography",
            Section::Window => "Window & Tabs",
            Section::Input => "Input",
            Section::Performance => "Performance",
            Section::Terminal => "Terminal",
            Section::Security => "Security",
            Section::Packages => "Packages",
            Section::KittyLog => "Kitty Log",
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
        return Section::Cursor;
    }
    if COLOR_KEYS.contains(&key) {
        return Section::Appearance;
    }
    // Nested tables route by PREFIX (one arm per table, not per leaf): the
    // network drive is a remote-access surface (Security, beside the opt-ins);
    // updates are app plumbing (Terminal); the two costume tables are screen
    // decorations (Appearance).
    if key.starts_with("net.") {
        return Section::Security;
    }
    if key.starts_with("update.") {
        return Section::Terminal;
    }
    // THE SOUND MENU (owner ask: "add the volume and SFX menu to settings").
    // Every audible knob in the product answers to ONE pane, so the box below
    // reads as a menu instead of rows scattered across unrelated sections. The
    // curse BONK is the exception that proves it: it lives under
    // `[sparkle_words.profanity]` in the file, but it is an SFX, so it is
    // routed here BEFORE the decorative-table prefix rule below — otherwise the
    // single loudest sparkle-words gesture would sit two panes away from the
    // volume slider that scales it.
    if SOUND_MENU_KEYS.contains(&key) {
        return Section::Cursor;
    }
    if key.starts_with("sparkle_words.") || key.starts_with("matrix_rain.") {
        return Section::Appearance;
    }
    // Robi the helper robot is a screen decoration like the two tables above.
    if key == EDIT_ROBI {
        return Section::Appearance;
    }
    // THE CURSOR KITTY'S OWN PANE, ahead of the generic cursor arms below: these
    // three keys answer to the CAT, not to the trail engine. `cursor_trail_style`
    // is the choice between the walking pet (the shipped default), the flying
    // head, another look entirely, and off; `cursor_trail_wake_ms` is documented
    // in `app_config` as the rainbow-kitty wake specifically; `cursor_nyan_sprite`
    // is the kitty's ART (it is Manual-only, so it never paints a row here — it
    // rides this section for Search/Modified grouping and the preview registry).
    if matches!(
        key,
        EDIT_CURSOR_TRAIL_STYLE | EDIT_CURSOR_TRAIL_WAKE_MS | EDIT_CURSOR_NYAN_SPRITE
    ) {
        return Section::CursorKitty;
    }
    match key {
        EDIT_THEME | EDIT_WINDOW_THEME | EDIT_WINDOW_COLORSPACE => Section::Appearance,
        // W5/SGR text-appearance knobs sit beside Theme + Colors: they all answer
        // "how does color behave on my screen", so Appearance is their matched tab.
        EDIT_SELECTION_INACTIVE
        | EDIT_MINIMUM_CONTRAST
        | EDIT_BOLD_IS_BRIGHT
        | EDIT_FAINT_OPACITY => Section::Appearance,
        // The indexed palette + M5 glass are colour-behaviour too, and the
        // wallpaper backdrop is the same surface (what the window ground shows).
        EDIT_PALETTE
        | EDIT_BACKGROUND_OPACITY
        | EDIT_BACKGROUND_MATERIAL
        | EDIT_WALLPAPER
        | EDIT_WALLPAPER_DIM
        | EDIT_WALLPAPER_TEXT_TINT => Section::Appearance,
        // Window sizing + chrome: the initial grid, the tab strip, the version
        // pill, the interior padding, and what a fresh launch reopens.
        EDIT_COLUMNS
        | EDIT_LINES
        | EDIT_TAB_STRIP_ROWS
        | EDIT_SHOW_BUILD_BADGE
        | EDIT_ACTIVE_TAB_COLOR => Section::Window,
        EDIT_WINDOW_PADDING | EDIT_WINDOW_PADDING_TOP | EDIT_RESTORE_SESSION => Section::Window,
        // Descriptive-title controls live alongside the tab/window chrome they label.
        EDIT_DESCRIPTIVE_TITLES
        | EDIT_TITLE_SUMMARY_PROVIDER
        | EDIT_TITLE_SUMMARY_MODEL
        | EDIT_TITLE_SUMMARY_ENDPOINT
        | EDIT_TITLE_SUMMARY_TOKEN_FILE
        | EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS
        | EDIT_TITLE_SUMMARY_PROXY_MODE
        | EDIT_TITLE_SUMMARY_CA_FILE
        | EDIT_TITLE_SUMMARY_INTERVAL_SECONDS
        | EDIT_TITLE_SUMMARY_CONTEXT_LINES
        | EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT
        | EDIT_TITLE_SUMMARY_ALLOW_REMOTE
        | EDIT_TAB_TITLE_FORMAT
        | EDIT_WINDOW_TITLE_FORMAT
        | EDIT_TAB_STATUS
        | EDIT_TAB_STATUS_QUIET_AFTER_MS
        | EDIT_TAB_STATUS_DWELL_MS
        | EDIT_TAB_STATUS_BADGE
        | EDIT_TAB_CONNECTION_BADGE => Section::Window,
        // Keyboard & clipboard behavior — how what you type and copy is handled.
        EDIT_COPY_ON_SELECT
        | EDIT_CONFIRM_MULTILINE_PASTE
        | EDIT_OPTION_AS_META
        | EDIT_PREDICTIVE_ECHO => Section::Input,
        // Focus-linked QoS is a performance knob, not an input one — as are the
        // launch-time renderer choice and the opt-in replay recorder.
        EDIT_FOCUS_BOOST | EDIT_GPU | EDIT_TEMPORAL_RECORDING => Section::Performance,
        // The [packages] maintenance switches live on the special Packages page;
        // this section keeps them findable (Search/Modified) without also
        // duplicating them onto an ordinary registry page.
        EDIT_PACKAGES_ENABLED
        | EDIT_PACKAGES_AUTO_UPDATE
        | EDIT_PACKAGES_AUTO_INSTALL
        | EDIT_PACKAGES_SEED_INSTALL => Section::Packages,
        EDIT_CURSOR_STYLE
        | EDIT_CURSOR_BLINK
        | EDIT_CURSOR_TRAIL
        | EDIT_CURSOR_TRAIL_MS
        | EDIT_CURSOR_TRAIL_LENGTH
        | EDIT_CURSOR_TRAIL_INTENSITY
        | EDIT_CURSOR_TRAIL_RADIUS
        | EDIT_CURSOR_TRAIL_RING
        | EDIT_CURSOR_TRAIL_BLOOM
        | EDIT_CURSOR_TRAIL_BLOOM_STRENGTH
        | EDIT_CURSOR_TRAIL_BLOOM_RADIUS
        | EDIT_CURSOR_FIRE_SHIMMER
        | EDIT_HDR_GLOW
        | EDIT_CURSOR_GLOW_SDR_BOOST
        | EDIT_MOTION
        | EDIT_SERIOUS_MODE
        // The update-celebration sparkles ride the FX page beside serious
        // mode — its "Effect policy" group is where "how much fun is this
        // terminal allowed to have" questions already live; the provisioning
        // progress card's trim answers the same question.
        | EDIT_NOTICE_SPARKLE
        | EDIT_PKG_PROGRESS_EFFECTS
        | EDIT_LOAD_ADAPTIVE_MOTION => Section::Cursor,
        // The rest of the trail/aurora surface (packs — the colour overrides
        // route via the early trail-colour return above) + the stream-fade
        // motion pair live with the cursor FX. The sound level moved to the
        // Sound menu with its siblings (see [`SOUND_MENU_KEYS`]).
        EDIT_CURSOR_TRAIL_PACKS | EDIT_STREAM_FADE | EDIT_STREAM_FADE_MS => Section::Cursor,
        // The shell program + argv are terminal-session settings.
        EDIT_SHELL | EDIT_SHELL_ARGS => Section::Terminal,
        EDIT_FONT_PX
        | EDIT_FONT_FAMILY
        | EDIT_DISPLAY_FONT
        | EDIT_LIGATURES
        | EDIT_LINE_HEIGHT
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

/// The GROUP-BOX a control belongs to inside its [`Section`] pane: `(caption, order)`
/// per the settings-v2 design §3.2 table. The caption is the uppercase group header
/// drawn above the rounded box; `order` sorts the groups top→bottom within the section
/// (fields keep their [`editable_fields`] build order within a group). Callers that
/// include an ungrouped key place it in a trailing per-section "General" box.
pub(crate) fn group_of(key: &str) -> (&'static str, u8) {
    // THE SOUND MENU, before every prefix rule below: the owner asked for ONE
    // coherent Sound surface — "the master volume slider plus the SFX toggles,
    // GROUPED so it reads as a menu rather than rows scattered across unrelated
    // boxes". This box IS that menu. It opens right after "Trail effect" (order
    // 2) because the keystroke palette is the aural half of the trail, and the
    // motion/colour/GPU boxes shift down one to make room.
    if SOUND_MENU_KEYS.contains(&key) {
        return ("Sound", 2);
    }
    if matches!(key, EDIT_CURSOR_TRAIL_COLOR | EDIT_CURSOR_TRAIL_ACCENT) {
        return ("Trail color", 4);
    }
    // The Cursor Kitty pane. "Companion" is the picker's own caption — the page
    // paints that row as its showcase card (the key is a Top Setting, so the
    // ordinary registry never draws it), and this entry only orders it first in
    // Search and Modified. "Rainbow wake" is the one group box the page paints;
    // "Kitty art" is Manual-only for the same reason and likewise never paints.
    if key == EDIT_CURSOR_TRAIL_STYLE {
        return ("Companion", 0);
    }
    if key == EDIT_CURSOR_TRAIL_WAKE_MS {
        return ("Rainbow wake", 1);
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
    // Nested tables group by PREFIX — one box per table, keeping their (many)
    // leaves out of the per-section "General" catch-all.
    if key.starts_with("net.") {
        return ("Network drive", 1);
    }
    if key.starts_with("update.") {
        return ("Updates", 3);
    }
    if key.starts_with("sparkle_words.") {
        return ("Sparkle words", 4);
    }
    if key.starts_with("matrix_rain.") {
        return ("Matrix rain", 5);
    }
    if key == EDIT_ROBI {
        return ("Robi the robot", 6);
    }
    match key {
        EDIT_THEME | EDIT_WINDOW_THEME | EDIT_WINDOW_COLORSPACE => ("Theme", 0),
        // The indexed ANSI palette edits beside the individual colour wells.
        EDIT_PALETTE => ("Colors", 1),
        // Appearance › how color behaves (contrast floor, unfocused dimming, SGR
        // brightness/faintness) — beside Theme + Colors, its matched tab.
        EDIT_MINIMUM_CONTRAST
        | EDIT_SELECTION_INACTIVE
        | EDIT_BOLD_IS_BRIGHT
        | EDIT_FAINT_OPACITY => ("Text & Contrast", 2),
        // M5 window glass (opacity + vibrancy material).
        EDIT_BACKGROUND_OPACITY | EDIT_BACKGROUND_MATERIAL => ("Transparency", 3),
        // The terminal-tab backdrop image, its legibility dim, and the
        // backdrop-hue glyph tint.
        EDIT_WALLPAPER | EDIT_WALLPAPER_DIM | EDIT_WALLPAPER_TEXT_TINT => ("Wallpaper", 4),
        // The celebration sparkles ride beside the process-wide effect switch:
        // both answer "how much fun is this terminal allowed to have".
        EDIT_SERIOUS_MODE | EDIT_NOTICE_SPARKLE | EDIT_PKG_PROGRESS_EFFECTS => {
            ("Effect policy", 0)
        }
        EDIT_CURSOR_STYLE | EDIT_CURSOR_BLINK => ("Cursor", 0),
        EDIT_CURSOR_TRAIL
        | EDIT_CURSOR_TRAIL_MS
        | EDIT_CURSOR_TRAIL_LENGTH
        | EDIT_CURSOR_TRAIL_INTENSITY
        | EDIT_CURSOR_TRAIL_RADIUS
        | EDIT_CURSOR_TRAIL_RING => ("Trail effect", 1),
        EDIT_MOTION | EDIT_LOAD_ADAPTIVE_MOTION => ("Motion", 3),
        // The extended trail surface rides the same box, after the basics
        // (colour identity and the GPU light knobs get their own boxes below).
        // The sound rows left for the "Sound" menu box above.
        EDIT_CURSOR_TRAIL_PACKS => ("Trail effect", 1),
        EDIT_CURSOR_TRAIL_BLOOM
        | EDIT_CURSOR_TRAIL_BLOOM_STRENGTH
        | EDIT_CURSOR_TRAIL_BLOOM_RADIUS
        | EDIT_CURSOR_FIRE_SHIMMER
        | EDIT_HDR_GLOW
        | EDIT_CURSOR_GLOW_SDR_BOOST => ("Light & GPU", 5),
        // M2 stream fade is its own motion pair, not part of the trail.
        EDIT_STREAM_FADE | EDIT_STREAM_FADE_MS => ("Stream fade", 6),
        // Typography splits into four scannable boxes (was one 22-row "Font" wall):
        // which faces / how glyphs join / where lines sit / how stems rasterize.
        EDIT_FONT_FAMILY
        | EDIT_FONT_PX
        | EDIT_DISPLAY_FONT
        | EDIT_FONT_FAMILY_BOLD
        | EDIT_FONT_FAMILY_ITALIC
        | EDIT_FONT_FAMILY_BOLD_ITALIC
        | EDIT_FONT_SYNTHETIC_STYLE
        | EDIT_FALLBACK_FONTS
        | EDIT_SYMBOL_FONT
        | EDIT_EMOJI_FONT => ("Font", 0),
        EDIT_LIGATURES | EDIT_CURSOR_BREAK_LIGATURES | EDIT_MERGED_LIGATURES => ("Shaping", 1),
        EDIT_LINE_HEIGHT
        | EDIT_ADJUST_BASELINE
        | EDIT_ADJUST_UNDERLINE_POSITION
        | EDIT_ADJUST_UNDERLINE_THICKNESS
        | EDIT_UNDERLINE_SKIP_DESCENDERS => ("Line layout", 2),
        EDIT_TEXT_BLENDING | EDIT_FONT_THICKEN | EDIT_STEM_GAMMA | EDIT_FONT_WEIGHT
        | EDIT_FONT_VARIATION => ("Rendering", 3),
        // OpenType features join Shaping (they drive the shaper like ligatures);
        // the dark-theme weight nudge is a rasterization knob like `font_weight`.
        EDIT_FONT_FEATURES => ("Shaping", 1),
        EDIT_FONT_WEIGHT_DARK_NUDGE => ("Rendering", 3),
        // Window › sizing, Smart Titles, tab status, interior padding, chrome,
        // then session. Tab Status sits next to Smart Titles because both answer
        // "what does this tab say about its session".
        EDIT_COLUMNS | EDIT_LINES | EDIT_TAB_STRIP_ROWS => ("Size", 0),
        EDIT_DESCRIPTIVE_TITLES
        | EDIT_TITLE_SUMMARY_PROVIDER
        | EDIT_TITLE_SUMMARY_MODEL
        | EDIT_TITLE_SUMMARY_ENDPOINT
        | EDIT_TITLE_SUMMARY_TOKEN_FILE
        | EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS
        | EDIT_TITLE_SUMMARY_PROXY_MODE
        | EDIT_TITLE_SUMMARY_CA_FILE
        | EDIT_TITLE_SUMMARY_INTERVAL_SECONDS
        | EDIT_TITLE_SUMMARY_CONTEXT_LINES
        | EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT
        | EDIT_TITLE_SUMMARY_ALLOW_REMOTE
        | EDIT_TAB_TITLE_FORMAT
        | EDIT_WINDOW_TITLE_FORMAT => ("Smart Titles", 1),
        EDIT_TAB_STATUS
        | EDIT_TAB_STATUS_QUIET_AFTER_MS
        | EDIT_TAB_STATUS_DWELL_MS
        | EDIT_TAB_STATUS_BADGE
        | EDIT_TAB_CONNECTION_BADGE => ("Tab Status", 2),
        EDIT_WINDOW_PADDING | EDIT_WINDOW_PADDING_TOP => ("Window padding", 2),
        EDIT_SHOW_BUILD_BADGE | EDIT_ACTIVE_TAB_COLOR => ("Chrome", 3),
        EDIT_RESTORE_SESSION => ("Session", 4),
        // Input › clipboard then keyboard.
        EDIT_COPY_ON_SELECT => ("Clipboard", 0),
        EDIT_CONFIRM_MULTILINE_PASTE => ("Paste safety", 0),
        EDIT_OPTION_AS_META | EDIT_PREDICTIVE_ECHO => ("Keyboard", 1),
        // Performance › focus-linked QoS, launch-time renderer choice, and replay.
        EDIT_FOCUS_BOOST | EDIT_GPU | EDIT_TEMPORAL_RECORDING => ("System", 1),
        // Packages › the toolchain-manager maintenance switches ride together.
        // `seed_install` groups here too even though the Packages page does not
        // render a fourth switch row (a fourth row overruns the compact card's
        // height budget); Search and `settings set` still reach it, and a key with
        // no group at all fails the grouping-table coherence test.
        EDIT_PACKAGES_ENABLED
        | EDIT_PACKAGES_AUTO_UPDATE
        | EDIT_PACKAGES_AUTO_INSTALL
        | EDIT_PACKAGES_SEED_INSTALL => ("Toolchain Packages", 0),
        EDIT_SCROLLBACK | EDIT_SEARCH_HISTORY_LINES => ("Scrollback", 0),
        EDIT_BIDI | EDIT_AMBIGUOUS_WIDTH => ("Text direction & width", 1),
        // Terminal › which program runs in the pane.
        EDIT_SHELL | EDIT_SHELL_ARGS => ("Shell", 2),
        _ => ("General", u8::MAX),
    }
}

/// The macOS-style FOOTNOTE under a group box, by its [`group_of`] caption (design
/// §3.2). `None` for groups the design leaves silent — footnotes explain consequences,
/// they never restate the control.
pub(crate) fn group_footnote(caption: &str) -> Option<&'static str> {
    Some(match caption {
        "Colors" => "Blank uses the theme's color.",
        "Text & Contrast" => {
            "Minimum contrast 1.0 leaves opaque colors unchanged. Translucent backgrounds enforce at least a 4.5:1 ratio."
        }
        "Transparency" => {
            "Opacity requires macOS GPU rendering; other renderers stay solid. Text over translucent backgrounds uses at least 4.5:1 contrast."
        }
        "Paste safety" => {
            "Confirm unbracketed multiline paste. Bracketed paste bypasses it. macOS asks with a sheet, Windows with a dialog, Linux with an in-window banner."
        }
        "Motion" => {
            "Automatic follows system motion when available and reduces effects under load by default. Full allows motion; Reduced limits it. Load adaptation is in Manual."
        }
        "Keyboard" => {
            "Predictive echo waits for confirmed echo and useful latency; passwords are never predicted. Manual's Always mode is unsafe at prompts."
        }
        "Scrollback" => {
            "Scrollback limit 0 is unlimited. Older lines beyond the Cmd-F/socket cap may give partial results. Set Searchable lines to 0 for live-screen-only search."
        }
        "Text direction & width" => {
            "Bidirectional mode reorders right-to-left text. Ambiguous width uses one or two cells and affects new text, not existing cells."
        }
        "Stream fade" => {
            "Fresh live-bottom output fades. It is instant with Reduce Motion, an unfocused window, full-screen apps, scrollback, input, or Serious Mode."
        }
        // The Sound box's consequence copy states the ONE fact its rows cannot:
        // this box holds TWO independent audio paths, not one. Every row except
        // [`EDIT_BELL_SOUND`] is a SYNTH voice — cue-borne, subordinate to
        // `trail_sounds` and scaled by `trail_sound_volume` (see
        // `app_render::{trail_sound_gain, sing_riff_gain, bonk_sound_gain}`).
        // The bell is the OS alert sound, emitted from `lib.rs::on_bell`, which
        // reads NEITHER of those keys — so the master switch and the volume
        // both stop at the box's edge, and a user who mutes Music effects
        // expecting silence would still be beeped at.
        //
        // Earlier copy said only that Volume misses the bell and called Music
        // effects "the master switch" flatly; that overclaimed the master, which
        // is why the scope is now spelled out.
        //
        // THE CURSE BONK USED TO BE A SECOND, UNDISCLOSED EXCEPTION (skeptic's
        // finding, 2026-08-09): `app_render::bonk_sound_gain` took no
        // `trail_sounds` input at all, so muting Music effects and typing
        // profanity still bonked. Two honest laws were available — name the
        // bonk here as a second exception, or make the code match the sentence
        // — and the code was changed, because the bonk is a synth voice by
        // every other measure (same `TrailAudio` host, same `trail_sound_volume`
        // scaling, same `SoundVoice` register) and a user who mutes "Music
        // effects" is asking for silence. The sentence below is therefore now
        // literally true of every voice, and the bell is the only exception
        // left. Pinned behaviourally by
        // `app_render::curse_bonk_drain_tests::music_effects_off_*` — the old
        // wiring test only grepped source text for accessor names and could
        // never have seen this.
        //
        // Every other caveat (silent with
        // the trail off, with the window unfocused, under Serious Mode, or on a
        // platform without audio) already has a per-row disclosure
        // (`native_settings::settings_effect_note`), and this string is
        // budgeted: a long footnote can consume a whole 320 pt page at 2× text
        // and leave no room for a control
        // (`advanced_group_footnotes_are_native_semantic_and_fit_responsive_layouts`).
        "Sound" => {
            "Music effects in Top Settings is the master switch for the synth voices; Volume scales them. Neither reaches the terminal bell's system alert sound."
        }
        // The Cursor Kitty box's consequence copy states the two facts its one
        // row cannot. (a) `0` is a real, useful value — it hides the plume and
        // KEEPS the cat, which a bare 0..1500 slider reads as "off" — see
        // `cursor_glow::rainbow_wake_persistence_is_a_host_dial_that_fails_off`.
        // (b) The dial is a rainbow-style dial: `GlowConfig::wake_persist_s`
        // only reaches the rainbow ribbon's wake, so on `comet`/`fire`/`beam`
        // it moves nothing. The upstream gates (Serious Mode, Reduced motion,
        // an unfocused window, `cursor_trail = false`) each already carry a
        // per-row `motion_suppression` disclosure, so they are not repeated.
        // BUDGETED to five wrapped lines at 2× Dynamic Type on a 320pt page:
        // one row plus a six-line footnote overflowed its own group box there
        // (`advanced_group_footnotes_…`), and a footnote that paints past the
        // page is worse than a shorter one.
        "Rainbow wake" => {
            "How much recent typing shows as a plume; 0 hides the plume and keeps the cat. Rainbow styles only."
        }
        "Trail color" => {
            "Blank colors follow the active terminal theme; Nyan uses its built-in sprite."
        }
        "Light & GPU" => {
            "Bloom, shimmer, and HDR gracefully fall back when the display path cannot provide them."
        }
        "Rendering" => "Variable weight needs a font with a wght axis; static fonts ignore it.",
        "Matrix rain" => {
            "Rain follows activity and drains when idle. View ▸ Matrix Rain overrides one session. Serious Mode and Reduce Motion disable it."
        }
        "Toolchain Packages" => {
            "Maintenance controls the background service. It and auto-update apply next launch. Auto-install runs next package operation and may fetch multiple GB. A batteries-included install lays down the bundled toolchain on first launch (the bytes ship inside the app); [packages] seed_install = false in aterm.toml turns that into an offer."
        }
        "Smart Titles" => {
            "Activity is a generated fallback when a session has no authored Description. Built-in stays on-device. On macOS, aterm auto-starts Ollama only after every file in its bounded runtime code closure passes pinned structural-signature, Apple Developer-ID Team, code-identifier, ownership, permission, and stable-identity checks; it repeats the closure check before terminal context is sent, clears inherited environment, disables cloud integration, and uses direct loopback. A pre-existing localhost service and every custom service remain untrusted network providers and require explicit consent. Other platforms never auto-execute a managed runtime without a platform attestation anchor. Environment proxy honors HTTP(S)_PROXY and NO_PROXY; Direct bypasses them. For HTTPS OpenAI-compatible endpoints, an explicit CA bundle replaces platform roots. Recent terminal text may be sent. Credential filtering is conservative but heuristic and cannot identify every secret; use Built-in or managed local Ollama when terminal context must stay on-device. Credentials and certificates are path-only—never stored here."
        }
        "Permissions" => {
            "Off by default. Programs request access; Secure Keyboard Entry stops \
             snooping. Notifications: macOS and Windows."
        }
        "Window padding" => {
            "Top padding cannot exceed all-edge padding; constrained values show their effective size."
        }
        _ => return None,
    })
}

/// When an authored value becomes effective. Most preferences are projected
/// live. Keep this metadata beside the complete preference schema so Advanced,
/// Modified, and the Manual language service cannot disagree about a saved
/// value's lifecycle.
pub(crate) fn application_timing(key: &str) -> Option<&'static str> {
    match key {
        EDIT_COLUMNS | EDIT_LINES => Some(
            "Applies on a fresh launch; an authenticated update handoff preserves the live size",
        ),
        EDIT_GPU
        | EDIT_PACKAGES_ENABLED
        | EDIT_PACKAGES_AUTO_UPDATE
        | "net.listen"
        | "net.cert"
        | "net.key" => Some("Applies next launch"),
        EDIT_ALLOW_KITTY_FILE_TRANSFER | EDIT_TEMPORAL_RECORDING | EDIT_SHELL | EDIT_SHELL_ARGS => {
            Some("Applies to new sessions")
        }
        EDIT_HDR_GLOW => Some("Disabling applies now; enabling may require a new window"),
        EDIT_RESTORE_SESSION => Some("Applies when closing or next launch"),
        EDIT_PACKAGES_AUTO_INSTALL
        | EDIT_PACKAGES_SEED_INSTALL
        | "packages.account"
        | "packages.channel"
        | "packages.include"
        | "packages.exclude"
        | "packages.links" => Some("Applies on the next package operation"),
        "update.owner" | "update.repo" => {
            Some("Manual checks use this now; automatic checks use it next launch")
        }
        "update.auto_apply" => Some("Applies on the next update transition"),
        EDIT_MATRIX_RAIN_ENABLED => {
            Some("Applies live unless this session has a View menu override")
        }
        EDIT_AMBIGUOUS_WIDTH => {
            Some("Applies to newly received text; existing cells keep their current width")
        }
        key if key == "net.connections" || key.starts_with("net.connections.") => {
            Some("Applies on the next dial")
        }
        _ => None,
    }
}

/// Whether saving this key has any immediate effect in the running app.
///
/// Most keys with an [`application_timing`] disclosure are wholly deferred.
/// Ambiguous-width policy is live for subsequent input while preserving
/// existing cell geometry; Matrix Rain is live unless a session-local View
/// override owns that session's effective switch; disabling HDR glow gates the
/// next present immediately, while enabling may need a new HDR-capable window.
pub(crate) fn application_has_live_effect(key: &str) -> bool {
    matches!(
        key,
        EDIT_AMBIGUOUS_WIDTH | EDIT_MATRIX_RAIN_ENABLED | EDIT_HDR_GLOW
    ) || application_timing(key).is_none()
}

/// Ambient precedence documented beside the schema. Active values are resolved
/// by `app_config::active_environment_override`; this text also teaches Manual
/// users why a future launch may not use the TOML value they are editing.
pub(crate) fn environment_precedence(key: &str) -> Option<&'static str> {
    Some(match key {
        EDIT_COLUMNS => {
            "$ATERM_COLUMNS / --columns overrides on a fresh launch; an authenticated update handoff preserves the live grid"
        }
        EDIT_LINES => {
            "$ATERM_LINES / --lines overrides on a fresh launch; an authenticated update handoff preserves the live grid"
        }
        EDIT_GPU => {
            "the last --cpu/--gpu flag wins; inherited $ATERM_CPU otherwise wins over $ATERM_GPU; both override this value"
        }
        EDIT_FONT_PX => "$ATERM_FONT_PX / --font-px overrides this value",
        EDIT_FONT_FAMILY => "$ATERM_FONT / --font overrides this value",
        EDIT_WINDOW_THEME => "on macOS, $ATERM_NO_DARK_CHROME forces Automatic for this launch",
        EDIT_TAB_STRIP_ROWS => "$ATERM_TAB_STRIP_ROWS overrides this value",
        EDIT_STEM_GAMMA => "$ATERM_STEM_GAMMA overrides this value",
        EDIT_SHELL => {
            "$ATERM_SHELL / --shell overrides this value; -e / --command bypasses the shell"
        }
        EDIT_SHELL_ARGS => "a launch -e / --command bypasses shell_args",
        "net.listen" => "$ATERM_NET_LISTEN overrides this value",
        "net.cert" => "$ATERM_NET_CERT overrides this value",
        "net.key" => "$ATERM_NET_KEY overrides this value",
        "update.owner" => "$ATERM_UPDATE_OWNER overrides this value",
        "update.repo" => "$ATERM_UPDATE_REPO overrides this value",
        "update.auto_apply" => "$ATERM_NO_AUTO_APPLY forces this off for the launch",
        "packages.account" => "$ATPKG_ACCOUNT overrides this value for package operations",
        EDIT_PACKAGES_AUTO_UPDATE => {
            "$ATPKG_UPDATE_INTERVAL_SECS controls cadence only (default 21600 seconds; 0 runs once); it never overrides packages.enabled or packages.auto_update"
        }
        EDIT_FALLBACK_FONTS => "when unset, deprecated $ATERM_FALLBACK_FONT supplies the fallback",
        EDIT_SYMBOL_FONT => "when unset, deprecated $ATERM_SYMBOL_FONT supplies the fallback",
        EDIT_EMOJI_FONT => "when unset, deprecated $ATERM_EMOJI_FONT supplies the fallback",
        _ => return None,
    })
}

/// Human label for a security opt-in row (keyed by its `SECURITY_BOOL_KEYS` entry).
fn security_label(key: &str) -> &'static str {
    match key {
        EDIT_ALLOW_OSC52_QUERY => "Allow programs to read the clipboard (OSC 52)",
        EDIT_SECURE_KEYBOARD_ENTRY => "Secure Keyboard Entry (block keystroke snooping)",
        EDIT_ALLOW_WINDOW_OPS => {
            if cfg!(target_os = "linux") {
                // The manipulation half is wired there (frame audit #4).
                "Allow window control & size queries (XTWINOPS)"
            } else {
                "Allow title / text-grid-size queries (XTWINOPS)"
            }
        }
        EDIT_ALLOW_NOTIFICATIONS => "Allow desktop notifications",
        EDIT_ALLOW_PALETTE_RECONFIGURE => "Allow programs to set indexed colors (OSC 4/21)",
        EDIT_ALLOW_KITTY_FILE_TRANSFER => "Allow local files for Kitty graphics (new sessions)",
        _ => "Security option",
    }
}

/// Truthful copy for `motion = "auto"` differs with the shipping platform
/// seam: macOS observes Reduce Motion live, Windows samples its animations
/// switch when a window attaches, and other platforms have no OS query yet.
fn motion_auto_copy(target_os: &str) -> (&'static str, &'static str) {
    match target_os {
        "macos" => (
            "auto (follows live macOS Reduce Motion)",
            "motion=auto follows macOS Reduce Motion live",
        ),
        "windows" => (
            "auto (samples Windows animations at window attach)",
            "motion=auto samples Show animations in Windows when a window attaches; preference changes are not observed live",
        ),
        _ => (
            "auto (OS Reduce Motion unavailable; no OS-driven reduction)",
            "motion=auto cannot query OS Reduce Motion on this platform and does not reduce motion for an OS preference; choose reduced explicitly",
        ),
    }
}

pub(crate) fn motion_auto_placeholder() -> &'static str {
    motion_auto_copy(std::env::consts::OS).0
}

pub(crate) fn motion_auto_help() -> &'static str {
    motion_auto_copy(std::env::consts::OS).1
}

/// Inclusive numeric bounds + step for a BOUNDED numeric control, so the settings UI
/// draws a slider (and the a11y tree exposes min/max/value). `None` ⇒ the value is
/// UNBOUNDED and is edited as a free-form numeric field/stepper instead of a slider.
/// Keep an arm for every bounded [`EditKind::Float`]/[`EditKind::Integer`] key; the
/// `range_of_covers_bounded_numerics_only` test enforces that each numeric key is
/// deliberately classified as one or the other.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Range {
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) step: f64,
}

pub(crate) fn range_of(key: &str) -> Option<Range> {
    let r = |min, max, step| Some(Range { min, max, step });
    match key {
        // Full runtime/CLI domain. Keeping only an ergonomic 6..=32 subset here
        // made valid high-DPI/accessibility values look invalid in Manual and
        // impossible to author through the native control.
        EDIT_FONT_PX => r(6.0, 200.0, 1.0),
        // The resolver clamps 30..=2000 (`cursor_trail_ms_or_default`), so the
        // slider floor is 30, not 0 — a 0 the slider could express would silently
        // load as 30 (turning the trail OFF is the style row's `off`, not ms 0).
        // Ten milliseconds keeps both the shipped 260 ms effective default and
        // the authored 30/2000 ms endpoints on the slider's exact value grid.
        EDIT_CURSOR_TRAIL_MS => r(30.0, 2000.0, 10.0),
        EDIT_CURSOR_TRAIL_LENGTH => r(1.0, 512.0, 1.0),
        // The typing wake spans OFF (0) to a long 1.5 s of travel. Unlike the
        // comet duration above, 0 is a real, reachable setting here — it is
        // how you keep the rainbow ribbon and drop the plume.
        EDIT_CURSOR_TRAIL_WAKE_MS => r(0.0, 1500.0, 25.0),
        EDIT_CURSOR_TRAIL_INTENSITY => r(0.0, 1.0, 0.05),
        EDIT_CURSOR_TRAIL_RADIUS => r(0.0, 2.0, 0.05),
        EDIT_CURSOR_TRAIL_BLOOM_STRENGTH => r(0.0, 3.0, 0.05),
        EDIT_CURSOR_TRAIL_BLOOM_RADIUS => r(0.5, 8.0, 0.1),
        EDIT_CURSOR_GLOW_SDR_BOOST => r(0.0, 1.0, 0.05),
        EDIT_TAB_STRIP_ROWS => r(0.0, 4.0, 1.0),
        // Smart-title cadence/context bounds match the config resolvers. Keeping the
        // UI on the same domain means its displayed value is exactly what reload uses.
        EDIT_TITLE_SUMMARY_INTERVAL_SECONDS => r(
            MIN_TITLE_SUMMARY_INTERVAL_SECONDS as f64,
            MAX_TITLE_SUMMARY_INTERVAL_SECONDS as f64,
            1.0,
        ),
        EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS => r(
            MIN_TITLE_SUMMARY_TIMEOUT_SECONDS as f64,
            MAX_TITLE_SUMMARY_TIMEOUT_SECONDS as f64,
            1.0,
        ),
        EDIT_TITLE_SUMMARY_CONTEXT_LINES => r(
            MIN_TITLE_SUMMARY_CONTEXT_LINES as f64,
            MAX_TITLE_SUMMARY_CONTEXT_LINES as f64,
            1.0,
        ),
        // Tab-status policy, on the resolver's own domain. Step 50ms: finer than
        // that is below the observation budget and could not be observed anyway.
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
        // W5 typography/appearance sliders — the same bounds the config
        // resolvers clamp to, so the slider can't express an out-of-range value.
        EDIT_LINE_HEIGHT => r(0.8, 2.0, 0.05),
        EDIT_MINIMUM_CONTRAST => r(1.0, 21.0, 0.5),
        EDIT_FAINT_OPACITY => r(0.0, 1.0, 0.05),
        EDIT_ADJUST_BASELINE => r(-32.0, 32.0, 1.0),
        // W7 underline escape hatches — same ±32 px domain the config clamps to.
        EDIT_ADJUST_UNDERLINE_POSITION | EDIT_ADJUST_UNDERLINE_THICKNESS => r(-32.0, 32.0, 1.0),
        // W2 aesthetic stem gamma — the renderer's clamp domain (<1 thicker, >1 thinner).
        EDIT_STEM_GAMMA => r(0.30, 3.0, 0.05),
        // W9 primary `wght` — exactly the renderer's 1..=1000 clamp domain.
        // Unit steps preserve every already-authored integer (including 400)
        // instead of forcing the slider onto a min-relative 25-unit grid.
        EDIT_FONT_WEIGHT => r(1.0, 1000.0, 1.0),
        // The trail/aurora numerics — each mirrors its resolver's clamp domain
        // exactly, so the slider can never express a value the load would
        // rewrite (the comet-geometry/bloom/SDR-glow arms are above).
        EDIT_TRAIL_SOUND_VOLUME => r(0.0, 1.0, 0.05),
        // M2 stream-fade window (resolver clamps 16..=1000).
        EDIT_STREAM_FADE_MS => r(16.0, 1000.0, 10.0),
        // W9 dark-theme nudge (config clamps 0..=300).
        EDIT_FONT_WEIGHT_DARK_NUDGE => r(0.0, 300.0, 10.0),
        // M5 glass (resolver clamps 0..=1).
        EDIT_BACKGROUND_OPACITY => r(0.0, 1.0, 0.05),
        // Wallpaper legibility dim (resolver clamps 0..=1).
        EDIT_WALLPAPER_DIM => r(0.0, 1.0, 0.05),
        // Interior window padding, logical px (resolver clamps 0..=64; the top
        // override is additionally capped at the base pad AT THE RESOLVER, so
        // the slider spans the full static domain and the resolver owns the
        // pairwise cap).
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
        // Nested matrix-rain numerics — `matrix_rain_config`'s clamp domains.
        "matrix_rain.fps" => r(12.0, 60.0, 1.0),
        "matrix_rain.density" => r(1.0, 12.0, 1.0),
        "matrix_rain.speed" | "matrix_rain.trail" => r(1.0, 10.0, 1.0),
        "matrix_rain.alpha" | "matrix_rain.head_alpha" => r(16.0, 135.0, 1.0),
        "matrix_rain.mutation_ms" => r(80.0, 2000.0, 10.0),
        "matrix_rain.idle_secs" => r(2.0, 120.0, 1.0),
        // scrollback_lines / columns / lines / search_history_lines /
        // matrix_rain.seed are open-ended — a slider can't span them sensibly,
        // so they edit as free-form numeric fields.
        _ => None,
    }
}

/// Extra search keywords for a control so a fuzzy query finds it by INTENT, not only by
/// its label (e.g. "dark mode" → `window_theme`, "history" → `scrollback_lines`). The
/// label, key, and section name are always part of the search corpus; this augments
/// them. An unknown key contributes no extra keywords (empty slice).
pub(crate) fn keywords_of(key: &str) -> &'static [&'static str] {
    match key {
        EDIT_WINDOW_THEME => &["dark", "light", "mode", "appearance"],
        EDIT_THEME => &["colors", "colour", "scheme", "palette"],
        EDIT_SCROLLBACK => &["history", "buffer", "lines", "scroll"],
        EDIT_COPY_ON_SELECT => &["clipboard", "selection", "mouse"],
        EDIT_LIGATURES => &["font", "programming", "arrows"],
        EDIT_CURSOR_TRAIL
        | EDIT_CURSOR_TRAIL_STYLE
        | EDIT_CURSOR_TRAIL_MS
        | EDIT_CURSOR_TRAIL_LENGTH
        | EDIT_CURSOR_TRAIL_INTENSITY
        | EDIT_CURSOR_TRAIL_RADIUS
        | EDIT_CURSOR_TRAIL_RING => &["effect", "motion", "comet", "trail"],
        // "nyan" stays alongside "kitty"/"rainbow": search keywords are DISCOVERY
        // aliases, and a user who knows the effect by its old name must still find
        // the row.
        EDIT_CURSOR_TRAIL_WAKE_MS => &[
            "effect", "motion", "trail", "wake", "typing", "kitty", "rainbow", "nyan", "plume",
        ],
        EDIT_CURSOR_TRAIL_COLOR | EDIT_CURSOR_TRAIL_ACCENT => {
            &["effect", "trail", "color", "colour", "aurora", "accent"]
        }
        EDIT_TRAIL_SOUNDS | EDIT_TRAIL_SOUND_VOLUME => {
            &["sound", "audio", "sfx", "mute", "volume", "effects"]
        }
        EDIT_TONE_MELODY => &["tone", "mood", "melody", "music", "classifier", "sound"],
        EDIT_TRAIL_SOUND_BED => &["bed", "ambient", "drone", "texture", "background", "sound"],
        // Discovery aliases for the riff: a user hunting it will type what they
        // HEAR ("song", "loud"), not the internal gesture name.
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
        EDIT_CURSOR_TRAIL_PACKS => &["trail", "pack", "manifest", "custom", "effect"],
        EDIT_CURSOR_TRAIL_BLOOM
        | EDIT_CURSOR_TRAIL_BLOOM_STRENGTH
        | EDIT_CURSOR_TRAIL_BLOOM_RADIUS => &["bloom", "halo", "glow", "gpu", "blur", "trail"],
        EDIT_CURSOR_FIRE_SHIMMER => &["fire", "shimmer", "heat", "haze", "gpu", "refraction"],
        EDIT_HDR_GLOW => &["hdr", "edr", "display", "glow", "bright", "luminous"],
        EDIT_CURSOR_GLOW_SDR_BOOST => &["sdr", "glow", "boost", "crown", "brightness"],
        EDIT_STREAM_FADE | EDIT_STREAM_FADE_MS => &["fade", "ink", "stream", "output", "motion"],
        EDIT_GPU => &["gpu", "renderer", "metal", "vulkan", "cpu", "restart"],
        EDIT_PALETTE => &["palette", "ansi", "colors", "colours", "indexed"],
        EDIT_SHELL | EDIT_SHELL_ARGS => &["shell", "bash", "zsh", "command", "login", "argv"],
        EDIT_WINDOW_COLORSPACE => &["colorspace", "srgb", "p3", "gamut", "color"],
        EDIT_RESTORE_SESSION => &["restore", "session", "reopen", "launch", "windows"],
        EDIT_FONT_FEATURES => &["opentype", "features", "ss01", "zero", "stylistic"],
        EDIT_FONT_WEIGHT_DARK_NUDGE => &["weight", "dark", "nudge", "wght", "theme"],
        EDIT_BACKGROUND_OPACITY | EDIT_BACKGROUND_MATERIAL => &[
            "opacity",
            "transparent",
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
        EDIT_WINDOW_PADDING | EDIT_WINDOW_PADDING_TOP => {
            &["padding", "margin", "border", "spacing", "inset", "edge"]
        }
        EDIT_CONFIRM_MULTILINE_PASTE => &["paste", "safety", "multiline"],
        EDIT_OPTION_AS_META => &["alt", "meta", "keyboard"],
        EDIT_BIDI => &["rtl", "arabic", "hebrew", "bidirectional"],
        EDIT_AMBIGUOUS_WIDTH => &["east", "asian", "cjk", "width"],
        EDIT_PREDICTIVE_ECHO => &["typing", "latency", "prediction"],
        EDIT_FOCUS_BOOST => &["priority", "qos", "latency", "windows", "starvation"],
        EDIT_MATRIX_RAIN_ENABLED => &[
            "matrix",
            "rain",
            "phosphor",
            "effect",
            "screensaver",
            "green",
        ],
        EDIT_PACKAGES_ENABLED => &[
            "packages",
            "toolchain",
            "atpkg",
            "tools",
            "automatic",
            "maintenance",
            "master",
        ],
        EDIT_PACKAGES_AUTO_UPDATE => &["packages", "toolchain", "atpkg", "tools", "alab"],
        EDIT_PACKAGES_SEED_INSTALL => &[
            "packages",
            "toolchain",
            "atpkg",
            "seed",
            "bundled",
            "batteries",
            "offline",
            "first run",
            "alab",
        ],
        EDIT_PACKAGES_AUTO_INSTALL => &[
            "packages",
            "toolchain",
            "atpkg",
            "install",
            "bootstrap",
            "alab",
        ],
        EDIT_FONT_FAMILY | EDIT_FONT_PX => &["typeface", "size", "text"],
        EDIT_FONT_FAMILY_BOLD | EDIT_FONT_FAMILY_ITALIC | EDIT_FONT_FAMILY_BOLD_ITALIC => {
            &["bold", "italic", "weight", "style", "typeface"]
        }
        EDIT_FONT_SYNTHETIC_STYLE => &["synthetic", "bold", "italic", "fake", "dilate"],
        EDIT_FALLBACK_FONTS => &["fallback", "unicode", "cjk", "chain", "coverage"],
        EDIT_SYMBOL_FONT => &["symbols", "math", "glyphs", "fallback"],
        EDIT_EMOJI_FONT => &["emoji", "color", "colour", "fallback"],
        EDIT_LINE_HEIGHT => &["spacing", "leading", "rows", "height"],
        EDIT_ADJUST_BASELINE => &["baseline", "vertical", "shift"],
        EDIT_ADJUST_UNDERLINE_POSITION | EDIT_ADJUST_UNDERLINE_THICKNESS => {
            &["underline", "position", "thickness", "decoration"]
        }
        EDIT_UNDERLINE_SKIP_DESCENDERS => &["underline", "descender", "skip", "ink", "gap"],
        EDIT_MINIMUM_CONTRAST => &["contrast", "legibility", "wcag", "accessibility"],
        EDIT_SELECTION_INACTIVE => &["selection", "focus", "dim", "unfocused"],
        EDIT_CURSOR_BREAK_LIGATURES => &["ligature", "cursor", "break"],
        EDIT_MERGED_LIGATURES => &["ligature", "merged", "cascadia", "collapse", "n:1"],
        EDIT_BOLD_IS_BRIGHT => &["bold", "bright", "ansi", "promote"],
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
        EDIT_ROBI => &[
            "robi", "robot", "helper", "tips", "monkey", "bars", "ladder", "show",
        ],
        EDIT_NOTICE_SPARKLE => &[
            "sparkle",
            "celebration",
            "update",
            "notice",
            "rainbow",
            "confetti",
            "badge",
        ],
        // "kitty" deliberately absent: the cursor-companion searches
        // (`wide_search_page_one_lists_native_rows_beside_the_manual_result`
        // pins the "kitty" roster) belong to the trail/companion settings;
        // "cat" already lands anyone hunting the walker on the bar.
        EDIT_PKG_PROGRESS_EFFECTS => &[
            "progress",
            "install",
            "toolchain",
            "packages",
            "rainbow",
            "sparkle",
            "cat",
        ],
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
        k if SECURITY_BOOL_KEYS.contains(&k) => &["security", "permission", "allow"],
        EDIT_SHOW_BUILD_BADGE => &["version", "build", "badge", "chrome"],
        EDIT_DESCRIPTIVE_TITLES => &[
            "smart title",
            "smart",
            "live",
            "activity",
            "summary",
            "description",
            "tab",
            "window",
        ],
        EDIT_TITLE_SUMMARY_PROVIDER => &[
            "smart title",
            "ai",
            "llm",
            "local",
            "ollama",
            "openai",
            "service",
            "provider",
            "activity",
        ],
        EDIT_TITLE_SUMMARY_MODEL => &[
            "smart title",
            "activity",
            "ai",
            "llm",
            "model",
            "qwen",
            "ollama",
        ],
        EDIT_TITLE_SUMMARY_ENDPOINT => &[
            "smart title",
            "url",
            "server",
            "host",
            "ollama",
            "openai",
            "service",
            "activity",
        ],
        EDIT_TITLE_SUMMARY_TOKEN_FILE => &[
            "smart title",
            "api",
            "key",
            "credential",
            "secret",
            "authentication",
            "file",
            "path",
            "activity",
        ],
        EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS => &[
            "smart title",
            "activity",
            "request",
            "timeout",
            "network",
            "seconds",
        ],
        EDIT_TITLE_SUMMARY_PROXY_MODE => &[
            "smart title",
            "activity",
            "proxy",
            "environment",
            "direct",
            "http_proxy",
            "https_proxy",
            "no_proxy",
        ],
        EDIT_TITLE_SUMMARY_CA_FILE => &[
            "smart title",
            "activity",
            "tls",
            "https",
            "certificate",
            "ca",
            "pem",
            "trust",
            "file",
            "path",
        ],
        EDIT_TITLE_SUMMARY_INTERVAL_SECONDS => &[
            "smart title",
            "refresh",
            "cadence",
            "frequency",
            "seconds",
            "update",
            "live",
            "activity",
        ],
        EDIT_TITLE_SUMMARY_CONTEXT_LINES => &[
            "smart title",
            "context",
            "recent",
            "history",
            "lines",
            "prompt",
            "terminal",
            "activity",
        ],
        EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT => &[
            "smart title",
            "privacy",
            "terminal",
            "output",
            "command",
            "context",
            "summary",
            "activity",
        ],
        EDIT_TITLE_SUMMARY_ALLOW_REMOTE => &[
            "smart title",
            "privacy",
            "network",
            "remote",
            "cloud",
            "consent",
            "send",
            "unattested",
            "trust",
            "activity",
        ],
        EDIT_TAB_TITLE_FORMAT | EDIT_WINDOW_TITLE_FORMAT => &[
            "smart title",
            "title",
            "description",
            "format",
            "layout",
            "order",
            "tab",
            "window",
            "activity",
        ],
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
        // Nested tables share one intent vocabulary per table.
        k if k.starts_with("net.") => &["network", "remote", "drive", "listener", "tls"],
        k if k.starts_with("update.") => &["update", "channel", "github", "release"],
        // The bonk keys are sparkle-words leaves in the file but SFX in the UI:
        // give them the sound vocabulary too, or the one sparkle gesture a user
        // searches for by ear ("bonk", "sound") would be findable only under
        // "sparkle".
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
pub(crate) fn editable_fields(cfg: &Config) -> Vec<EditField> {
    let font_family = configured_str(cfg.font_family.as_deref());
    let display_font = configured_str(cfg.display_font.as_deref());
    let active_tab_color = configured_str(cfg.active_tab_color.as_deref());
    let font_family_bold = configured_str(cfg.font_family_bold.as_deref());
    let font_family_italic = configured_str(cfg.font_family_italic.as_deref());
    let font_family_bold_italic = configured_str(cfg.font_family_bold_italic.as_deref());
    // The Settings editor edits `fallback_fonts` as one comma-separated string
    // (the FontList string form, which the config loader parses back), so the
    // list round-trips through the plain Text control.
    let fallback_fonts = cfg
        .fallback_fonts
        .as_ref()
        .filter(|l| !l.0.is_empty())
        .map(|l| l.0.join(", "));
    let symbol_font = configured_str(cfg.symbol_font.as_deref());
    let emoji_font = configured_str(cfg.emoji_font.as_deref());
    // The variable-font axes round-trip as one comma-joined string (the FontList
    // form the loader parses back), like `fallback_fonts`.
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
    let cursor_nyan_sprite = configured_str(cfg.cursor_nyan_sprite.as_deref());
    let foreground = configured_str(cfg.foreground.as_deref());
    let background = configured_str(cfg.background.as_deref());
    let cursor_color = configured_str(cfg.cursor_color.as_deref());
    let selection_color = configured_str(cfg.selection_color.as_deref());
    let selection_foreground = configured_str(cfg.selection_foreground.as_deref());
    let predictive_echo = configured_str(cfg.predictive_echo.as_deref());
    let title_summary_provider = cfg
        .title_summary_provider
        .as_ref()
        .map(|provider| provider.as_str().to_string());
    let title_summary_model = configured_str(cfg.title_summary_model.as_deref());
    let title_summary_endpoint = configured_str(cfg.title_summary_endpoint.as_deref());
    let title_summary_token_file = configured_str(cfg.title_summary_token_file.as_deref());
    let title_summary_proxy_mode = cfg
        .title_summary_proxy_mode
        .map(|mode| mode.as_str().to_string());
    let title_summary_ca_file = configured_str(cfg.title_summary_ca_file.as_deref());
    let tab_title_format = cfg
        .tab_title_format
        .as_ref()
        .map(|format| format.as_str().to_string());
    let window_title_format = cfg
        .window_title_format
        .as_ref()
        .map(|format| format.as_str().to_string());
    let mut fields = vec![
        // Family before size — the Typography "Font" group-box order (design §3.2).
        EditField {
            label: "Font family",
            key: EDIT_FONT_FAMILY,
            kind: EditKind::Text,
            placeholder: font_family.clone().unwrap_or_else(|| "default".to_string()),
            seed: font_family,
        },
        EditField {
            label: "Font size",
            key: EDIT_FONT_PX,
            kind: EditKind::Float,
            seed: cfg.font_px.map(|px| format!("{px}")),
            placeholder: match cfg.font_px {
                Some(px) => format!("{px} px"),
                None => "auto (default)".to_string(),
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
                .unwrap_or_else(|| "auto (discovered)".to_string()),
            seed: font_family_bold,
        },
        EditField {
            label: "Italic font family",
            key: EDIT_FONT_FAMILY_ITALIC,
            kind: EditKind::Text,
            placeholder: font_family_italic
                .clone()
                .unwrap_or_else(|| "auto (discovered)".to_string()),
            seed: font_family_italic,
        },
        EditField {
            label: "Bold italic font family",
            key: EDIT_FONT_FAMILY_BOLD_ITALIC,
            kind: EditKind::Text,
            placeholder: font_family_bold_italic
                .clone()
                .unwrap_or_else(|| "auto (discovered)".to_string()),
            seed: font_family_bold_italic,
        },
        EditField {
            label: "Synthetic bold/italic",
            key: EDIT_FONT_SYNTHETIC_STYLE,
            kind: EditKind::Bool,
            // Checkbox reflects the RESOLVED state (default ON: styles are
            // synthesized when no real face exists).
            seed: Some(cfg.font_synthetic_style.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Fallback fonts",
            key: EDIT_FALLBACK_FONTS,
            kind: EditKind::Text,
            placeholder: fallback_fonts
                .clone()
                .unwrap_or_else(|| "system discovery".to_string()),
            seed: fallback_fonts,
        },
        EditField {
            label: "Symbol font",
            key: EDIT_SYMBOL_FONT,
            kind: EditKind::Text,
            placeholder: symbol_font
                .clone()
                .unwrap_or_else(|| "system discovery".to_string()),
            seed: symbol_font,
        },
        EditField {
            label: "Emoji font",
            key: EDIT_EMOJI_FONT,
            kind: EditKind::Text,
            placeholder: emoji_font
                .clone()
                .unwrap_or_else(|| "system discovery".to_string()),
            seed: emoji_font,
        },
        EditField {
            label: "Ligatures",
            key: EDIT_LIGATURES,
            kind: EditKind::Bool,
            // Checkbox reflects the RESOLVED state (default ON); Save writes the
            // explicit bool. The checkbox shows its state directly, so no placeholder.
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
                None => "1.0 (default)".to_string(),
            },
        },
        EditField {
            label: "Adjust baseline (px)",
            key: EDIT_ADJUST_BASELINE,
            kind: EditKind::Integer,
            seed: cfg.adjust_baseline.map(|v| v.to_string()),
            placeholder: cfg
                .adjust_baseline
                .map_or_else(|| "0 (default)".to_string(), |v| v.to_string()),
        },
        EditField {
            label: "Adjust underline position (px)",
            key: EDIT_ADJUST_UNDERLINE_POSITION,
            kind: EditKind::Integer,
            seed: cfg.adjust_underline_position.map(|v| v.to_string()),
            placeholder: cfg
                .adjust_underline_position
                .map_or_else(|| "0 (default)".to_string(), |v| v.to_string()),
        },
        EditField {
            label: "Adjust underline thickness (px)",
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
            // The variable-font axes edit as ONE comma-joined string (the
            // FontList form the loader parses back), so the list round-trips
            // through the plain Text control — exactly like `fallback_fonts`.
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
            label: "Cursor style",
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
            // Default ON (the checkbox reflects the resolved state directly).
            seed: Some(cfg.cursor_blink.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Cursor trail",
            key: EDIT_CURSOR_TRAIL,
            kind: EditKind::Bool,
            // Master on/off for the cursor wake (default ON). The checkbox reflects the
            // RESOLVED state directly, so no placeholder.
            seed: Some(cfg.cursor_trail_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // ONE popup row over the whole style list (design graft #1) — while its
            // menu is open the preview card's demo lane plays the HIGHLIGHTED look,
            // so browsing the menu live-demos each effect before anything commits.
            label: "Cursor trail",
            key: EDIT_CURSOR_TRAIL_STYLE,
            kind: EditKind::Enum {
                options: CURSOR_TRAIL_STYLES,
            },
            placeholder: cursor_trail_style
                .clone()
                .unwrap_or_else(|| format!("{DEFAULT_CURSOR_TRAIL_STYLE} (default)")),
            seed: cursor_trail_style,
        },
        EditField {
            // The aural half of the trail styles: each style's signature
            // palette plays softly on the same spawn edge that lights the
            // trail (droplets, crackle, chimes, the beam's hum...). Default ON;
            // silent whenever the trail's light is (off style, reduced motion,
            // unfocused). `trail_sound_volume` in aterm.toml trims the level.
            label: "Music effects",
            key: EDIT_TRAIL_SOUNDS,
            kind: EditKind::Bool,
            seed: Some(cfg.trail_sounds_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // THE MASTER VOLUME, built here rather than 600 rows down with the
            // leftovers: rows sort by (group order, BUILD order), so the owner's
            // "master volume slider plus the SFX toggles" only reads as a menu
            // if the slider follows the master switch it scales.
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
            // Tone melody: the trail sound's melody leans with the inferred
            // mood of the line being typed (a tiny on-device multilingual
            // classifier over TYPED input only — never screen content, never
            // leaves the machine). Default ON; deliberately subtle — same
            // instruments, same volume, and `technical`/uncertain lines
            // sound exactly like the melody with this off.
            label: "Tone-adaptive melody",
            key: EDIT_TONE_MELODY,
            kind: EditKind::Bool,
            seed: Some(cfg.tone_melody_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // The ambient BED: the continuous per-style background texture
            // that swells behind fast typing. Default OFF (the owner dislikes
            // the drone) — the toggle gates the synth's bed mixer entirely
            // (zero bed samples, not a muted gain), leaving the discrete
            // notes, the brrrring, the bonk and the melody untouched.
            label: "Ambient sound bed",
            key: EDIT_TRAIL_SOUND_BED,
            kind: EditKind::Bool,
            seed: Some(cfg.trail_sound_bed_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // WHICH instrument the keystrokes speak with: `auto` follows the
            // visual trail style's signature palette (today's sound); every
            // other option is a voice of its own — the nine palettes by
            // sound (`glass bell`, `droplet`, …), the `mechanical` keyboard,
            // `typewriter` / `marimba` / `felt` — spoken whatever the trail
            // looks like. Volume/on-off/bed gates apply unchanged; picking a
            // voice auditions one keystroke of it (`App::audition_typing_
            // sound`).
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
            // THE SING-ALONG RIFF's own switch (owner ask). It is TIER 5 — the
            // loudest thing the engine emits — and until now the only ways to
            // quiet it were the master switch (kills every sound) or the volume
            // (turns your keystrokes down with it). Default ON: a shipped
            // feature, so this is the opt-OUT. Sound only; the celebration's
            // ribbon, star shower and dancing cat keep running.
            label: "Sing-along riff",
            key: EDIT_TRAIL_SOUND_RIFF,
            kind: EditKind::Bool,
            seed: Some(cfg.trail_sound_riff_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // THE AUDIBLE BEL (owner ask): the one sound in the product with no
            // key at all. It is an OS alert sound (NSBeep / MessageBeep), NOT a
            // synth voice, so `trail_sound_volume` never reached it. Default ON
            // — turning it off silences only the beep; the visual flash and the
            // urgent-window request still surface background activity.
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
                None => "0.7 (default)".to_string(),
            },
        },
        EditField {
            label: "Typing wake",
            key: EDIT_CURSOR_TRAIL_WAKE_MS,
            kind: EditKind::Integer,
            seed: cfg.cursor_trail_wake_ms.map(|n| n.to_string()),
            placeholder: match cfg.cursor_trail_wake_ms {
                Some(0) => "off".to_string(),
                Some(n) => format!("{n} ms of travel"),
                None => "300 ms (default)".to_string(),
            },
        },
        EditField {
            label: "Light-crown radius",
            key: EDIT_CURSOR_TRAIL_RADIUS,
            kind: EditKind::Float,
            seed: cfg.cursor_trail_radius.map(|v| v.to_string()),
            placeholder: match cfg.cursor_trail_radius {
                Some(v) => format!("{v} cells"),
                None => "0.6 cells (default)".to_string(),
            },
        },
        EditField {
            label: "Landing ring",
            key: EDIT_CURSOR_TRAIL_RING,
            kind: EditKind::Bool,
            seed: Some(cfg.cursor_trail_ring_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Trail color",
            key: EDIT_CURSOR_TRAIL_COLOR,
            kind: EditKind::Color,
            placeholder: cursor_trail_color
                .clone()
                .unwrap_or_else(|| "cursor color (default)".to_string()),
            seed: cursor_trail_color,
        },
        EditField {
            label: "Trail accent",
            key: EDIT_CURSOR_TRAIL_ACCENT,
            kind: EditKind::Color,
            placeholder: cursor_trail_accent
                .clone()
                .unwrap_or_else(|| "brightened cursor color (default)".to_string()),
            seed: cursor_trail_accent,
        },
        EditField {
            label: "Rainbow kitty sprite",
            key: EDIT_CURSOR_NYAN_SPRITE,
            kind: EditKind::Text,
            placeholder: cursor_nyan_sprite
                .clone()
                .unwrap_or_else(|| "built-in sprite (default)".to_string()),
            seed: cursor_nyan_sprite,
        },
        EditField {
            label: "GPU bloom",
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
            label: "Bloom radius",
            key: EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
            kind: EditKind::Float,
            seed: cfg.cursor_trail_bloom_radius.map(|v| v.to_string()),
            placeholder: match cfg.cursor_trail_bloom_radius {
                Some(v) => v.to_string(),
                None => "2.2 (default)".to_string(),
            },
        },
        EditField {
            label: "Fire heat shimmer",
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
            label: "Serious mode (no sounds or effects)",
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
                .unwrap_or_else(|| motion_auto_placeholder().to_string()),
            seed: configured_str(cfg.motion.as_deref()),
        },
        EditField {
            // Load-adaptive shedding drops decorative effects under sustained RENDER
            // overload. Default ON; the checkbox reflects the resolved state directly.
            // Turn OFF (or set Motion = full) to keep the cursor trail / aurora on
            // regardless of load.
            label: "Load-adaptive motion",
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
            // The checkbox always reflects the RESOLVED state (default on), so it
            // starts in the right position; Save writes the explicit bool. The checkbox
            // shows its state directly, so the placeholder is unused (empty).
            seed: Some(cfg.copy_on_select_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Foreground",
            key: EDIT_FOREGROUND,
            kind: EditKind::Color,
            placeholder: foreground
                .clone()
                .unwrap_or_else(|| "theme default".to_string()),
            seed: foreground,
        },
        EditField {
            label: "Background",
            key: EDIT_BACKGROUND,
            kind: EditKind::Color,
            placeholder: background
                .clone()
                .unwrap_or_else(|| "theme default".to_string()),
            seed: background,
        },
        EditField {
            label: "Cursor color",
            key: EDIT_CURSOR_COLOR,
            kind: EditKind::Color,
            placeholder: cursor_color
                .clone()
                .unwrap_or_else(|| "theme default".to_string()),
            seed: cursor_color,
        },
        EditField {
            label: "Selection color",
            key: EDIT_SELECTION_COLOR,
            kind: EditKind::Color,
            placeholder: selection_color
                .clone()
                .unwrap_or_else(|| "theme default".to_string()),
            seed: selection_color,
        },
        EditField {
            label: "Selection foreground",
            key: EDIT_SELECTION_FOREGROUND,
            kind: EditKind::Color,
            placeholder: selection_foreground
                .clone()
                .unwrap_or_else(|| "auto (contrast floor)".to_string()),
            seed: selection_foreground,
        },
        EditField {
            label: "System appearance",
            key: EDIT_WINDOW_THEME,
            kind: EditKind::Enum {
                options: WINDOW_THEMES,
            },
            placeholder: configured_str(cfg.window_theme.as_deref())
                .unwrap_or_else(|| "auto (default)".to_string()),
            seed: configured_str(cfg.window_theme.as_deref()),
        },
        EditField {
            label: "Selected tab color",
            key: EDIT_ACTIVE_TAB_COLOR,
            kind: EditKind::Color,
            placeholder: active_tab_color
                .clone()
                .unwrap_or_else(|| "transparent white (default)".to_string()),
            seed: active_tab_color,
        },
        EditField {
            label: "Show floating version pill",
            key: EDIT_SHOW_BUILD_BADGE,
            kind: EditKind::Bool,
            // The version already lives in the menu bar (the v<version> menu → About);
            // this is the OPTIONAL extra floating top-right pill. Checkbox reflects the
            // RESOLVED state (default OFF); Save writes the bool.
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
            placeholder: cfg
                .tab_strip_rows
                .map_or_else(|| "default".to_string(), |n| n.to_string()),
        },
        EditField {
            label: "Generate live Activity",
            key: EDIT_DESCRIPTIVE_TITLES,
            kind: EditKind::Bool,
            seed: Some(cfg.descriptive_titles_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Activity provider",
            key: EDIT_TITLE_SUMMARY_PROVIDER,
            kind: EditKind::Enum {
                options: TITLE_SUMMARY_PROVIDERS,
            },
            placeholder: title_summary_provider
                .clone()
                .unwrap_or_else(|| "builtin (default)".to_string()),
            seed: title_summary_provider,
        },
        EditField {
            label: "Activity model",
            key: EDIT_TITLE_SUMMARY_MODEL,
            kind: EditKind::Text,
            placeholder: title_summary_model
                .clone()
                .unwrap_or_else(|| format!("{DEFAULT_TITLE_SUMMARY_MODEL} (default)")),
            seed: title_summary_model,
        },
        EditField {
            label: "Provider endpoint",
            key: EDIT_TITLE_SUMMARY_ENDPOINT,
            kind: EditKind::Text,
            placeholder: title_summary_endpoint.clone().unwrap_or_else(|| {
                match cfg.title_summary_provider_or_default() {
                    crate::app_config::TitleSummaryProvider::Ollama => format!(
                        "blank = automatic private endpoint; {EXAMPLE_EXPLICIT_OLLAMA_TITLE_SUMMARY_ENDPOINT} = explicit shared endpoint"
                    ),
                    crate::app_config::TitleSummaryProvider::OpenAiCompatible => {
                        "required: https://provider.example/v1/chat/completions".to_string()
                    }
                    crate::app_config::TitleSummaryProvider::Builtin
                    | crate::app_config::TitleSummaryProvider::Off => {
                        "not used by selected provider".to_string()
                    }
                }
            }),
            seed: title_summary_endpoint,
        },
        EditField {
            // This is intentionally a path-only text field: the config/runtime
            // interprets its contents as a filesystem path, never as a bearer token.
            label: "Provider token file (path only)",
            key: EDIT_TITLE_SUMMARY_TOKEN_FILE,
            kind: EditKind::Text,
            placeholder: title_summary_token_file
                .clone()
                .unwrap_or_else(|| "not configured (file path only)".to_string()),
            seed: title_summary_token_file,
        },
        EditField {
            label: "Request timeout (seconds)",
            key: EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS,
            kind: EditKind::Integer,
            seed: cfg
                .title_summary_timeout_seconds
                .map(|seconds| seconds.to_string()),
            placeholder: cfg.title_summary_timeout_seconds.map_or_else(
                || format!("{DEFAULT_TITLE_SUMMARY_TIMEOUT_SECONDS} (default)"),
                |seconds| seconds.to_string(),
            ),
        },
        EditField {
            label: "Remote proxy policy",
            key: EDIT_TITLE_SUMMARY_PROXY_MODE,
            kind: EditKind::Enum {
                options: TITLE_SUMMARY_PROXY_MODES,
            },
            placeholder: title_summary_proxy_mode
                .clone()
                .unwrap_or_else(|| "environment (default)".to_string()),
            seed: title_summary_proxy_mode,
        },
        EditField {
            // Like the token row, this is path-only. When present, the explicit
            // PEM bundle replaces platform roots for this provider.
            label: "Provider CA bundle (PEM path)",
            key: EDIT_TITLE_SUMMARY_CA_FILE,
            kind: EditKind::Text,
            placeholder: title_summary_ca_file
                .clone()
                .unwrap_or_else(|| "platform trust roots (default)".to_string()),
            seed: title_summary_ca_file,
        },
        EditField {
            label: "Activity refresh interval (seconds)",
            key: EDIT_TITLE_SUMMARY_INTERVAL_SECONDS,
            kind: EditKind::Integer,
            seed: cfg
                .title_summary_interval_seconds
                .map(|seconds| seconds.to_string()),
            placeholder: cfg.title_summary_interval_seconds.map_or_else(
                || format!("{DEFAULT_TITLE_SUMMARY_INTERVAL_SECONDS} (default)"),
                |seconds| seconds.to_string(),
            ),
        },
        EditField {
            label: "Activity context lines",
            key: EDIT_TITLE_SUMMARY_CONTEXT_LINES,
            kind: EditKind::Integer,
            seed: cfg
                .title_summary_context_lines
                .map(|lines| lines.to_string()),
            placeholder: cfg.title_summary_context_lines.map_or_else(
                || format!("{DEFAULT_TITLE_SUMMARY_CONTEXT_LINES} (default)"),
                |lines| lines.to_string(),
            ),
        },
        EditField {
            label: "Include recent terminal output",
            key: EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT,
            kind: EditKind::Bool,
            seed: Some(cfg.title_summary_include_output_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Trust network or unowned provider",
            key: EDIT_TITLE_SUMMARY_ALLOW_REMOTE,
            kind: EditKind::Bool,
            seed: Some(cfg.title_summary_allow_remote_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Tab label format",
            key: EDIT_TAB_TITLE_FORMAT,
            kind: EditKind::Enum {
                options: TITLE_FORMATS,
            },
            placeholder: tab_title_format
                .clone()
                .unwrap_or_else(|| "title-description (default)".to_string()),
            seed: tab_title_format,
        },
        EditField {
            label: "Window title format",
            key: EDIT_WINDOW_TITLE_FORMAT,
            kind: EditKind::Enum {
                options: TITLE_FORMATS,
            },
            placeholder: window_title_format
                .clone()
                .unwrap_or_else(|| "title-description (default)".to_string()),
            seed: window_title_format,
        },
        EditField {
            label: "Classify session status",
            key: EDIT_TAB_STATUS,
            kind: EditKind::Bool,
            seed: Some(cfg.tab_status_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Quiet after (ms)",
            key: EDIT_TAB_STATUS_QUIET_AFTER_MS,
            kind: EditKind::Integer,
            seed: cfg.tab_status_quiet_after_ms.map(|ms| ms.to_string()),
            placeholder: cfg.tab_status_quiet_after_ms.map_or_else(
                || format!("{DEFAULT_TAB_STATUS_QUIET_AFTER_MS} (default)"),
                |ms| ms.to_string(),
            ),
        },
        EditField {
            label: "Status dwell (ms)",
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
            label: "Focus priority boost (Windows)",
            key: EDIT_FOCUS_BOOST,
            kind: EditKind::Bool,
            seed: Some(cfg.focus_boost.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            // `[packages] enabled`: the visible master for the background
            // updater thread. Explicit Check/Install actions remain available.
            label: "Automatic package maintenance",
            key: EDIT_PACKAGES_ENABLED,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_enabled().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // `[packages] auto_update` (dotted key): keep installed ALab tools
            // current on the background cadence. Seeded RESOLVED (default ON —
            // today's behavior); the switch renders on the special Packages
            // page and through Search/Modified, never on an ordinary page.
            label: "Auto-update ALab tools",
            key: EDIT_PACKAGES_AUTO_UPDATE,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_auto_update().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // `[packages] auto_install` (dotted key): ALSO install missing
            // default-set members. Default OFF — flipping this switch is the
            // explicit multi-GB consent (§11); the co-located atpkg reads the
            // same bit from the same table.
            label: "Auto-install ALab toolset",
            key: EDIT_PACKAGES_AUTO_INSTALL,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_auto_install().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // `[packages] seed_install` (dotted key): lay down the toolchain
            // sealed INSIDE the app on first launch. Default ON — those bytes
            // already shipped, so the cost is extraction rather than download.
            // It is here, and not only in aterm.toml, because the documented way
            // to decline used to require editing a config file that does not
            // exist until the app has already run once and installed everything.
            label: "Install bundled ALab toolset on first launch",
            key: EDIT_PACKAGES_SEED_INSTALL,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_seed_install().to_string()),
            placeholder: String::new(),
        },
    ];
    // ---- FULL-COVERAGE batch: the remaining scalar keys, the list rows, and the
    // nested-table leaves — so Manual and `settings set` cover EVERY user setting.
    // Same seeding law as above: configured raw value only
    // (unset = blank), resolved state for Bools, effective default in the
    // placeholder; list rows seed the comma-joined form their Save re-splits.
    let join_list = |v: Option<&Vec<String>>| v.filter(|l| !l.is_empty()).map(|l| l.join(", "));
    let palette = join_list(cfg.palette.as_ref());
    let trail_packs = join_list(cfg.cursor_trail_packs.as_ref());
    let shell_args = join_list(cfg.shell_args.as_ref());
    let font_features = join_list(cfg.font_features.as_ref());
    let shell = configured_str(cfg.shell.as_deref());
    let window_colorspace = configured_str(cfg.window_colorspace.as_deref());
    let background_material = configured_str(cfg.background_material.as_deref());
    fields.extend([
        EditField {
            label: "GPU rendering (restart)",
            key: EDIT_GPU,
            kind: EditKind::Bool,
            // Resolved CONFIG state (default ON with CPU fallback); the env/CLI
            // overrides are process-fixed and deliberately not folded in here.
            seed: Some(cfg.gpu.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "ANSI palette (comma-separated)",
            key: EDIT_PALETTE,
            kind: EditKind::Text,
            placeholder: palette
                .clone()
                .unwrap_or_else(|| "theme palette — #RRGGBB by index, comma-separated".to_string()),
            seed: palette,
        },
        // (The Sound-menu rows — master, volume, palette, melody, bed, riff and
        // the terminal bell — are built together above, so the box paints in
        // the order the owner asked to read it.)
        // (The trail colour/accent, kitty sprite, comet-geometry, bloom, shimmer
        // and HDR/SDR glow ROWS live in the core list above — one row per key.)
        EditField {
            label: "Trail Pack manifests (comma-separated)",
            key: EDIT_CURSOR_TRAIL_PACKS,
            kind: EditKind::Text,
            placeholder: trail_packs
                .clone()
                .unwrap_or_else(|| "none — *.toml paths, comma-separated".to_string()),
            seed: trail_packs,
        },
        EditField {
            label: "Fade in new output",
            key: EDIT_STREAM_FADE,
            kind: EditKind::Bool,
            seed: Some(cfg.stream_fade_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Stream fade ms",
            key: EDIT_STREAM_FADE_MS,
            kind: EditKind::Integer,
            seed: cfg.stream_fade_ms.map(|n| n.to_string()),
            placeholder: cfg
                .stream_fade_ms
                .map_or_else(|| "90 (default)".to_string(), |n| n.to_string()),
        },
        EditField {
            label: "Shell",
            key: EDIT_SHELL,
            kind: EditKind::Text,
            placeholder: shell
                .clone()
                .unwrap_or_else(|| "platform default".to_string()),
            seed: shell,
        },
        EditField {
            label: "Shell arguments (comma-separated)",
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
            label: "Font features (comma-separated)",
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
                .unwrap_or_else(|| "none (flat background)".to_string()),
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
            // Robi the helper robot: the chrome-walking, tip-sharing
            // RESIDENT (walks the typed row, jumping jacks, ladder, tab-bar
            // monkey bars — forever). Default OFF (opt-in by owner
            // directive); reduced motion and serious mode hide an invited
            // Robi without touching this preference.
            label: "Robi the helper robot",
            key: EDIT_ROBI,
            kind: EditKind::Bool,
            seed: Some(cfg.robi_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // Rainbow sparkles on the post-update celebration card. Default
            // ON (user-facing features ship enabled; this is an opt-OUT);
            // decorative only — the wording, timing, and purpose of the
            // notice never change, and reduced motion holds the colours
            // still. Everything else about this key was already wired (the
            // EditKind::Bool arm, the VISUAL_PREVIEW_EXEMPT_KEYS rationale,
            // the renderer read) — this row is what makes it real: without
            // it the setting existed everywhere except where a user could
            // see it, and both registry-conformance tests failed.
            label: "Update-celebration sparkles",
            key: EDIT_NOTICE_SPARKLE,
            kind: EditKind::Bool,
            seed: Some(cfg.notice_sparkle_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // The toolchain-install progress card's party trim (rainbow bar,
            // completion sparkles, the cat). Default ON (opt-OUT); decorative
            // only — the card's information and behaviour are identical with
            // it off, and reduced motion / serious mode strip the same trim
            // without touching this preference.
            label: "Install-progress rainbow & cat",
            key: EDIT_PKG_PROGRESS_EFFECTS,
            kind: EditKind::Bool,
            seed: Some(cfg.pkg_progress_effects_or_default().to_string()),
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
            label: "Temporal recording",
            key: EDIT_TEMPORAL_RECORDING,
            kind: EditKind::Bool,
            seed: Some(cfg.temporal_recording.unwrap_or(false).to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Window padding (px)",
            key: EDIT_WINDOW_PADDING,
            kind: EditKind::Float,
            seed: cfg.window_padding.map(|v| format!("{v}")),
            placeholder: match cfg.window_padding {
                Some(v) => format!("{v} px"),
                None => "12 (default)".to_string(),
            },
        },
        EditField {
            label: "Window top padding (px)",
            key: EDIT_WINDOW_PADDING_TOP,
            kind: EditKind::Float,
            seed: cfg.window_padding_top.map(|v| format!("{v}")),
            placeholder: match cfg.window_padding_top {
                Some(v) => format!("{v} px"),
                None => "2 (default)".to_string(),
            },
        },
    ]);
    // Nested dotted-key rows — one per registered scalar leaf, driven off the ONE
    // `NESTED_LEAVES` registry so a leaf can never exist for the writer/`edit_kind`
    // yet miss its settings row (the conformance tests pin seed/placeholder too).
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
    // Security opt-ins (all fail-closed: default OFF). One Bool per key, sourced from
    // SECURITY_BOOL_KEYS so `edit_kind` agrees. `allow_value` reads the matching field.
    for &key in SECURITY_BOOL_KEYS {
        let on = match key {
            EDIT_ALLOW_OSC52_QUERY => cfg.allow_osc52_query,
            EDIT_ALLOW_WINDOW_OPS => cfg.allow_window_ops,
            EDIT_ALLOW_NOTIFICATIONS => cfg.allow_notifications,
            EDIT_ALLOW_PALETTE_RECONFIGURE => cfg.allow_palette_reconfigure,
            EDIT_ALLOW_KITTY_FILE_TRANSFER => cfg.allow_kitty_file_transfer,
            EDIT_SECURE_KEYBOARD_ENTRY => cfg.secure_keyboard_entry,
            _ => None,
        }
        .unwrap_or(false);
        fields.push(EditField {
            label: security_label(key),
            key,
            kind: EditKind::Bool,
            seed: Some(on.to_string()),
            placeholder: String::new(),
        });
    }
    // Group the rows by section LAST (after every push), stably so within-section build
    // order is preserved. This is the single ordering the painter, scroll, and hit-test
    // all see; the overlay renders a header before each section's controls.
    //
    // `sort_by_cached_key` (not `sort_by_key`) because `section_of` is a linear scan over
    // ~88 key constants plus three slice `contains` passes, and `sort_by_key` re-runs the
    // key closure twice per comparison (~2,500 evaluations for ~170 rows). The cached form
    // evaluates it once per row and is documented stable, so the within-section ordering
    // above is byte-identical.
    fields.sort_by_cached_key(|f| section_of(f.key).order_index());
    fields
}

/// The result of [`save_prefs_edits`], so the window can show visible feedback (a status
/// line) rather than silently succeeding/failing. `Saved` means the file actually changed
/// and a reload should follow; `Unchanged` is a true all-no-op Save; `Conflict`
/// preserves the expected and observed disk generations for a retry UI;
/// `PublishedUnverified` requires reconciliation before retry; and `Error`
/// carries a short human message for a pre-publication failure.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
        message: String,
    },
    /// The save failed before publication. Carries a short message for the UI.
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

/// Persist a batch of Preferences edits to `aterm.toml` NON-DESTRUCTIVELY, returning a
/// [`SaveOutcome`] so the caller can both decide whether to reload AND show the user
/// what happened.
///
/// Best-effort + never panics: a missing file is treated as empty (the keys are
/// created); validation/I/O failures return [`SaveOutcome::Error`], an OCC loss
/// returns [`SaveOutcome::Conflict`], and a post-publication proof failure returns
/// [`SaveOutcome::PublishedUnverified`]. [`SaveOutcome::Saved`] is returned only
/// when the complete durable proof was produced.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn save_prefs_edits(edits: &[(&str, Option<String>)]) -> SaveOutcome {
    let Some(path) = crate::app_config::config_path() else {
        let msg = "no config path (HOME/XDG unset)".to_string();
        eprintln!("aterm-gui: prefs save: {msg}; skipping");
        return SaveOutcome::Error(msg);
    };
    // A missing file is fine — start from empty and create it on write. The
    // returned baseline is the same fingerprint/target binding Manual uses.
    let contents = match crate::native_document_host::read_config_atomic_file(
        &path,
        crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
        true,
    ) {
        Ok(contents) => contents,
        Err(error) => {
            let msg = format!("{} unreadable ({error})", path.display());
            eprintln!("aterm-gui: prefs save: {msg}; leaving config unchanged");
            return SaveOutcome::Error(msg);
        }
    };
    let existing = match std::str::from_utf8(&contents.bytes) {
        Ok(text) => text.to_string(),
        Err(error) => {
            let msg = format!("{} is not UTF-8 ({error})", path.display());
            eprintln!("aterm-gui: prefs save: {msg}; leaving config unchanged");
            return SaveOutcome::Error(msg);
        }
    };
    let updated = match apply_prefs_edits(&existing, edits) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("aterm-gui: prefs save: {e}; leaving config unchanged");
            return SaveOutcome::Error(e.to_string());
        }
    };
    if updated == existing {
        return SaveOutcome::Unchanged; // nothing changed — skip the write + reload
    }
    commit_prefs_bytes(&path, &contents.baseline, updated.as_bytes())
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
                outcome: SaveOutcome::Error("no config path (HOME/XDG unset)".to_string()),
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
                return ConfigSnapshotSaveResult {
                    outcome: SaveOutcome::Error(format!("{} unreadable ({error})", path.display())),
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
            eprintln!("aterm-gui: prefs save: {message}; config unchanged");
            ConfigSnapshotSaveResult {
                outcome: SaveOutcome::Conflict {
                    expected: baseline.observed.content,
                    observed,
                    message,
                },
                observed: None,
            }
        }
        crate::native_document_host::AtomicCommitResult::Failed { stage, message } => {
            let message = format!("{} save failed at {stage:?} ({message})", path.display());
            eprintln!("aterm-gui: prefs save: {message}; config unchanged");
            ConfigSnapshotSaveResult {
                outcome: SaveOutcome::Error(message),
                observed: None,
            }
        }
        crate::native_document_host::AtomicCommitResult::PublishedUnverified {
            stage,
            observed,
            message,
        } => {
            let message = format!(
                "{} may already contain the requested bytes; reload and reconcile before retrying \
                 ({stage:?}: {message})",
                path.display()
            );
            eprintln!("aterm-gui: prefs save: {message}");
            ConfigSnapshotSaveResult {
                outcome: SaveOutcome::PublishedUnverified {
                    stage,
                    observed,
                    message,
                },
                observed: None,
            }
        }
    }
}

fn commit_prefs_bytes(
    path: &std::path::Path,
    baseline: &crate::native_document_host::AtomicFileBaseline,
    updated: &[u8],
) -> SaveOutcome {
    match crate::native_document_host::commit_atomic_bytes(baseline, updated) {
        crate::native_document_host::AtomicCommitResult::Committed(_) => SaveOutcome::Saved,
        crate::native_document_host::AtomicCommitResult::Conflict { observed, message } => {
            let message = format!(
                "{} changed while saving ({message}); review the latest file and retry",
                path.display()
            );
            eprintln!("aterm-gui: prefs save: {message}; config unchanged");
            SaveOutcome::Conflict {
                expected: baseline.observed.content,
                observed,
                message,
            }
        }
        crate::native_document_host::AtomicCommitResult::Failed { stage, message } => {
            let message = format!("{} save failed at {stage:?} ({message})", path.display());
            eprintln!("aterm-gui: prefs save: {message}; config unchanged");
            SaveOutcome::Error(message)
        }
        crate::native_document_host::AtomicCommitResult::PublishedUnverified {
            stage,
            observed,
            message,
        } => {
            let message = format!(
                "{} may already contain the requested bytes; reload and reconcile before retrying \
                 ({stage:?}: {message})",
                path.display()
            );
            eprintln!("aterm-gui: prefs save: {message}");
            SaveOutcome::PublishedUnverified {
                stage,
                observed,
                message,
            }
        }
    }
}

/// Tests for the PURE non-destructive editor: each edit goes through exactly the
/// [`apply_prefs_edits`] the Settings overlay's save path calls, so the file-rewrite
/// semantics are proven here without a GUI.
#[cfg(test)]
mod trail_style_tests {
    use super::{CURSOR_TRAIL_STYLES, Config, apply_prefs_edits};
    use crate::cursor_glow::GlowStyle;

    /// `Some(v)` helper to keep the edit lists terse.
    fn set(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    /// C5: the documented "single source" claim — every `CURSOR_TRAIL_STYLES`
    /// entry (the picker/cycler domain) is classified by `GlowStyle::parse`, and the
    /// additive looks map to distinct variants (`comet` graduated from the
    /// Lumen default when it grew its icy tail + debris + nucleus cursor — it
    /// STILL additionally routes the cadence-comet body by raw string — and
    /// `beam` graduated as the steady power-down tube). The one specially-
    /// routed entry left (`off` = disabled) is handled BEFORE `GlowStyle` in
    /// `App::glow_config` / `trail_config`, so `parse` folds it to the `Lumen`
    /// default. This pins the two lists together so adding a style to one
    /// without the other fails CI.
    /// (Lives here, beside the list, since the aurora engine moved to aterm-effects.)
    #[test]
    fn cursor_trail_styles_map_to_expected_glow_style() {
        let expected = |s: &str| match s {
            "lumen" => GlowStyle::Lumen,
            "phaser" => GlowStyle::Phaser,
            "rainbow kitty" => GlowStyle::RainbowKitty,
            // The pet is a COMPANION swap, not a trail: it deliberately shares
            // `RainbowKitty` so the ribbon, starfield and sound palette are
            // literally the same code. `GlowStyle::style_names_kitty_pet` is
            // what separates them, and its own pin is below.
            "rainbow kitty pet" => GlowStyle::RainbowKitty,
            // …and the dog pet, for the same reason: a species of the pet, not
            // a trail of its own.
            "rainbow dog pet" => GlowStyle::RainbowKitty,
            "sparkle" => GlowStyle::Sparkle,
            "fire" => GlowStyle::Fire,
            "laser" => GlowStyle::Laser,
            "water" => GlowStyle::Water,
            "beam" => GlowStyle::Beam,
            "comet" => GlowStyle::Comet,
            "off" => GlowStyle::Lumen, // routed separately; parse → default
            other => {
                panic!("CURSOR_TRAIL_STYLES entry {other:?} is unclassified by GlowStyle::parse")
            }
        };
        for &s in CURSOR_TRAIL_STYLES {
            assert_eq!(GlowStyle::parse(s), expected(s), "style {s:?}");
        }
    }

    /// The pet's twin pin: exactly one canonical style (and its documented
    /// aliases) selects the full-body CAT companion, and every other spelling —
    /// including the plain `rainbow kitty` it shares a `GlowStyle` with, and
    /// the dog pet it shares the pet machinery with — does not. Without this,
    /// the three rainbow-kitty entries would be indistinguishable to the picker
    /// and the draw path could never disagree with the trail.
    #[test]
    fn exactly_one_trail_style_selects_the_full_body_pet() {
        for &s in CURSOR_TRAIL_STYLES {
            assert_eq!(
                GlowStyle::style_names_kitty_pet(s),
                s == "rainbow kitty pet",
                "style {s:?}"
            );
        }
        for alias in ["kitty pet", "pet kitty", "  Kitty Pet  "] {
            assert!(
                GlowStyle::style_names_kitty_pet(alias),
                "documented alias {alias:?} must select the pet"
            );
        }
        for other in ["rainbow", "nyan", "pet", "kitty", "rainbowkittypet"] {
            assert!(
                !GlowStyle::style_names_kitty_pet(other),
                "{other:?} must NOT select the pet"
            );
        }
        // The cat predicate must not claim the dog's spellings — they share the
        // pet machinery, so a leak here would draw a cat for `rainbow dog pet`.
        for dog in ["rainbow dog pet", "dog pet", "pet dog", "rainbow puppy pet"] {
            assert!(
                !GlowStyle::style_names_kitty_pet(dog),
                "{dog:?} is the DOG pet, not the kitty pet"
            );
        }
    }

    /// The DOG pet's pin, the exact mirror of the kitty pet's above: one
    /// canonical style plus its documented aliases selects the dog skin, the
    /// cat pet does not, and `style_names_any_pet` is the union — which is the
    /// predicate the draw path asks, so it must cover both and nothing else.
    #[test]
    fn exactly_one_trail_style_selects_the_dog_pet() {
        for &s in CURSOR_TRAIL_STYLES {
            assert_eq!(
                GlowStyle::style_names_dog_pet(s),
                s == "rainbow dog pet",
                "style {s:?}"
            );
            assert_eq!(
                GlowStyle::style_names_any_pet(s),
                s == "rainbow dog pet" || s == "rainbow kitty pet",
                "any-pet union, style {s:?}"
            );
        }
        for alias in ["dog pet", "pet dog", "rainbow puppy pet", "  Dog Pet  "] {
            assert!(
                GlowStyle::style_names_dog_pet(alias),
                "documented alias {alias:?} must select the dog pet"
            );
        }
        for other in ["rainbow", "dog", "puppy", "rainbowdogpet", "rainbow kitty pet"] {
            assert!(
                !GlowStyle::style_names_dog_pet(other),
                "{other:?} must NOT select the dog pet"
            );
        }
    }

    /// THE SHIPPED DEFAULT IS THE WALKING CAT (owner ruling, 2026-08-10: "what
    /// we have on this machine should be the default", where that machine's
    /// `aterm.toml` reads `cursor_trail_style = "rainbow kitty pet"`).
    ///
    /// This proves the DESTINATION, not the constant: an unset config resolves
    /// through `Config::cursor_trail_style_raw` into a spelling that (a) is a
    /// real picker option, (b) satisfies the engine's own pet predicate — the
    /// single seam `app_render::trail_is_kitty_pet` asks before drawing the
    /// full-body companion instead of the flying head — and (c) still lands on
    /// the rainbow ribbon. Pinning only `DEFAULT_CURSOR_TRAIL_STYLE == "…"`
    /// would pass for a typo the engine silently disables.
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
        // The flying head is not gone — it is one explicit line of config away,
        // and every historical spelling still selects it.
        assert!(!GlowStyle::style_names_kitty_pet("rainbow kitty"));
        for legacy in ["nyan", "nyan rainbow", "rainbow"] {
            assert!(
                !GlowStyle::style_names_kitty_pet(
                    super::cursor_trail_style_canonical(legacy).expect("documented alias")
                ),
                "{legacy:?} must keep resolving to the flying head"
            );
        }
    }

    /// The alias table's twin pin: every documented alias maps to a canonical
    /// option that is (a) actually in the picker's domain and (b) parses to the
    /// SAME engine style as the alias itself — so the Settings panel, the
    /// validator, and `GlowStyle::parse` can never disagree about what an
    /// aliased spelling means (the "rainbow shows phaser but renders rainbow kitty" bug).
    #[test]
    fn cursor_trail_style_aliases_agree_with_engine_parse() {
        for &(alias, canonical) in super::CURSOR_TRAIL_STYLE_ALIASES {
            assert!(
                CURSOR_TRAIL_STYLES.contains(&canonical),
                "alias {alias:?} maps outside the picker domain: {canonical:?}"
            );
            assert_eq!(
                GlowStyle::parse(alias),
                GlowStyle::parse(canonical),
                "alias {alias:?} and its canonical {canonical:?} diverge in the engine"
            );
            assert_eq!(
                super::cursor_trail_style_canonical(alias),
                Some(canonical),
                "resolver must accept the documented alias {alias:?}"
            );
        }
        // Canonical spellings resolve to themselves (case-insensitively)…
        for &s in CURSOR_TRAIL_STYLES {
            assert_eq!(super::cursor_trail_style_canonical(s), Some(s));
            assert_eq!(
                super::cursor_trail_style_canonical(&s.to_ascii_uppercase()),
                Some(s)
            );
        }
        // …and a genuinely unknown spelling is refused (the validator's arm).
        assert_eq!(super::cursor_trail_style_canonical("plasma"), None);
        assert_eq!(super::cursor_trail_style_canonical(""), None);
    }

    /// THE TYPING-SOUND PICKER IS THE SYNTH'S ROSTER: `TRAIL_SOUND_STYLES` is
    /// exactly `SoundVoice::ALL` by name, in order (a bijection — no voice
    /// missing from the picker, no picker row without a voice), `auto` leads
    /// and is the default, and every spelling is the lowercase-words form the
    /// `cursor_trail_style` convention uses.
    #[test]
    fn trail_sound_styles_are_the_synth_roster() {
        use aterm_effects::trail_sound::SoundVoice;
        let names: Vec<&str> = SoundVoice::ALL.iter().map(|v| v.name()).collect();
        assert_eq!(super::TRAIL_SOUND_STYLES, names.as_slice());
        assert_eq!(super::TRAIL_SOUND_STYLES[0], "auto");
        assert_eq!(super::DEFAULT_TRAIL_SOUND_STYLE, "auto");
        assert_eq!(super::TRAIL_SOUND_STYLES.len(), 14);
        for &o in super::TRAIL_SOUND_STYLES {
            assert!(o.chars().all(|c| c.is_ascii_lowercase() || c == ' '), "{o:?}");
        }
        assert!(super::TRAIL_SOUND_STYLES.contains(&"glass bell"));
        assert!(super::TRAIL_SOUND_STYLES.contains(&"typewriter"));
        assert!(super::TRAIL_SOUND_STYLES.contains(&"marimba"));
        assert!(super::TRAIL_SOUND_STYLES.contains(&"felt"));
        assert!(
            !super::TRAIL_SOUND_STYLES.contains(&"off"),
            "no second off: `trail_sounds = false` already is off"
        );
        match super::edit_kind(super::EDIT_TRAIL_SOUND_STYLE) {
            super::EditKind::Enum { options } => assert_eq!(options, super::TRAIL_SOUND_STYLES),
            other => panic!("trail_sound_style should be Enum, got {other:?}"),
        }
    }

    /// The typing-sound resolver: canonical spellings resolve to themselves
    /// (case- and whitespace-insensitively), every documented synth alias
    /// projects onto its canonical row, and an unknown spelling is refused —
    /// while the WRITER accepts only canonical spellings (aliases are load-
    /// only, exactly as for `cursor_trail_style`) and the loader
    /// (`Config::trail_sound_voice`) accepts both and falls back to `auto`.
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
            assert_eq!(super::trail_sound_style_canonical(alias), Some(voice.name()), "{alias}");
            // Aliases are accepted at LOAD…
            let cfg = Config {
                trail_sound_style: Some(alias.to_string()),
                ..Config::default()
            };
            assert_eq!(cfg.trail_sound_voice(), voice, "loader: {alias}");
            // …and REJECTED by the writer (never offered, never persisted).
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
            ("Glass Bell", SoundVoice::Of(GlowStyle::RainbowKitty)),
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
            assert!(out.contains(&format!("trail_sound_style = \"{o}\"")), "{out}");
            let up = apply_prefs_edits(
                "",
                &[(super::EDIT_TRAIL_SOUND_STYLE, set(&o.to_ascii_uppercase()))],
            )
            .unwrap();
            assert!(up.contains(&format!("trail_sound_style = \"{o}\"")), "{up}");
        }
    }

    /// The picker's dynamic option resolver lists the built-ins verbatim, then
    /// one sorted `pack:<id>` entry per loaded Trail Pack.
    #[test]
    fn cursor_trail_style_options_lists_builtins_then_packs() {
        // No packs → byte-identical to the static built-in list.
        let none = super::cursor_trail_style_options(std::iter::empty());
        assert_eq!(none.len(), CURSOR_TRAIL_STYLES.len());
        assert_eq!(none[0], CURSOR_TRAIL_STYLES[0]);

        let with = super::cursor_trail_style_options(["synthwave", "emberfall"].into_iter());
        assert_eq!(with.len(), CURSOR_TRAIL_STYLES.len() + 2);
        assert!(with.contains(&"pack:emberfall".to_string()));
        assert!(with.contains(&"pack:synthwave".to_string()));
        // Pack entries are sorted and follow all built-ins.
        let first_pack = with.iter().position(|o| o.starts_with("pack:")).unwrap();
        assert_eq!(&with[first_pack], "pack:emberfall");
        assert_eq!(&with[first_pack + 1], "pack:synthwave");
    }
}

#[cfg(test)]
mod edit_tests {
    use super::{
        Config, EDIT_ALLOW_WINDOW_OPS, EDIT_BACKGROUND, EDIT_COLUMNS, EDIT_COPY_ON_SELECT,
        EDIT_CURSOR_COLOR, EDIT_CURSOR_STYLE, EDIT_CURSOR_TRAIL, EDIT_CURSOR_TRAIL_STYLE,
        EDIT_FONT_FAMILY, EDIT_FONT_PX, EDIT_FOREGROUND, EDIT_LIGATURES, EDIT_LINES, EDIT_MOTION,
        EDIT_SCROLLBACK, EDIT_SEARCH_HISTORY_LINES, EDIT_SELECTION_COLOR, EDIT_THEME,
        EDIT_WINDOW_THEME, EditKind, PrefsEditError, SaveOutcome, apply_prefs_edits,
        editable_fields, keywords_of, range_of,
    };

    /// `Some(v)` helper to keep the edit lists terse.
    fn set(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[cfg(unix)]
    #[test]
    fn atomic_config_save_preserves_a_bound_final_symlink() {
        use std::os::unix::fs::symlink;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock follows the Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "aterm-prefs-symlink-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("create isolated test directory");
        let target = directory.join("managed.toml");
        let link = directory.join("aterm.toml");
        std::fs::write(&target, "font_px = 12\n").expect("seed managed config");
        symlink(&target, &link).expect("create config symlink");

        let baseline = crate::native_document_host::read_config_atomic_file(&link, 4096, false)
            .expect("config authority admits a bound final symlink")
            .baseline;
        assert!(matches!(
            super::commit_prefs_bytes(&link, &baseline, b"font_px = 14\n"),
            SaveOutcome::Saved
        ));

        assert!(
            std::fs::symlink_metadata(&link)
                .expect("symlink remains")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read updated target"),
            "font_px = 14\n"
        );
        std::fs::remove_dir_all(directory).expect("remove isolated test directory");
    }

    #[test]
    fn atomic_config_occ_preserves_structured_conflict_context() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "aterm-prefs-conflict-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("aterm.toml");
        std::fs::write(&path, "font_px = 12\n").unwrap();
        let baseline = crate::native_document_host::read_atomic_file(&path, 4096, false)
            .unwrap()
            .baseline;
        std::fs::write(&path, "font_px = 14\n").unwrap();

        assert!(matches!(
            super::commit_prefs_bytes(&path, &baseline, b"font_px = 16\n"),
            SaveOutcome::Conflict {
                expected,
                observed,
                message,
            } if expected == baseline.observed.content
                && observed.content
                    == crate::native_document_io::ContentFingerprint::of(b"font_px = 14\n")
                && message.contains("review the latest file")
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "font_px = 14\n");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn visual_preview_registry_is_complete_unique_and_counted() {
        use std::collections::BTreeSet;

        let registry = super::VISUAL_PREVIEW_KEYS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            registry.len(),
            super::VISUAL_PREVIEW_KEYS.len(),
            "duplicate preview key"
        );
        assert_eq!(registry.len(), 58, "the explicit visual contract changed");

        let fields = super::editable_fields(&Config::default());
        let expected = fields
            .iter()
            .filter(|field| {
                matches!(
                    super::section_of(field.key),
                    super::Section::Appearance
                        | super::Section::Typography
                        | super::Section::Cursor
                        // The Cursor Kitty pane is a VISUAL section like its
                        // Cursor & Motion sibling — it shares that page's live
                        // cursor scene — so its three keys owe the same
                        // projection-or-documented-exemption proof.
                        | super::Section::CursorKitty
                        | super::Section::Window
                )
            })
            // The preview registry is an explicitly VISUAL contract; fields with
            // no honest workbench projection are individually allowlisted (with
            // rationale) in `VISUAL_PREVIEW_EXEMPT_KEYS` — the decorative
            // overlay tables route by prefix there for the same reason.
            .filter(|field| {
                !super::VISUAL_PREVIEW_EXEMPT_KEYS.contains(&field.key)
                    && !field.key.starts_with("sparkle_words.")
                    && !field.key.starts_with("matrix_rain.")
            })
            .map(|field| field.key)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            registry, expected,
            "every visual field has exactly one preview projection or a \
             documented VISUAL_PREVIEW_EXEMPT_KEYS rationale"
        );
        // The exempt list stays honest: every entry is a REAL registry row in a
        // visual section (a stale exemption fails loudly), and no key is both
        // previewed and exempt.
        for key in super::VISUAL_PREVIEW_EXEMPT_KEYS {
            assert!(
                fields.iter().any(|field| &field.key == key),
                "stale exempt key {key}"
            );
            assert!(
                !registry.contains(key),
                "{key} is both previewed and exempt"
            );
        }

        for (section, count) in [
            (super::Section::Appearance, 11),
            (super::Section::Typography, 22),
            // 21 → 18 + 3: the trail-style picker, the rainbow wake dial and the
            // custom sprite moved to the cat's own pane. Same 58 total.
            (super::Section::Cursor, 18),
            (super::Section::CursorKitty, 3),
            (super::Section::Window, 4),
        ] {
            assert_eq!(
                registry
                    .iter()
                    .filter(|key| super::section_of(key) == section)
                    .count(),
                count,
                "{section:?} preview count",
            );
        }

        let range = super::range_of(super::EDIT_TAB_STRIP_ROWS).expect("tab strip range");
        assert_eq!(range.min, 0.0);
        assert_eq!(
            range.max,
            f64::from(crate::app_config::MAX_TAB_STRIP_ROWS),
            "control and shipping config clamp must agree",
        );
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

    /// Dotted write on an EMPTY file materialises an explicit `[matrix_rain]`
    /// table with a typed bool, and the result round-trips through serde into
    /// the real resolver.
    #[test]
    fn dotted_write_creates_the_table_and_round_trips() {
        let out = apply_prefs_edits("", &[(super::EDIT_MATRIX_RAIN_ENABLED, set("true"))])
            .expect("dotted write");
        assert!(
            out.contains("[matrix_rain]") && out.contains("enabled = true"),
            "explicit table + typed bool: {out:?}"
        );
        let c: Config = toml::from_str(&out).expect("round-trips through serde");
        assert!(c.matrix_rain_enabled());
    }

    /// Dotted edits are NON-DESTRUCTIVE inside the table: comments, unrelated
    /// sibling keys (in `[matrix_rain]` AND at top level) and formatting all
    /// survive an update; only the addressed child changes.
    #[test]
    fn dotted_update_preserves_comments_and_unrelated_keys() {
        let existing = "# my config\nfont_px = 13.0\n\n[matrix_rain]\n# keep the fps comment\nfps = 24 # inline note\nenabled = false\n";
        let out = apply_prefs_edits(existing, &[(super::EDIT_MATRIX_RAIN_ENABLED, set("true"))])
            .expect("dotted update");
        assert!(out.contains("# my config"), "top comment survives");
        assert!(out.contains("font_px = 13.0"), "unrelated top key survives");
        assert!(
            out.contains("# keep the fps comment"),
            "table comment survives"
        );
        assert!(
            out.contains("fps = 24 # inline note"),
            "sibling + its inline note survive"
        );
        assert!(out.contains("enabled = true"), "the child updated in place");
        let c: Config = toml::from_str(&out).unwrap();
        assert!(c.matrix_rain_enabled());
        assert_eq!(c.matrix_rain.as_ref().unwrap().fps, Some(24));
    }

    /// Blank = revert: removing the dotted child deletes only that key while
    /// siblings remain — and an EMPTIED table is deliberately RETAINED (its
    /// header may carry the user's comments, and an empty table parses to the
    /// same defaults as an absent one — the nested writer's contract).
    #[test]
    fn dotted_remove_reverts_and_retains_an_emptied_table() {
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
        assert!(
            out.contains("[matrix_rain]"),
            "an emptied table is retained (comments live on its header): {out:?}"
        );
        assert!(out.contains("font_px = 13.0"));
        let cfg: crate::app_config::Config = toml::from_str(&out).expect("round-trip");
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

    /// The inline-table spelling (`matrix_rain = { … }`) is table-like too:
    /// dotted edits address into it rather than clobbering it.
    #[test]
    fn dotted_write_addresses_into_an_inline_table() {
        let out = apply_prefs_edits(
            "matrix_rain = { enabled = false, fps = 24 }\n",
            &[(super::EDIT_MATRIX_RAIN_ENABLED, set("true"))],
        )
        .expect("inline-table update");
        let c: Config = toml::from_str(&out).unwrap();
        assert!(c.matrix_rain_enabled());
        assert_eq!(
            c.matrix_rain.as_ref().unwrap().fps,
            Some(24),
            "sibling kept"
        );
    }

    /// FAIL-CLOSED: a dotted write refuses when the top-level name is already
    /// a NON-TABLE value (user data the editor must not destroy); a dotted
    /// remove of a structurally-impossible child is a harmless no-op.
    #[test]
    fn dotted_write_refuses_a_non_table_collision() {
        let err = apply_prefs_edits(
            "matrix_rain = 5\n",
            &[(super::EDIT_MATRIX_RAIN_ENABLED, set("true"))],
        )
        .expect_err("refuses to clobber a scalar");
        assert!(
            err.to_string().contains("not a table"),
            "names the conflict: {err}"
        );
        let out = apply_prefs_edits(
            "matrix_rain = 5\n",
            &[(super::EDIT_MATRIX_RAIN_ENABLED, None)],
        )
        .expect("removal is a no-op against a scalar");
        assert_eq!(out, "matrix_rain = 5\n", "the scalar is untouched");
    }

    /// The Settings row itself: registered exactly ONCE (the NESTED_LEAVES
    /// entry — Appearance ▸ Matrix rain), grouped, searchable, and seeded with
    /// the RESOLVED bool so the switch starts in the right position.
    #[test]
    fn matrix_rain_row_surfaces_in_the_settings_model() {
        let key = super::EDIT_MATRIX_RAIN_ENABLED;
        assert_eq!(super::section_of(key), super::Section::Appearance);
        assert_eq!(super::group_of(key), ("Matrix rain", 5));
        assert!(super::group_footnote("Matrix rain").is_some());
        assert!(!keywords_of(key).is_empty());
        assert!(
            super::nested_leaf(key).is_some(),
            "the row is registered ONCE, via NESTED_LEAVES (no duplicate EditField)"
        );

        let unset = editable_fields(&Config::default());
        let row = unset.iter().find(|f| f.key == key).expect("row exists");
        assert_eq!(row.kind, EditKind::Bool);
        assert_eq!(row.seed.as_deref(), Some("false"), "resolved default OFF");

        let on: Config = toml::from_str("[matrix_rain]\nenabled = true").unwrap();
        let row_on = editable_fields(&on)
            .into_iter()
            .find(|f| f.key == key)
            .unwrap();
        assert_eq!(row_on.seed.as_deref(), Some("true"), "resolved ON");
    }

    /// The `[packages]` maintenance switches: Bool-typed dotted keys, sectioned in
    /// the search-only Packages section (which NO ordinary route owns — the
    /// special page renders them), grouped and searchable, seeded with the
    /// DIFFERING resolved defaults (auto_update ON, auto_install OFF).
    #[test]
    fn packages_consent_rows_surface_in_the_settings_model() {
        for key in [
            super::EDIT_PACKAGES_ENABLED,
            super::EDIT_PACKAGES_AUTO_UPDATE,
            super::EDIT_PACKAGES_AUTO_INSTALL,
        ] {
            assert_eq!(super::edit_kind(key), EditKind::Bool, "{key} types Bool");
            assert_eq!(super::section_of(key), super::Section::Packages);
            assert_eq!(super::group_of(key), ("Toolchain Packages", 0));
            assert!(!keywords_of(key).is_empty());
            assert!(
                !super::VISUAL_PREVIEW_KEYS.contains(&key),
                "Packages rows carry no preview obligation"
            );
        }
        assert!(super::group_footnote("Toolchain Packages").is_some());

        let fields = editable_fields(&Config::default());
        let enabled = fields
            .iter()
            .find(|f| f.key == super::EDIT_PACKAGES_ENABLED)
            .expect("background-service master row exists");
        assert_eq!(enabled.seed.as_deref(), Some("true"), "master defaults ON");
        let auto_update = fields
            .iter()
            .find(|f| f.key == super::EDIT_PACKAGES_AUTO_UPDATE)
            .expect("auto_update row exists");
        assert_eq!(auto_update.seed.as_deref(), Some("true"), "default ON");
        let auto_install = fields
            .iter()
            .find(|f| f.key == super::EDIT_PACKAGES_AUTO_INSTALL)
            .expect("auto_install row exists");
        assert_eq!(
            auto_install.seed.as_deref(),
            Some("false"),
            "consent-gated default OFF"
        );

        let configured: Config = toml::from_str(
            "[packages]\nenabled = false\nauto_update = false\nauto_install = true\n",
        )
        .unwrap();
        let fields = editable_fields(&configured);
        assert_eq!(
            fields
                .iter()
                .find(|f| f.key == super::EDIT_PACKAGES_ENABLED)
                .unwrap()
                .seed
                .as_deref(),
            Some("false")
        );
        assert_eq!(
            fields
                .iter()
                .find(|f| f.key == super::EDIT_PACKAGES_AUTO_UPDATE)
                .unwrap()
                .seed
                .as_deref(),
            Some("false")
        );
        assert_eq!(
            fields
                .iter()
                .find(|f| f.key == super::EDIT_PACKAGES_AUTO_INSTALL)
                .unwrap()
                .seed
                .as_deref(),
            Some("true")
        );
    }

    /// Dotted writes into `[packages]` are NON-DESTRUCTIVE around the keys the
    /// CO-LOCATED atpkg consumes from the same table (account/channel/links):
    /// the switch flips only its own child, and a blank-revert of the last GUI
    /// key keeps the atpkg-owned siblings (an emptied table is retained too —
    /// the nested writer never deletes a parent table).
    #[test]
    fn packages_dotted_writes_preserve_the_atpkg_owned_siblings() {
        let existing = "# consent ledger\n[packages]\naccount = \"alabsystems\" # index owner\nchannel = \"stable\"\n\n[packages.links]\nay = \"~/ay\"\n";
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
        let config: Config = toml::from_str(&out).expect("round-trips through serde");
        assert!(config.packages_auto_install());
        assert!(
            config.packages_auto_update(),
            "unrelated flag keeps its default"
        );

        // Blank = revert removes only the child; the table (with atpkg keys)
        // stays. OCC face: the dotted child projects as its own value.
        let reverted = apply_prefs_edits(&out, &[(super::EDIT_PACKAGES_AUTO_INSTALL, None)])
            .expect("dotted revert");
        assert!(!reverted.contains("auto_install"));
        assert!(
            reverted.contains("[packages]") && reverted.contains("account = \"alabsystems\""),
            "atpkg-owned keys keep the table alive: {reverted:?}"
        );
        let config: Config = toml::from_str(&reverted).unwrap();
        assert!(
            !config.packages_auto_install(),
            "back to the consent default"
        );

        // Bad value never reaches the file (typed_item fail-closed).
        assert!(matches!(
            apply_prefs_edits("", &[(super::EDIT_PACKAGES_AUTO_UPDATE, set("yes"))]),
            Err(PrefsEditError::BadValue { .. })
        ));
    }

    /// Setting a NEW key on an empty file writes it typed correctly and re-parses
    /// through `Config` (a Save → reload round-trip).
    #[test]
    fn set_new_key_on_empty_file() {
        let out = apply_prefs_edits("", &[(EDIT_FONT_PX, set("15.5"))]).unwrap();
        assert!(out.contains("font_px"), "wrote the key: {out:?}");
        // Floats are typed as TOML floats, not strings, so serde reads f32.
        let c: Config = toml::from_str(&out).expect("round-trips");
        assert_eq!(c.font_px, Some(15.5));
    }

    /// UPDATING an existing key changes only its value, leaving its line in place.
    #[test]
    fn update_existing_key() {
        let existing = "font_px = 12.0\ntheme = \"Dracula\"\n";
        let out = apply_prefs_edits(existing, &[(EDIT_FONT_PX, set("18.0"))]).unwrap();
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.font_px, Some(18.0));
        // The unrelated key is untouched.
        assert_eq!(c.theme.as_deref(), Some("Dracula"));
    }

    /// A `None` (blank/cleared) edit REMOVES the key — reverting it to its default —
    /// rather than writing an empty string.
    #[test]
    fn blank_removes_key() {
        let existing = "font_px = 12.0\ntheme = \"Dracula\"\n";
        let out = apply_prefs_edits(existing, &[(EDIT_FONT_PX, None)]).unwrap();
        assert!(!out.contains("font_px"), "key removed: {out:?}");
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.font_px, None);
        // Removing the cleared key does NOT touch the sibling.
        assert_eq!(c.theme.as_deref(), Some("Dracula"));
    }

    /// Removing an ALREADY-absent key is a clean no-op, not an error.
    #[test]
    fn remove_absent_key_is_noop() {
        let out = apply_prefs_edits("theme = \"Dracula\"\n", &[(EDIT_FONT_PX, None)]).unwrap();
        assert_eq!(out, "theme = \"Dracula\"\n");
    }

    /// COMMENTS and unrelated keys survive an edit — the whole point of the
    /// non-destructive (toml_edit DOM) write vs. a re-serialize.
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
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.theme.as_deref(), Some("Nord"));
        assert_eq!(c.font_px, Some(12.0));
    }

    /// The EDITED key's own inline (same-line) comment and `=`-spacing survive a
    /// value replacement — top-level and dotted-leaf alike. The sibling test above
    /// only proves comments survive on keys that are NOT being edited; this one
    /// closes the gap the wholesale-`Item`-replace used to have, where every
    /// slider drag / popup pick / `settings set` on an annotated key silently and
    /// irreversibly deleted the user's annotation.
    #[test]
    fn value_edit_preserves_the_edited_keys_inline_comment() {
        let existing = "\
# header comment stays
font_px = 12.0  # cozy size
scrollback_lines = 5000 # deep history

[net]
listen = \"127.0.0.1:7777\" # local only
";
        let out = apply_prefs_edits(
            existing,
            &[
                (EDIT_FONT_PX, set("18")),
                ("net.listen", set("0.0.0.0:7777")),
            ],
        )
        .unwrap();
        // The edited keys keep their EXACT line shape — value swapped, the
        // double-space alignment and inline comment intact on the same line.
        assert!(out.contains("font_px = 18.0  # cozy size"), "{out}");
        assert!(
            out.contains("listen = \"0.0.0.0:7777\" # local only"),
            "{out}"
        );
        // Untouched decor still survives too (header + unrelated key's comment).
        assert!(out.contains("# header comment stays"), "{out}");
        assert!(
            out.contains("scrollback_lines = 5000 # deep history"),
            "{out}"
        );
        // And the round trip stays serde-clean with the new values.
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.font_px, Some(18.0));
        assert_eq!(
            c.net.as_ref().and_then(|n| n.listen.as_deref()),
            Some("0.0.0.0:7777")
        );
        assert_eq!(c.scrollback_lines, Some(5000));
    }

    /// W6 (PROVEN: config round-trip for the new keys): a Save of every
    /// per-style / fallback font key through the REAL editor writes TOML that
    /// re-parses into `Config` with the exact values — including the
    /// `fallback_fonts` comma-string form the Text control produces, which the
    /// `FontList` deserializer must split back into the ordered list. The
    /// TOML-array spelling of `fallback_fonts` (a hand-edited file) parses to
    /// the same value, so the two forms are interchangeable.
    #[test]
    fn w6_font_keys_round_trip() {
        let out = apply_prefs_edits(
            "",
            &[
                (super::EDIT_FONT_FAMILY_BOLD, set("JetBrains Mono Bold")),
                (super::EDIT_FONT_FAMILY_ITALIC, set("JetBrains Mono Italic")),
                (
                    super::EDIT_FONT_FAMILY_BOLD_ITALIC,
                    set("JetBrains Mono Bold Italic"),
                ),
                (super::EDIT_FONT_SYNTHETIC_STYLE, set("false")),
                (
                    super::EDIT_FALLBACK_FONTS,
                    set("Sarasa Mono, Apple Symbols"),
                ),
                (super::EDIT_SYMBOL_FONT, set("STIX Two Math")),
                (super::EDIT_EMOJI_FONT, set("Noto Color Emoji")),
            ],
        )
        .unwrap();
        let c: Config = toml::from_str(&out).expect("round-trips");
        assert_eq!(c.font_family_bold.as_deref(), Some("JetBrains Mono Bold"));
        assert_eq!(
            c.font_family_italic.as_deref(),
            Some("JetBrains Mono Italic")
        );
        assert_eq!(
            c.font_family_bold_italic.as_deref(),
            Some("JetBrains Mono Bold Italic")
        );
        assert_eq!(c.font_synthetic_style, Some(false));
        // The comma-string form splits into the ORDERED list (precedence is
        // list position — the proven fallback_chain_order law).
        assert_eq!(
            c.fallback_fonts.as_ref().map(|l| l.0.clone()),
            Some(vec!["Sarasa Mono".to_string(), "Apple Symbols".to_string()])
        );
        assert_eq!(c.symbol_font.as_deref(), Some("STIX Two Math"));
        assert_eq!(c.emoji_font.as_deref(), Some("Noto Color Emoji"));
        // The TOML-array spelling parses to the SAME value.
        let arr: Config =
            toml::from_str("fallback_fonts = [\"Sarasa Mono\", \"Apple Symbols\"]").unwrap();
        assert_eq!(arr.fallback_fonts, c.fallback_fonts);
        // And every new key surfaces in the Settings model (search visibility):
        // present in the schema rows, grouped under Typography.
        let cfg: Config = toml::from_str(&out).unwrap();
        let fields = editable_fields(&cfg);
        for key in [
            super::EDIT_FONT_FAMILY_BOLD,
            super::EDIT_FONT_FAMILY_ITALIC,
            super::EDIT_FONT_FAMILY_BOLD_ITALIC,
            super::EDIT_FONT_SYNTHETIC_STYLE,
            super::EDIT_FALLBACK_FONTS,
            super::EDIT_SYMBOL_FONT,
            super::EDIT_EMOJI_FONT,
        ] {
            assert!(
                fields.iter().any(|f| f.key == key),
                "{key} missing from the settings rows"
            );
            assert_eq!(super::section_of(key), super::Section::Typography);
            assert!(
                !keywords_of(key).is_empty(),
                "{key} needs search keywords for the settings search"
            );
        }
        // The list row seeds the comma-joined string the loader parses back.
        let row = fields
            .iter()
            .find(|f| f.key == super::EDIT_FALLBACK_FONTS)
            .unwrap();
        assert_eq!(row.seed.as_deref(), Some("Sarasa Mono, Apple Symbols"));
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
        let c: Config = toml::from_str(&out).expect("round-trips through serde");
        // The load-adaptive shedding toggle is typed Bool (not a corrupting string) and
        // opts out of the render-overload heuristic.
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
        // The comma-string font_variation splits into the ORDERED axis list...
        assert_eq!(
            c.font_variation.as_ref().map(|l| l.0.clone()),
            Some(vec!["wght=450".to_string(), "opsz=14".to_string()])
        );
        // ...the TOML-array spelling parses to the same value...
        let arr: Config = toml::from_str("font_variation = [\"wght=450\", \"opsz=14\"]").unwrap();
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

        // Every new key surfaces in the settings model: present, searchable, sectioned.
        let fields = editable_fields(&c);
        for (key, section) in [
            (super::EDIT_TEXT_BLENDING, super::Section::Typography),
            (super::EDIT_FONT_THICKEN, super::Section::Typography),
            (super::EDIT_STEM_GAMMA, super::Section::Typography),
            (super::EDIT_FONT_WEIGHT, super::Section::Typography),
            (super::EDIT_FONT_VARIATION, super::Section::Typography),
            (super::EDIT_MOTION, super::Section::Cursor),
            (super::EDIT_SERIOUS_MODE, super::Section::Cursor),
            (super::EDIT_LOAD_ADAPTIVE_MOTION, super::Section::Cursor),
        ] {
            assert!(
                fields.iter().any(|f| f.key == key),
                "{key} missing from the settings rows"
            );
            assert_eq!(super::section_of(key), section, "{key} section");
            assert!(!keywords_of(key).is_empty(), "{key} needs search keywords");
        }
        // The variable-font row seeds the comma-joined string the loader parses back.
        let row = fields
            .iter()
            .find(|f| f.key == super::EDIT_FONT_VARIATION)
            .unwrap();
        assert_eq!(row.seed.as_deref(), Some("wght=450, opsz=14"));
    }

    /// Each key is typed PER ITS `Config` field: float / int / bool / string — so a
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
        let c: Config = toml::from_str(&out).expect("typed values round-trip");
        assert_eq!(c.font_px, Some(14.0)); // float, even from "14"
        assert_eq!(c.scrollback_lines, Some(50000)); // integer
        assert_eq!(c.copy_on_select, Some(true)); // bool
        assert_eq!(c.theme.as_deref(), Some("Dracula")); // string
        assert_eq!(c.font_family.as_deref(), Some("JetBrains Mono")); // string w/ space
        assert_eq!(c.cursor_style.as_deref(), Some("bar")); // string
        // The numeric fields are NOT quoted strings in the output.
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

    /// A non-integer scrollback (a float string) is rejected — `scrollback_lines` is a
    /// usize, so "1.5" must not silently truncate or write a float.
    #[test]
    fn float_for_integer_field_is_rejected() {
        let err = apply_prefs_edits("", &[(EDIT_SCROLLBACK, set("1.5"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
    }

    /// A malformed EXISTING file is refused (`Parse`) rather than overwritten, so a
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
        let c: Config = toml::from_str(&out).unwrap();
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
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.theme.as_deref(), Some("dark:Dracula,light:GitHub Light"));
    }

    /// An `Enum` value (cursor_trail_style) is validated against its options and stored
    /// in its CANONICAL spelling: a case-variant input ("Phaser") writes "phaser" and
    /// round-trips through serde unchanged.
    #[test]
    fn enum_value_is_validated_and_canonicalised() {
        let out = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("Phaser"))]).unwrap();
        assert!(
            out.contains("cursor_trail_style = \"phaser\""),
            "canonical lower-case spelling written: {out}"
        );
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.cursor_trail_style.as_deref(), Some("phaser"));
        // `fire` likewise — the second look the owner asked about.
        let out = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("fire"))]).unwrap();
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.cursor_trail_style.as_deref(), Some("fire"));
    }

    /// An out-of-domain `Enum` value is REJECTED (`BadValue`) so a typo'd style can never
    /// be saved (and silently fall back to lumen at reload).
    #[test]
    fn bad_enum_value_is_rejected() {
        let err = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("rainbwo"))]).unwrap_err();
        match err {
            PrefsEditError::BadValue { key, .. } => assert_eq!(key, EDIT_CURSOR_TRAIL_STYLE),
            other => panic!("expected BadValue, got {other:?}"),
        }
    }

    /// A `pack:<id>` selection (a loaded Trail Pack, offered by the dynamic picker)
    /// saves VERBATIM through the `cursor_trail_style` Enum row — it is not a
    /// canonical built-in, but the engine resolves/fail-closes it at load. An empty
    /// `pack:` (no id) is still rejected.
    #[test]
    fn pack_trail_style_saves_verbatim() {
        let out =
            apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("pack:synthwave"))]).unwrap();
        assert!(
            out.contains("cursor_trail_style = \"pack:synthwave\""),
            "pack selection written verbatim: {out}"
        );
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.cursor_trail_style.as_deref(), Some("pack:synthwave"));
        // An empty pack ref is not a valid pack AND not a canonical option → rejected.
        let err = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("pack:"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::BadValue { .. }));
    }

    #[test]
    fn initial_grid_placeholders_match_fixed_runtime_defaults() {
        let fields = editable_fields(&Config::default());
        let field = |key: &str| {
            fields
                .iter()
                .find(|field| field.key == key)
                .unwrap_or_else(|| panic!("missing field {key}"))
        };
        assert_eq!(field(EDIT_COLUMNS).seed, None);
        assert_eq!(field(EDIT_COLUMNS).placeholder, "80 (default)");
        assert_eq!(field(EDIT_LINES).seed, None);
        assert_eq!(field(EDIT_LINES).placeholder, "24 (default)");

        let configured: Config = toml::from_str("columns = 132\nlines = 50\n").unwrap();
        let fields = editable_fields(&configured);
        let field = |key: &str| {
            fields
                .iter()
                .find(|field| field.key == key)
                .unwrap_or_else(|| panic!("missing field {key}"))
        };
        assert_eq!(field(EDIT_COLUMNS).seed.as_deref(), Some("132"));
        assert_eq!(field(EDIT_COLUMNS).placeholder, "132");
        assert_eq!(field(EDIT_LINES).seed.as_deref(), Some("50"));
        assert_eq!(field(EDIT_LINES).placeholder, "50");
    }

    /// The cursor-trail rows are present: the master toggle seeds its RESOLVED state (ON
    /// by default — the owner's batteries-on delight call) and the style enum seeds blank
    /// on an unset config but advertises its effective default in the placeholder.
    #[test]
    fn editable_fields_includes_cursor_trail() {
        let fields = editable_fields(&Config::default());
        let f = |k: &str| fields.iter().find(|f| f.key == k).expect("row present");
        assert_eq!(f(EDIT_CURSOR_TRAIL).seed.as_deref(), Some("true"));
        assert_eq!(f(EDIT_CURSOR_TRAIL_STYLE).seed, None);
        assert_eq!(
            f(EDIT_CURSOR_TRAIL_STYLE).placeholder,
            format!("{} (default)", super::DEFAULT_CURSOR_TRAIL_STYLE),
            "style placeholder shows the effective default"
        );
        assert!(
            matches!(
                f(EDIT_CURSOR_TRAIL_STYLE).kind,
                super::EditKind::Enum { .. }
            ),
            "style row is an Enum control"
        );
    }

    /// Every shipping cursor-effect knob has a typed Settings row and survives the
    /// exact non-destructive writer used by the native app. This is the completeness
    /// guard that prevents renderer capabilities from becoming hidden TOML folklore.
    #[test]
    fn cursor_effect_controls_are_complete_typed_and_round_trip() {
        let edits = [
            (super::EDIT_CURSOR_TRAIL_COLOR, set("#40C8FF")),
            (super::EDIT_CURSOR_TRAIL_ACCENT, set("#FF77AA")),
            (super::EDIT_CURSOR_NYAN_SPRITE, set("~/cat.png")),
            (super::EDIT_CURSOR_TRAIL_MS, set("420")),
            (super::EDIT_CURSOR_TRAIL_LENGTH, set("48")),
            (super::EDIT_CURSOR_TRAIL_INTENSITY, set("0.9")),
            (super::EDIT_CURSOR_TRAIL_RADIUS, set("1.2")),
            (super::EDIT_CURSOR_TRAIL_RING, set("false")),
            (super::EDIT_CURSOR_TRAIL_BLOOM, set("true")),
            (super::EDIT_CURSOR_TRAIL_BLOOM_STRENGTH, set("1.4")),
            (super::EDIT_CURSOR_TRAIL_BLOOM_RADIUS, set("3.5")),
            (super::EDIT_CURSOR_FIRE_SHIMMER, set("false")),
            (super::EDIT_HDR_GLOW, set("true")),
            (super::EDIT_CURSOR_GLOW_SDR_BOOST, set("0.4")),
        ];
        let out = apply_prefs_edits("", &edits).expect("cursor effects write typed TOML");
        let cfg: Config = toml::from_str(&out).expect("cursor effects reparse through Config");
        assert_eq!(cfg.cursor_trail_color.as_deref(), Some("#40C8FF"));
        assert_eq!(cfg.cursor_trail_accent.as_deref(), Some("#FF77AA"));
        assert_eq!(cfg.cursor_nyan_sprite.as_deref(), Some("~/cat.png"));
        assert_eq!(cfg.cursor_trail_ms, Some(420));
        assert_eq!(cfg.cursor_trail_length, Some(48));
        assert_eq!(cfg.cursor_trail_intensity, Some(0.9));
        assert_eq!(cfg.cursor_trail_radius, Some(1.2));
        assert_eq!(cfg.cursor_trail_ring, Some(false));
        assert_eq!(cfg.cursor_trail_bloom, Some(true));
        assert_eq!(cfg.cursor_trail_bloom_strength, Some(1.4));
        assert_eq!(cfg.cursor_trail_bloom_radius, Some(3.5));
        assert_eq!(cfg.cursor_fire_shimmer, Some(false));
        assert_eq!(cfg.hdr_glow, Some(true));
        assert_eq!(cfg.cursor_glow_sdr_boost, Some(0.4));

        let fields = editable_fields(&cfg);
        for (key, _) in edits {
            let field = fields
                .iter()
                .find(|field| field.key == key)
                .unwrap_or_else(|| panic!("missing native Settings row for {key}"));
            // The sprite art followed the cat to its own pane on 2026-08-10;
            // everything else here still tunes the trail engine.
            let expected_section = if key == super::EDIT_CURSOR_NYAN_SPRITE {
                super::Section::CursorKitty
            } else {
                super::Section::Cursor
            };
            assert_eq!(super::section_of(key), expected_section, "{key}");
            assert_eq!(field.kind, super::edit_kind(key), "{key} kind drift");
        }
    }

    /// A colour control validates as hex on Save: a good `#RRGGBB` round-trips as a
    /// string, a malformed colour is rejected (BadValue) rather than written.
    #[test]
    fn color_value_is_validated_on_save() {
        let out = apply_prefs_edits("", &[(EDIT_FOREGROUND, set("#1E2030"))]).unwrap();
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.foreground.as_deref(), Some("#1E2030"));
        let err = apply_prefs_edits("", &[(EDIT_BACKGROUND, set("not-a-color"))]).unwrap_err();
        match err {
            PrefsEditError::BadValue { key, .. } => assert_eq!(key, EDIT_BACKGROUND),
            other => panic!("expected BadValue, got {other:?}"),
        }
    }

    /// Every colour key classifies as `EditKind::Color` (so Save validates it as hex).
    #[test]
    fn color_keys_classify_as_color() {
        for key in [
            EDIT_FOREGROUND,
            EDIT_BACKGROUND,
            EDIT_CURSOR_COLOR,
            EDIT_SELECTION_COLOR,
            super::EDIT_CURSOR_TRAIL_COLOR,
            super::EDIT_CURSOR_TRAIL_ACCENT,
        ] {
            assert!(
                matches!(super::edit_kind(key), super::EditKind::Color),
                "edit_kind({key}) is Color"
            );
        }
    }

    /// P1.4b scalar keys: security toggles classify Bool (fail-closed), geometry classifies
    /// Integer, and both round-trip through serde as the correct TOML type.
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
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.allow_osc52_query, Some(true)); // TOML bool, not a string
        assert_eq!(c.columns, Some(100)); // TOML int, not a string
    }

    #[test]
    fn launch_only_timing_metadata_is_exact_and_shared() {
        for key in [super::EDIT_COLUMNS, super::EDIT_LINES] {
            assert_eq!(
                super::application_timing(key),
                Some(
                    "Applies on a fresh launch; an authenticated update handoff preserves the live size"
                )
            );
        }
        for key in [
            super::EDIT_GPU,
            super::EDIT_PACKAGES_AUTO_UPDATE,
            super::EDIT_PACKAGES_ENABLED,
            "net.listen",
            "net.cert",
            "net.key",
        ] {
            assert_eq!(super::application_timing(key), Some("Applies next launch"));
        }
        for key in [
            super::EDIT_ALLOW_KITTY_FILE_TRANSFER,
            super::EDIT_TEMPORAL_RECORDING,
            super::EDIT_SHELL,
            super::EDIT_SHELL_ARGS,
        ] {
            assert_eq!(
                super::application_timing(key),
                Some("Applies to new sessions")
            );
        }
        assert_eq!(
            super::application_timing(super::EDIT_HDR_GLOW),
            Some("Disabling applies now; enabling may require a new window")
        );
        assert_eq!(
            super::application_timing(super::EDIT_RESTORE_SESSION),
            Some("Applies when closing or next launch")
        );
        for key in [
            super::EDIT_PACKAGES_AUTO_INSTALL,
            "packages.account",
            "packages.channel",
            "packages.include",
            "packages.exclude",
            "packages.links",
        ] {
            assert_eq!(
                super::application_timing(key),
                Some("Applies on the next package operation"),
                "{key}"
            );
        }
        assert_eq!(
            super::application_timing("net.connections.host"),
            Some("Applies on the next dial")
        );
        assert_eq!(
            super::application_timing("update.owner"),
            Some("Manual checks use this now; automatic checks use it next launch")
        );
        assert_eq!(
            super::application_timing("update.auto_apply"),
            Some("Applies on the next update transition")
        );
        assert_eq!(
            super::application_timing(super::EDIT_AMBIGUOUS_WIDTH),
            Some("Applies to newly received text; existing cells keep their current width")
        );
        assert!(
            super::application_has_live_effect(super::EDIT_AMBIGUOUS_WIDTH),
            "the width policy is consumed immediately for subsequent input"
        );
        assert_eq!(
            super::application_timing(super::EDIT_MATRIX_RAIN_ENABLED),
            Some("Applies live unless this session has a View menu override")
        );
        assert!(
            super::application_has_live_effect(super::EDIT_MATRIX_RAIN_ENABLED),
            "the config switch is live whenever no session-local override owns the effect"
        );
        assert!(
            super::application_has_live_effect(super::EDIT_HDR_GLOW),
            "disabling HDR glow gates the next present in an existing GPU window"
        );
        for key in [super::EDIT_GPU, super::EDIT_SHELL, "net.connections.host"] {
            assert!(
                !super::application_has_live_effect(key),
                "{key} is wholly deferred"
            );
        }
        for key in [
            super::EDIT_THEME,
            super::EDIT_FONT_PX,
            super::EDIT_CURSOR_BLINK,
            super::EDIT_TAB_STRIP_ROWS,
        ] {
            assert_eq!(super::application_timing(key), None, "{key} applies live");
            assert!(
                super::application_has_live_effect(key),
                "{key} applies live"
            );
        }
    }

    #[test]
    fn audited_advanced_labels_defaults_and_consequence_notes_are_plain_and_exact() {
        let fields = editable_fields(&Config::default());
        let field = |key: &str| {
            fields
                .iter()
                .find(|field| field.key == key)
                .unwrap_or_else(|| panic!("missing field {key}"))
        };
        assert_eq!(field(super::EDIT_BIDI).label, "Bidirectional text");
        assert_eq!(
            field(super::EDIT_AMBIGUOUS_WIDTH).label,
            "Ambiguous-character width"
        );
        assert_eq!(field(super::EDIT_FONT_WEIGHT).label, "Variable font weight");
        let search = field(super::EDIT_SEARCH_HISTORY_LINES);
        assert_eq!(search.label, "Searchable scrollback lines");
        assert_eq!(
            search.placeholder,
            format!("{} (default)", aterm_core::search::DEFAULT_MAX_CACHED_LINES)
        );

        let keyboard = super::group_footnote("Keyboard").unwrap();
        assert!(keyboard.contains("confirmed echo"));
        assert!(keyboard.contains("passwords"));
        assert!(keyboard.contains("Manual's Always mode is unsafe"));
        let paste = super::group_footnote("Paste safety").unwrap();
        assert!(paste.contains("unbracketed multiline paste"));
        assert!(paste.contains("Bracketed paste bypasses it"));
        let scrollback = super::group_footnote("Scrollback").unwrap();
        assert!(scrollback.contains("Cmd-F/socket cap"));
        assert!(scrollback.contains("partial results"));
        assert!(scrollback.contains("Set Searchable lines to 0 for live-screen-only search"));
        let semantics = super::group_footnote("Text direction & width").unwrap();
        assert!(semantics.contains("reorders right-to-left text"));
        assert!(semantics.contains("one or two cells"));
        assert!(semantics.contains("not existing cells"));
        let rendering = super::group_footnote("Rendering").unwrap();
        assert!(rendering.contains("needs a font with a wght axis"));
        assert!(rendering.contains("static fonts ignore it"));
        assert_eq!(
            field(super::EDIT_ALLOW_PALETTE_RECONFIGURE).label,
            "Allow programs to set indexed colors (OSC 4/21)"
        );
        let packages = super::group_footnote("Toolchain Packages").unwrap();
        assert!(packages.contains("auto-update apply next launch"));
        assert!(packages.contains("Auto-install runs next package operation"));
    }

    /// P1.4c enum-domain keys: cursor_style/window_theme/bidi/ambiguous_width classify as
    /// Enum with the canonical option lists, and each option writes back as itself (the
    /// spelling the config loader accepts — the loader mappings are tested in app_config).
    #[test]
    fn enum_domain_keys_classify_and_round_trip_canonical() {
        let cases: [(&str, &[&str]); 5] = [
            (EDIT_CURSOR_STYLE, super::CURSOR_STYLES),
            (super::EDIT_WINDOW_THEME, super::WINDOW_THEMES),
            (super::EDIT_BIDI, super::BIDI_MODES),
            (super::EDIT_AMBIGUOUS_WIDTH, super::AMBIGUOUS_WIDTHS),
            (super::EDIT_TRAIL_SOUND_STYLE, super::TRAIL_SOUND_STYLES),
        ];
        for (key, opts) in cases {
            match super::edit_kind(key) {
                super::EditKind::Enum { options } => assert_eq!(options, opts, "{key} options"),
                other => panic!("{key} should be Enum, got {other:?}"),
            }
            for &o in opts {
                let out = apply_prefs_edits("", &[(key, set(o))]).unwrap();
                assert!(
                    out.contains(&format!("{key} = \"{o}\"")),
                    "{key}={o} writes its canonical string: {out}"
                );
            }
        }
    }

    /// Smart Titles is one complete, searchable group in Window & Tabs. Every scalar
    /// uses its real TOML type, and every string enum exposes only canonical values.
    #[test]
    fn smart_title_schema_is_complete_typed_and_searchable() {
        let fields = editable_fields(&Config::default());
        let grouped: Vec<&str> = fields
            .iter()
            .filter(|field| super::group_of(field.key).0 == "Smart Titles")
            .map(|field| field.key)
            .collect();
        assert_eq!(
            grouped,
            super::SMART_TITLE_KEYS,
            "the UI group and health-invalidation roster must not drift"
        );
        for &key in super::SMART_TITLE_KEYS {
            let row = fields
                .iter()
                .find(|field| field.key == key)
                .unwrap_or_else(|| panic!("missing Smart Titles row {key}"));
            assert_eq!(super::section_of(key), super::Section::Window, "{key}");
            assert_eq!(super::group_of(key).0, "Smart Titles", "{key}");
            assert!(!keywords_of(key).is_empty(), "{key} must be searchable");
            assert!(
                keywords_of(key).contains(&"smart title"),
                "the group-level search phrase must find {key}"
            );
            assert_eq!(row.kind, super::edit_kind(key), "{key} row/writer type");
        }
        for key in [
            super::EDIT_DESCRIPTIVE_TITLES,
            super::EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT,
            super::EDIT_TITLE_SUMMARY_ALLOW_REMOTE,
        ] {
            assert!(matches!(super::edit_kind(key), EditKind::Bool), "{key}");
        }
        for key in [
            super::EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS,
            super::EDIT_TITLE_SUMMARY_INTERVAL_SECONDS,
            super::EDIT_TITLE_SUMMARY_CONTEXT_LINES,
        ] {
            assert!(matches!(super::edit_kind(key), EditKind::Integer), "{key}");
        }
        assert!(
            super::group_footnote("Smart Titles").is_some_and(|note| note.contains("Activity")
                && note.contains("untrusted network providers")
                && note.contains("HTTP(S)_PROXY")
                && note.contains("replaces platform roots")
                && note.contains("heuristic")
                && note.contains("cannot identify every secret")
                && note.contains("path-only")),
            "the group must explain precedence, provider trust, transport, and the limits of secret handling"
        );
    }

    /// Unset fields remain blank (so Save can preserve an unset config) while the
    /// controls still communicate all effective defaults. Bools seed their resolved
    /// state because checkboxes have no placeholder channel.
    #[test]
    fn smart_title_defaults_seed_and_placeholder_correctly() {
        let fields = editable_fields(&Config::default());
        let row = |key: &str| {
            fields
                .iter()
                .find(|field| field.key == key)
                .unwrap_or_else(|| panic!("missing row {key}"))
        };
        assert_eq!(
            row(super::EDIT_DESCRIPTIVE_TITLES).seed.as_deref(),
            Some("true")
        );
        assert_eq!(
            row(super::EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT)
                .seed
                .as_deref(),
            Some("true")
        );
        assert_eq!(
            row(super::EDIT_TITLE_SUMMARY_ALLOW_REMOTE).seed.as_deref(),
            Some("false")
        );
        for key in [
            super::EDIT_TITLE_SUMMARY_PROVIDER,
            super::EDIT_TITLE_SUMMARY_MODEL,
            super::EDIT_TITLE_SUMMARY_ENDPOINT,
            super::EDIT_TITLE_SUMMARY_TOKEN_FILE,
            super::EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS,
            super::EDIT_TITLE_SUMMARY_PROXY_MODE,
            super::EDIT_TITLE_SUMMARY_CA_FILE,
            super::EDIT_TITLE_SUMMARY_INTERVAL_SECONDS,
            super::EDIT_TITLE_SUMMARY_CONTEXT_LINES,
            super::EDIT_TAB_TITLE_FORMAT,
            super::EDIT_WINDOW_TITLE_FORMAT,
        ] {
            assert_eq!(row(key).seed, None, "{key} stays unset");
            assert!(
                !row(key).placeholder.is_empty(),
                "{key} needs a default hint"
            );
        }
        assert_eq!(
            row(super::EDIT_TITLE_SUMMARY_PROVIDER).placeholder,
            "builtin (default)"
        );
        assert_eq!(
            row(super::EDIT_TITLE_SUMMARY_ENDPOINT).placeholder,
            "not used by selected provider"
        );
        assert_eq!(
            row(super::EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS).placeholder,
            "20 (default)"
        );
        assert_eq!(
            row(super::EDIT_TITLE_SUMMARY_PROXY_MODE).placeholder,
            "environment (default)"
        );
        assert_eq!(
            row(super::EDIT_TITLE_SUMMARY_CA_FILE).placeholder,
            "platform trust roots (default)"
        );
        assert_eq!(
            row(super::EDIT_TITLE_SUMMARY_INTERVAL_SECONDS).placeholder,
            "15 (default)"
        );
        assert_eq!(
            row(super::EDIT_TITLE_SUMMARY_CONTEXT_LINES).placeholder,
            "24 (default)"
        );
        assert_eq!(
            row(super::EDIT_TAB_TITLE_FORMAT).placeholder,
            "title-description (default)"
        );
    }

    #[test]
    fn smart_title_endpoint_hint_matches_the_selected_provider() {
        let endpoint_hint = |provider| {
            let mut config = Config {
                title_summary_provider: Some(provider),
                ..Config::default()
            };
            // A stale blank string has the same effective meaning as an absent key.
            config.title_summary_endpoint = Some("   ".to_string());
            editable_fields(&config)
                .into_iter()
                .find(|field| field.key == super::EDIT_TITLE_SUMMARY_ENDPOINT)
                .expect("endpoint row")
                .placeholder
        };

        assert!(
            endpoint_hint(crate::app_config::TitleSummaryProvider::Ollama)
                .starts_with("blank = automatic private endpoint")
        );
        assert!(
            endpoint_hint(crate::app_config::TitleSummaryProvider::OpenAiCompatible)
                .starts_with("required: https://")
        );
        for provider in [
            crate::app_config::TitleSummaryProvider::Builtin,
            crate::app_config::TitleSummaryProvider::Off,
        ] {
            assert_eq!(endpoint_hint(provider), "not used by selected provider");
        }
    }

    /// A Save writes every Smart Titles control using serde-compatible TOML types and
    /// configured values seed back into the exact controls on the next open.
    #[test]
    fn smart_title_edits_round_trip_and_reseed() {
        let out = apply_prefs_edits(
            "",
            &[
                (super::EDIT_DESCRIPTIVE_TITLES, set("false")),
                (super::EDIT_TITLE_SUMMARY_PROVIDER, set("ollama")),
                (super::EDIT_TITLE_SUMMARY_MODEL, set("qwen3.5:4b-q4_K_M")),
                (
                    super::EDIT_TITLE_SUMMARY_ENDPOINT,
                    set("http://127.0.0.1:11434/api/chat"),
                ),
                (
                    super::EDIT_TITLE_SUMMARY_TOKEN_FILE,
                    set("/tmp/aterm-summary-token"),
                ),
                (super::EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS, set("42")),
                (super::EDIT_TITLE_SUMMARY_PROXY_MODE, set("direct")),
                (
                    super::EDIT_TITLE_SUMMARY_CA_FILE,
                    set("/tmp/private-model-ca.pem"),
                ),
                (super::EDIT_TITLE_SUMMARY_INTERVAL_SECONDS, set("30")),
                (super::EDIT_TITLE_SUMMARY_CONTEXT_LINES, set("40")),
                (super::EDIT_TITLE_SUMMARY_INCLUDE_OUTPUT, set("false")),
                (super::EDIT_TITLE_SUMMARY_ALLOW_REMOTE, set("true")),
                (super::EDIT_TAB_TITLE_FORMAT, set("description-title")),
                (super::EDIT_WINDOW_TITLE_FORMAT, set("description")),
            ],
        )
        .expect("typed Smart Titles edits");
        assert!(out.contains("descriptive_titles = false"));
        assert!(out.contains("title_summary_interval_seconds = 30"));
        assert!(out.contains("title_summary_context_lines = 40"));
        assert!(out.contains("title_summary_provider = \"ollama\""));
        assert!(out.contains("title_summary_timeout_seconds = 42"));
        assert!(out.contains("title_summary_proxy_mode = \"direct\""));
        assert!(out.contains("title_summary_ca_file = \"/tmp/private-model-ca.pem\""));

        let cfg: Config = toml::from_str(&out).expect("Smart Titles config re-parses");
        assert_eq!(cfg.descriptive_titles, Some(false));
        assert_eq!(
            cfg.title_summary_provider
                .as_ref()
                .map(|value| value.as_str()),
            Some("ollama")
        );
        assert_eq!(cfg.title_summary_interval_seconds, Some(30));
        assert_eq!(cfg.title_summary_context_lines, Some(40));
        assert_eq!(cfg.title_summary_timeout_seconds, Some(42));
        assert_eq!(
            cfg.title_summary_proxy_mode.map(|mode| mode.as_str()),
            Some("direct")
        );
        assert_eq!(
            cfg.title_summary_ca_file.as_deref(),
            Some("/tmp/private-model-ca.pem")
        );
        assert_eq!(cfg.title_summary_include_output, Some(false));
        assert_eq!(cfg.title_summary_allow_remote, Some(true));
        assert_eq!(
            cfg.tab_title_format.as_ref().map(|value| value.as_str()),
            Some("description-title")
        );
        assert_eq!(
            cfg.window_title_format.as_ref().map(|value| value.as_str()),
            Some("description")
        );

        let fields = editable_fields(&cfg);
        let seed = |key: &str| {
            fields
                .iter()
                .find(|field| field.key == key)
                .and_then(|field| field.seed.as_deref())
        };
        assert_eq!(seed(super::EDIT_TITLE_SUMMARY_PROVIDER), Some("ollama"));
        assert_eq!(
            seed(super::EDIT_TITLE_SUMMARY_MODEL),
            Some("qwen3.5:4b-q4_K_M")
        );
        assert_eq!(seed(super::EDIT_TITLE_SUMMARY_INTERVAL_SECONDS), Some("30"));
        assert_eq!(seed(super::EDIT_TITLE_SUMMARY_CONTEXT_LINES), Some("40"));
        assert_eq!(seed(super::EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS), Some("42"));
        assert_eq!(seed(super::EDIT_TITLE_SUMMARY_PROXY_MODE), Some("direct"));
        assert_eq!(
            seed(super::EDIT_TITLE_SUMMARY_CA_FILE),
            Some("/tmp/private-model-ca.pem")
        );
        assert_eq!(
            seed(super::EDIT_TITLE_SUMMARY_TOKEN_FILE),
            Some("/tmp/aterm-summary-token")
        );
    }

    /// The token setting stores a FILE PATH, never credential material. The shared
    /// typed writer rejects obvious pasted API keys, authorization headers, JWTs,
    /// and URLs before it can produce replacement TOML. A bad token-file value also
    /// makes the whole edit batch fail, so an earlier valid edit cannot be partially
    /// persisted by either the Settings overlay or native/control Settings service.
    #[test]
    fn smart_title_token_file_rejects_inline_credentials_before_toml() {
        let pasted_values = vec![
            "sk_pasted_bearer_not_a_file_path".to_string(),
            "Bearer pasted-secret-value".to_string(),
            "eyJhbGciOiJub25lIn0.payload.signature".to_string(),
            "https://credentials.example.test/pasted-bearer".to_string(),
            ["AK", "IA", "ABCDEFGHIJKLMNOP"].concat(),
            ["AI", "za", "abcdefghijklmnopqrstuvwxyz012345"].concat(),
            "0123456789abcdef".repeat(4),
            "lowercaseopaque0123456789abcdefghi".to_string(),
        ];

        for pasted in &pasted_values {
            let err = apply_prefs_edits(
                "theme = \"Dracula\"\n",
                &[
                    (super::EDIT_THEME, set("Nord")),
                    (super::EDIT_TITLE_SUMMARY_TOKEN_FILE, set(pasted)),
                ],
            )
            .expect_err("pasted credential material must not produce TOML");
            let diagnostic = err.to_string();
            assert!(
                !diagnostic.contains(pasted.as_str()),
                "rejected credential leaked through its diagnostic: {diagnostic}"
            );
            assert!(diagnostic.contains("expected a file path"), "{diagnostic}");
            match err {
                PrefsEditError::SensitiveBadValue { key, .. } => {
                    assert_eq!(key, super::EDIT_TITLE_SUMMARY_TOKEN_FILE)
                }
                other => panic!("expected redacted token-file error, got {other:?}"),
            }

            // This is the exact string carried by `save_prefs_edits` into both its
            // stderr line and the Settings status surface.
            let outcome = super::SaveOutcome::Error(diagnostic);
            let super::SaveOutcome::Error(message) = outcome else {
                unreachable!();
            };
            assert!(!message.contains(pasted.as_str()));
        }
    }

    /// Absolute, home-relative, and ordinary relative paths remain valid. Dots and
    /// hyphens in a filename — including a dotted name that is not a JWT — must not
    /// be mistaken for inline credential material.
    #[test]
    fn smart_title_token_file_paths_round_trip_without_false_positives() {
        for path in [
            "/Users//example/.config/aterm/title-summary.token",
            "~/.config/aterm/title-summary-token",
            "../secrets/provider-token.txt",
            "relative.token-file.with-dots",
            "risk-analysis.txt",
            "sk_prod/token-file",
        ] {
            let out = apply_prefs_edits("", &[(super::EDIT_TITLE_SUMMARY_TOKEN_FILE, set(path))])
                .unwrap_or_else(|error| panic!("valid path {path:?} was rejected: {error}"));
            let cfg: Config = toml::from_str(&out).expect("token-file TOML re-parses");
            assert_eq!(cfg.title_summary_token_file.as_deref(), Some(path));
        }
    }

    #[test]
    fn smart_title_ca_file_rejects_inline_pem_without_diagnostic_disclosure() {
        for pasted in [
            "-----BEGIN CERTIFICATE-----MIIBpasted",
            concat!("-----BEGIN ", "PRIVATE KEY-----pasted-secret"),
            "https://certificates.example.test/root.pem",
            "-----BEGIN CERTIFICATE-----\npasted",
        ] {
            let err = apply_prefs_edits(
                "theme = \"Dracula\"\n",
                &[
                    (super::EDIT_THEME, set("Nord")),
                    (super::EDIT_TITLE_SUMMARY_CA_FILE, set(pasted)),
                ],
            )
            .expect_err("inline certificate/key material must not produce TOML");
            let diagnostic = err.to_string();
            assert!(!diagnostic.contains(pasted), "PEM leaked: {diagnostic}");
            assert!(diagnostic.contains("expected a file path"), "{diagnostic}");
            assert!(matches!(
                err,
                PrefsEditError::SensitiveBadValue { key, .. }
                    if key == super::EDIT_TITLE_SUMMARY_CA_FILE
            ));
            let outcome = super::SaveOutcome::Error(diagnostic);
            let super::SaveOutcome::Error(message) = outcome else {
                unreachable!();
            };
            assert!(!message.contains(pasted));
        }

        for path in [
            "/Users//example/.config/aterm/private-model-ca.pem",
            "~/.config/aterm/root-ca.pem",
            "../certificates/provider-root.pem",
            "C:\\certificates\\provider-root.pem",
        ] {
            let out = apply_prefs_edits("", &[(super::EDIT_TITLE_SUMMARY_CA_FILE, set(path))])
                .unwrap_or_else(|error| panic!("valid CA path {path:?} was rejected: {error}"));
            let cfg: Config = toml::from_str(&out).expect("CA-path TOML re-parses");
            assert_eq!(cfg.title_summary_ca_file.as_deref(), Some(path));
        }
    }

    #[test]
    fn smart_title_endpoint_rejects_secrets_and_malformed_urls_without_disclosure() {
        for pasted in [
            "https://models.example.test/v1/chat?api_key=pasted-value",
            "https://models.example.test/v1/chat?api-version=2026-01-01",
            "https://models.example.test/v1/chat#pasted-token",
            "https://alice:p4ssw0rd@models.example.test/v1/chat",
            "models.example.test/v1/chat",
            "ftp://models.example.test/v1/chat",
            "https://models example.test/v1/chat",
            "https://models.example.test:not-a-port/v1/chat",
            "http://127.0.0.1:0/api/chat",
            "https://-/v1/chat",
            "https://models..example.test/v1/chat",
            "https://127.1:9443/v1/chat",
            "https://2130706433:9443/v1/chat",
            "https://0x7f000001:9443/v1/chat",
            concat!(
                "https://models.example.test/v1/",
                "sk-",
                "proj-abcdefghijklmnopqrstuvwxyz012345/chat"
            ),
            concat!(
                "https://",
                "sk-",
                "proj-abcdefghijklmnopqrstuvwxyz012345.models.example/v1/chat"
            ),
            "https://models.example.test/v1/%2Fchat",
            "https://models.example.test/v1/%ZZ/chat",
            "https://models.example.test\\v1\\chat",
            "https://models.example.test/v1/{bad}/chat",
            "https://models.example.test/v1/秘密/chat",
        ] {
            let err = apply_prefs_edits(
                "title_summary_provider = \"openai-compatible\"\n",
                &[(super::EDIT_TITLE_SUMMARY_ENDPOINT, set(pasted))],
            )
            .expect_err("endpoint parameters must not enter persistent config");
            let diagnostic = err.to_string();
            assert!(
                !diagnostic.contains(pasted),
                "endpoint leaked: {diagnostic}"
            );
            assert!(diagnostic.contains("without a query or fragment"));
            assert!(matches!(
                err,
                PrefsEditError::SensitiveBadValue { key, .. }
                    if key == super::EDIT_TITLE_SUMMARY_ENDPOINT
            ));
        }

        let endpoint = "https://models.example.test/v1/chat/completions";
        let out = apply_prefs_edits(
            "title_summary_provider = \"openai-compatible\"\n",
            &[(super::EDIT_TITLE_SUMMARY_ENDPOINT, set(endpoint))],
        )
        .expect("parameter-free endpoint remains valid");
        let cfg: Config = toml::from_str(&out).expect("endpoint TOML re-parses");
        assert_eq!(cfg.title_summary_endpoint.as_deref(), Some(endpoint));
    }

    /// Provider/proxy/title-format controls reject unknown values and canonicalize case;
    /// timeout/cadence/context sliders enforce the same bounded domains as config.
    #[test]
    fn smart_title_enums_and_ranges_are_validated() {
        for (key, options) in [
            (
                super::EDIT_TITLE_SUMMARY_PROVIDER,
                super::TITLE_SUMMARY_PROVIDERS,
            ),
            (
                super::EDIT_TITLE_SUMMARY_PROXY_MODE,
                super::TITLE_SUMMARY_PROXY_MODES,
            ),
            (super::EDIT_TAB_TITLE_FORMAT, super::TITLE_FORMATS),
            (super::EDIT_WINDOW_TITLE_FORMAT, super::TITLE_FORMATS),
        ] {
            match super::edit_kind(key) {
                EditKind::Enum { options: actual } => assert_eq!(actual, options),
                other => panic!("{key} should be Enum, got {other:?}"),
            }
            let canonical = options[0];
            let out = apply_prefs_edits("", &[(key, set(&canonical.to_ascii_uppercase()))])
                .expect("case-insensitive canonical enum");
            assert!(out.contains(&format!("{key} = \"{canonical}\"")), "{out}");
            assert!(matches!(
                apply_prefs_edits("", &[(key, set("not-a-real-option"))]),
                Err(PrefsEditError::BadValue { .. })
            ));
        }

        assert_eq!(
            range_of(super::EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS),
            Some(super::Range {
                min: 1.0,
                max: 120.0,
                step: 1.0,
            })
        );
        assert_eq!(
            range_of(super::EDIT_TITLE_SUMMARY_INTERVAL_SECONDS),
            Some(super::Range {
                min: 5.0,
                max: 300.0,
                step: 1.0,
            })
        );
        assert_eq!(
            range_of(super::EDIT_TITLE_SUMMARY_CONTEXT_LINES),
            Some(super::Range {
                min: 4.0,
                max: 80.0,
                step: 1.0,
            })
        );
        for (key, value) in [
            (super::EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS, "0"),
            (super::EDIT_TITLE_SUMMARY_TIMEOUT_SECONDS, "121"),
            (super::EDIT_TITLE_SUMMARY_INTERVAL_SECONDS, "4"),
            (super::EDIT_TITLE_SUMMARY_INTERVAL_SECONDS, "301"),
            (super::EDIT_TITLE_SUMMARY_CONTEXT_LINES, "3"),
            (super::EDIT_TITLE_SUMMARY_CONTEXT_LINES, "81"),
        ] {
            assert!(
                matches!(
                    apply_prefs_edits("", &[(key, set(value))]),
                    Err(PrefsEditError::BadValue { .. })
                ),
                "{key}={value} must be rejected"
            );
        }
    }

    /// `editable_fields` seeds CONFIGURED values only (unset = blank), in the documented
    /// row order, with the right keys + kinds — what the window maps to controls.
    #[test]
    fn editable_fields_seed_from_config() {
        let c: Config = toml::from_str(
            "font_px = 13.0\ntheme = \"Nord\"\nscrollback_lines = 4000\ncopy_on_select = true\n",
        )
        .unwrap();
        let fields = editable_fields(&c);
        let keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
        // No duplicate keys — each control edits a distinct Config key (robust to schema
        // growth, unlike a brittle exact-vector match).
        let mut uniq: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for k in &keys {
            assert!(uniq.insert(k), "duplicate key {k} in editable_fields");
        }
        // Rows are GROUPED by section — the section order index is non-decreasing down
        // the list (so the painter can header each section). Robust to adding controls.
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
        // The bool seeds its RESOLVED state so the checkbox starts in the right spot.
        assert_eq!(seed(EDIT_COPY_ON_SELECT).as_deref(), Some("true"));
        // Ligatures defaults ON, so an unset config seeds the checkbox "true".
        assert_eq!(seed(EDIT_LIGATURES).as_deref(), Some("true"));
        // Unset keys seed None (blank control) — NOT the effective default.
        assert_eq!(seed(EDIT_FONT_FAMILY), None);
        assert_eq!(seed(EDIT_CURSOR_STYLE), None);
    }

    /// On a fully-unset config every text field seeds blank and each bool seeds its
    /// effective default — so an unchanged Save (all-blank, checkboxes at their
    /// defaults) writes/removes nothing the round-trip can't represent.
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
        // Bools seed their RESOLVED platform default: ligatures are ON everywhere;
        // copy_on_select is ON off Linux and OFF on Linux, where a selection owns
        // the X11 PRIMARY buffer and the CLIPBOARD stays for explicit copies (see
        // `Config::copy_on_select_or_default`).
        assert_eq!(
            seed(EDIT_COPY_ON_SELECT).as_deref(),
            Some(if cfg!(target_os = "linux") {
                "false"
            } else {
                "true"
            })
        );
        assert_eq!(seed(EDIT_LIGATURES).as_deref(), Some("true"));
    }

    /// Every numeric control must be DELIBERATELY classified as either a bounded slider
    /// (`range_of` is `Some`) or an open-ended numeric field (a known unbounded key),
    /// and non-numeric controls never get a range — the slider/field split is exhaustive.
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
    }

    /// A few representative controls expose intent keywords so fuzzy search can find
    /// them by synonym (the corpus the settings search bar ranks against).
    #[test]
    fn keywords_surface_intent_synonyms() {
        assert!(keywords_of(EDIT_WINDOW_THEME).contains(&"dark"));
        assert!(keywords_of(EDIT_SCROLLBACK).contains(&"history"));
        assert!(keywords_of(EDIT_COPY_ON_SELECT).contains(&"clipboard"));
        assert!(keywords_of(EDIT_ALLOW_WINDOW_OPS).contains(&"security"));
        assert!(keywords_of(EDIT_FONT_PX).contains(&"size"));
    }

    /// A whitespace-only configured string seeds BLANK (None), matching the display
    /// fallback — so re-saving an untouched window doesn't materialise a "   " value.
    #[test]
    fn editable_fields_blank_string_seeds_none() {
        let c: Config = toml::from_str("theme = \"   \"\nfont_family = \"\"\n").unwrap();
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

    fn placeholder(fields: &[super::EditField], k: &str) -> String {
        fields
            .iter()
            .find(|f| f.key == k)
            .map(|f| f.placeholder.clone())
            .unwrap_or_default()
    }

    /// On a fully-unset config every text field's placeholder shows the EFFECTIVE
    /// DEFAULT (never blank) — the fix for the "every row looks empty" confusion. The
    /// control still SEEDS blank (so an untouched Save removes nothing), but the user
    /// sees what's in effect.
    #[test]
    fn editable_fields_placeholder_shows_effective_default() {
        let fields = editable_fields(&Config::default());
        assert_eq!(placeholder(&fields, EDIT_FONT_PX), "auto (default)");
        assert_eq!(placeholder(&fields, EDIT_FONT_FAMILY), "default");
        assert_eq!(placeholder(&fields, EDIT_THEME), "Default");
        assert_eq!(placeholder(&fields, EDIT_CURSOR_STYLE), "block (default)");
        assert_eq!(placeholder(&fields, EDIT_SCROLLBACK), "100000 (default)");
        assert_eq!(
            placeholder(&fields, EDIT_MOTION),
            super::motion_auto_placeholder()
        );
        // None of the text-row placeholders are blank.
        for k in [
            EDIT_FONT_PX,
            EDIT_FONT_FAMILY,
            EDIT_THEME,
            EDIT_CURSOR_STYLE,
            EDIT_SCROLLBACK,
        ] {
            assert!(!placeholder(&fields, k).is_empty(), "{k} placeholder blank");
        }
    }

    #[test]
    fn motion_auto_copy_is_truthful_for_each_platform_capability() {
        let (mac_placeholder, mac_help) = super::motion_auto_copy("macos");
        assert!(mac_placeholder.contains("live macOS"));
        assert!(mac_help.contains("live"));

        let (windows_placeholder, windows_help) = super::motion_auto_copy("windows");
        assert!(windows_placeholder.contains("window attach"));
        assert!(windows_help.contains("not observed live"));

        for target in ["linux", "freebsd"] {
            let (placeholder, help) = super::motion_auto_copy(target);
            assert!(placeholder.contains("unavailable"));
            assert!(help.contains("choose reduced explicitly"));
            assert!(!placeholder.contains("follow OS"));
        }
    }

    /// A CONFIGURED value is surfaced verbatim in the placeholder too (so the effective
    /// value shows whether or not the control is pre-filled), and `0` scrollback reads as
    /// "unlimited".
    #[test]
    fn editable_fields_placeholder_reflects_configured_value() {
        let c: Config = toml::from_str(
            "theme = \"Nord\"\ncursor_style = \"bar\"\nscrollback_lines = 0\nfont_px = 15.0\n",
        )
        .unwrap();
        let fields = editable_fields(&c);
        assert_eq!(placeholder(&fields, EDIT_THEME), "Nord");
        assert_eq!(placeholder(&fields, EDIT_CURSOR_STYLE), "bar");
        assert_eq!(placeholder(&fields, EDIT_SCROLLBACK), "unlimited");
        assert_eq!(placeholder(&fields, EDIT_FONT_PX), "15 px");
    }
}

/// FULL-COVERAGE conformance: the proofs that the settings registry spans the
/// ENTIRE `Config` surface (the AI-driveability contract behind Manual and
/// `settings set|unset`) — an exhaustiveness gate over the serde fields, the
/// dotted-key nested-table writer, and the list-row round-trips.
#[cfg(test)]
mod registry_conformance_tests {
    use super::{
        Config, DEFERRED_CONFIG_KEYS, EditKind, NESTED_LEAVES, PrefsEditError, apply_prefs_edits,
        edit_kind, editable_fields, integer_domain, range_of, section_of,
    };

    fn set(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    /// THE EXHAUSTIVENESS GATE: every serde field of `Config` (parsed straight
    /// out of the struct's source, so a freshly-added field is seen the moment
    /// it exists) is either registered in `editable_fields` — directly, or via
    /// at least one dotted-leaf row for a nested table — or sits on the
    /// [`DEFERRED_CONFIG_KEYS`] allowlist WITH its written rationale. A new
    /// `Config` field therefore fails CI until it is deliberately exposed or
    /// deliberately deferred; it can never silently skip the introspection
    /// surface.
    #[test]
    fn every_config_field_is_registered_or_deferred() {
        let src = include_str!("app_config.rs");
        let head = "pub(crate) struct Config {";
        let start = src
            .find(head)
            .expect("Config struct present in app_config.rs");
        let body = &src[start + head.len()..];
        let body = &body[..body.find("\n}").expect("Config struct closes")];
        let mut serde_keys: Vec<&str> = Vec::new();
        for line in body.lines() {
            let t = line.trim();
            // Every Config field is a `pub(crate) name: Option<...>` line; doc
            // comments and attributes don't match the prefix.
            if let Some(rest) = t.strip_prefix("pub(crate) ")
                && let Some((name, ty)) = rest.split_once(':')
                && ty.contains("Option<")
            {
                serde_keys.push(name.trim());
            }
        }
        assert!(
            serde_keys.len() >= 80,
            "sanity: the source parse found the Config fields (got {})",
            serde_keys.len()
        );
        let fields = editable_fields(&Config::default());
        let registered: std::collections::HashSet<&str> = fields.iter().map(|f| f.key).collect();
        for key in serde_keys {
            let direct = registered.contains(key);
            let via_dotted = fields.iter().any(|f| {
                f.key.len() > key.len()
                    && f.key.starts_with(key)
                    && f.key.as_bytes()[key.len()] == b'.'
            });
            let deferred = DEFERRED_CONFIG_KEYS.iter().any(|&(k, _)| k == key);
            assert!(
                direct || via_dotted || deferred,
                "Config field {key:?} is neither registered in editable_fields (directly or via \
                 dotted-key leaves) nor on DEFERRED_CONFIG_KEYS — expose it or defer it with a \
                 rationale"
            );
            if deferred {
                assert!(
                    !direct && !via_dotted,
                    "{key:?} is deferred AND registered — remove it from one list"
                );
            }
        }
        // The allowlist carries a real rationale for every entry.
        for &(key, why) in DEFERRED_CONFIG_KEYS {
            assert!(
                !why.trim().is_empty(),
                "{key:?} deferred without a rationale"
            );
        }
    }

    /// Every registered nested leaf has a settings row whose kind agrees with
    /// the registry AND `edit_kind` (the writer's typing), follows the seeding
    /// law (Bool ⇒ resolved seed; everything else ⇒ effective-default
    /// placeholder — which also proves `nested_seed_placeholder` has an arm per
    /// leaf, since the fallback yields a blank pair), sits in a real
    /// section/group, and WRITES a value serde re-parses — the full read+write
    /// introspection contract, per leaf.
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
            // Appearance, net is Security, update is Terminal BY ARM; asserting
            // group != General proves the group arm too).
            let (group, order) = super::group_of(leaf.key);
            assert_ne!(group, "General", "{} fell to the catch-all group", leaf.key);
            assert!(order < u8::MAX, "{} group order unset", leaf.key);
            let _ = section_of(leaf.key); // total (never panics) — routing pinned above via group
            // Write through the REAL editor; the result must re-parse as Config
            // (the reload contract — a mis-typed leaf would corrupt the file).
            let sample = match leaf.kind {
                EditKind::Bool => "true",
                EditKind::Integer => "5",
                EditKind::Float => "0.5",
                EditKind::Enum { options } => options[0],
                EditKind::Color => "#123456",
                EditKind::Text | EditKind::Theme => "sample",
            };
            let out = apply_prefs_edits("", &[(leaf.key, set(sample))])
                .unwrap_or_else(|e| panic!("{} rejected its own sample value: {e}", leaf.key));
            let _: Config = toml::from_str(&out).unwrap_or_else(|e| {
                panic!("{} wrote TOML serde rejects: {e}\n---\n{out}", leaf.key)
            });
        }
    }

    /// Every [`EditKind::Integer`] key enforces its declared writable integer
    /// domain at Save: normally the serde field's actual domain, with narrower
    /// operational-policy domains for Smart Title request controls. `-1` is rejected
    /// unless the writable domain is signed;
    /// `u64::MAX + 1` (beyond any TOML integer) is always rejected; BOTH
    /// [`integer_domain`] boundaries write files the REAL `Config` serde model
    /// re-parses; and one-past-each-boundary is rejected. So a settings write —
    /// the overlay's free-form editor or `settings set` over the control socket —
    /// can never make `aterm.toml` serde-unparseable, which the live reload would
    /// answer by silently keeping the old config and the NEXT launch by resetting
    /// the user's entire configuration to defaults.
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
        // The whole contract in one probe: a value the editor ACCEPTS must
        // re-parse through the real serde model (an accepted-but-unparseable
        // write is the corruption); a rejection must be the typed BadValue.
        let saves = |key: &str, value: i128| -> bool {
            match apply_prefs_edits("", &[(key, set(&value.to_string()))]) {
                Ok(out) => {
                    let _: Config = toml::from_str(&out).unwrap_or_else(|e| {
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
            // The declared writable boundaries save and re-parse (proves the arm is
            // not WIDER than the field type — a too-wide arm dies in the re-parse).
            assert!(saves(key, lo), "{key}: domain floor {lo} must save");
            assert!(saves(key, hi), "{key}: domain ceiling {hi} must save");
            // One past each writable boundary is refused at Save.
            if lo > i128::from(i64::MIN) {
                assert!(!saves(key, lo - 1), "{key}: {} must be a BadValue", lo - 1);
            }
            if hi < i128::from(i64::MAX) {
                assert!(!saves(key, hi + 1), "{key}: {} must be a BadValue", hi + 1);
            }
            // The review's exact probes: `-1` saves ONLY for signed fields…
            assert_eq!(
                saves(key, -1),
                lo < 0,
                "{key}: -1 acceptance must match the writable domain's signedness"
            );
            // …and an integer beyond u64::MAX (unwritable in TOML at all)
            // never saves for ANY key.
            assert!(
                !saves(key, i128::from(u64::MAX) + 1),
                "{key}: u64::MAX + 1 must be a BadValue"
            );
        }
    }

    /// Dotted-key writes CREATE nested tables on an empty file and EDIT deep
    /// leaves in existing ones — preserving the header comment, an in-table
    /// comment, an inline comment, and every sibling key (the non-destructive
    /// contract, extended below top level).
    #[test]
    fn dotted_key_writes_create_and_edit_nested_tables_preserving_comments() {
        // CREATE: three depths on an empty file, then serde reads them back.
        let out = apply_prefs_edits(
            "",
            &[
                ("net.listen", set("0.0.0.0:7100")),
                ("update.auto_apply", set("false")),
                ("sparkle_words.profanity.enabled", set("false")),
            ],
        )
        .unwrap();
        let c: Config = toml::from_str(&out).expect("created tables re-parse");
        assert_eq!(
            c.net.as_ref().and_then(|n| n.listen.as_deref()),
            Some("0.0.0.0:7100")
        );
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
[net]
# operator cert (DER)
cert = \"~/x.der\"  # keep me
[matrix_rain]
enabled = true
";
        let out = apply_prefs_edits(
            existing,
            &[
                ("net.listen", set("0.0.0.0:7100")),
                ("matrix_rain.fps", set("42")),
            ],
        )
        .unwrap();
        assert!(out.contains("# my config"), "{out}");
        assert!(out.contains("# operator cert (DER)"), "{out}");
        assert!(out.contains("# keep me"), "{out}");
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.font_px, Some(12.0));
        let net = c.net.as_ref().unwrap();
        assert_eq!(net.cert.as_deref(), Some("~/x.der"));
        assert_eq!(net.listen.as_deref(), Some("0.0.0.0:7100"));
        let mr = c.matrix_rain.as_ref().unwrap();
        assert_eq!(mr.enabled, Some(true));
        assert_eq!(mr.fps, Some(42));

        // The INLINE-table spelling (`net = { … }`) edits in place too.
        let out =
            apply_prefs_edits("net = { cert = \"a\" }\n", &[("net.listen", set("b"))]).unwrap();
        let c: Config = toml::from_str(&out).unwrap();
        let net = c.net.as_ref().unwrap();
        assert_eq!(net.cert.as_deref(), Some("a"));
        assert_eq!(net.listen.as_deref(), Some("b"));
    }

    /// Dotted-key REMOVAL reverts exactly the leaf (siblings + comments stay),
    /// removing through a missing table is a clean no-op, and a NON-table
    /// intermediate refuses the edit (Parse error, file untouched) instead of
    /// clobbering user data.
    #[test]
    fn dotted_key_removal_and_non_table_refusal() {
        let existing = "[sparkle_words]\n# note\nenabled = false\nreduced_motion = true\n";
        let out = apply_prefs_edits(existing, &[("sparkle_words.reduced_motion", None)]).unwrap();
        assert!(!out.contains("reduced_motion"), "{out}");
        assert!(out.contains("# note"), "{out}");
        let c: Config = toml::from_str(&out).unwrap();
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
            apply_prefs_edits(untouched, &[("net.listen", None)]).unwrap(),
            untouched
        );
        // A scalar squatting on the table name refuses the write (never doc[..] panics).
        let err = apply_prefs_edits("net = 5\n", &[("net.listen", set("x"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::Parse(_)), "{err}");
        // And a scalar write over an existing TABLE leaf is refused too.
        let err =
            apply_prefs_edits("[net.listen]\nx = 1\n", &[("net.listen", set("y"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::Parse(_)), "{err}");
    }

    /// LIST rows (`palette`/`shell_args`/`cursor_trail_packs`/`font_features`):
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
                (super::EDIT_CURSOR_TRAIL_PACKS, set("~/a.toml, ~/b.toml")),
                (super::EDIT_FONT_FEATURES, set("ss01, zero, -calt")),
            ],
        )
        .unwrap();
        let c: Config = toml::from_str(&out).expect("arrays re-parse as Vec<String>");
        assert_eq!(
            c.palette,
            Some(vec!["#1d1f21".to_string(), "#cc6666".to_string()])
        );
        assert_eq!(c.shell_args, Some(vec!["-l".to_string(), "-i".to_string()]));
        assert_eq!(
            c.cursor_trail_packs,
            Some(vec!["~/a.toml".to_string(), "~/b.toml".to_string()])
        );
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

    /// The full-coverage TOP-LEVEL batch: the corruption-critical Bool arms, the
    /// numeric/enum/colour typings, and a serde round-trip through the real
    /// editor — including the two NEW padding keys and the enum domains whose
    /// canonical spellings must parse in their consumers.
    #[test]
    fn full_coverage_scalar_keys_classify_and_round_trip() {
        use crate::app_config::{BackgroundMaterial, WindowColorspace};
        for k in [
            super::EDIT_GPU,
            super::EDIT_CURSOR_TRAIL_RING,
            super::EDIT_CURSOR_TRAIL_BLOOM,
            super::EDIT_CURSOR_FIRE_SHIMMER,
            super::EDIT_HDR_GLOW,
            super::EDIT_STREAM_FADE,
            super::EDIT_RESTORE_SESSION,
            super::EDIT_TEMPORAL_RECORDING,
            super::EDIT_TRAIL_SOUND_BED,
            super::EDIT_NOTICE_SPARKLE,
            super::EDIT_PKG_PROGRESS_EFFECTS,
        ] {
            assert!(
                matches!(edit_kind(k), EditKind::Bool),
                "{k} is Bool (else Save writes a string for a serde bool)"
            );
        }
        // Every canonical enum spelling is accepted by its consuming parser —
        // the two lists can never drift (the CURSOR_TRAIL_STYLES pattern).
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
                (super::EDIT_STREAM_FADE_MS, set("120")),
                (super::EDIT_WINDOW_COLORSPACE, set("Display-P3")),
                (super::EDIT_BACKGROUND_MATERIAL, set("under-window")),
                (super::EDIT_CURSOR_TRAIL_COLOR, set("#50FA7B")),
                (super::EDIT_SHELL, set("bash")),
                (super::EDIT_WINDOW_PADDING, set("20")),
                (super::EDIT_WINDOW_PADDING_TOP, set("6")),
            ],
        )
        .unwrap();
        let c: Config = toml::from_str(&out).expect("typed values round-trip");
        assert_eq!(c.gpu, Some(false));
        // The ambient-bed opt-in writes a real TOML bool and resolves ON.
        assert_eq!(c.trail_sound_bed, Some(true));
        assert!(c.trail_sound_bed_or_default());
        assert_eq!(c.trail_sound_volume, Some(0.2));
        assert_eq!(c.cursor_trail_length, Some(48));
        assert_eq!(c.stream_fade_ms, Some(120));
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

    /// FONT-DISPLAY: the registry's `display_font` option list is EXACTLY the
    /// bundled face registry (ids and order), so the settings toggles, the Enum
    /// validation, and the `display:` resolver can never disagree; and the two
    /// keys round-trip through the real TOML writer with their typed validation
    /// (a bad face id / a non-hex tab color is rejected, never written).
    #[test]
    fn display_font_and_tab_color_registry_rows_are_wired() {
        let bundled: Vec<&str> = aterm_render::DISPLAY_FACES.iter().map(|f| f.id).collect();
        assert_eq!(
            super::DISPLAY_FONT_OPTIONS.first(),
            Some(&"off"),
            "the popup's explicit off state leads the option list"
        );
        assert_eq!(
            &super::DISPLAY_FONT_OPTIONS[1..],
            bundled.as_slice(),
            "DISPLAY_FONT_OPTIONS' id tail must mirror aterm_render::DISPLAY_FACES"
        );
        // "off" behaves exactly like an unset key in the resolver.
        let off = Config {
            display_font: Some("off".to_string()),
            font_family: Some("Menlo".to_string()),
            ..Config::default()
        };
        assert_eq!(off.font_family_request().as_deref(), Some("Menlo"));
        let set = |v: &str| Some(v.to_string());
        let out = apply_prefs_edits(
            "",
            &[
                (super::EDIT_DISPLAY_FONT, set("pixel")),
                (super::EDIT_ACTIVE_TAB_COLOR, set("#FF00AA")),
            ],
        )
        .unwrap();
        let c: Config = toml::from_str(&out).unwrap();
        assert_eq!(c.display_font.as_deref(), Some("pixel"));
        assert_eq!(c.active_tab_color.as_deref(), Some("#FF00AA"));
        assert_eq!(c.active_tab_color_rgb(), Some([0xFF, 0x00, 0xAA]));
        assert_eq!(c.font_family_request().as_deref(), Some("display:pixel"));
        // A MIX round-trips through the writer in canonical joined form, and
        // the resolver canonicalizes it into the `display:` mix family.
        let out = apply_prefs_edits("", &[(super::EDIT_DISPLAY_FONT, set(" pixel + engraved "))])
            .unwrap();
        let mixed: Config = toml::from_str(&out).unwrap();
        assert_eq!(mixed.display_font.as_deref(), Some("pixel+engraved"));
        assert_eq!(
            mixed.font_family_request().as_deref(),
            Some("display:pixel+engraved")
        );
        // Unknown face id / bad mixes / non-hex color: rejected by the writer.
        for bad in [
            "doom",
            "pixel+doom",
            "engraved+engraved",
            "chunky+pixel+engraved+bubble",
            // A legacy alias may not smuggle a duplicate past the distinctness
            // check by wearing its old name.
            "pixel+minecraft",
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
        // Clearing the display face restores the plain family passthrough.
        let mut cleared = c.clone();
        cleared.display_font = None;
        assert_eq!(cleared.font_family_request(), cleared.font_family);
        // A typo'd id that somehow reaches config falls back fail-open.
        let mut typo = c;
        typo.display_font = Some("dooom".to_string());
        assert_eq!(typo.font_family_request(), typo.font_family);
    }

    /// MIGRATION: a config written before the rename keeps working, and keeps
    /// working the SAME way. This is the promise the deprecated spellings exist
    /// to keep — deleting a shipped key or id would turn a file that was valid
    /// yesterday into a complaint about a line the user typed correctly.
    ///
    /// The one id with no successor (`mariokart`, whose face carried no
    /// redistribution grant and had no substitute) falls back to the primary
    /// font instead of erroring, for the same reason.
    #[test]
    fn the_pre_rename_key_and_ids_still_load_and_resolve() {
        // The accepted-alias list is the renderer's retirement table, exactly —
        // a face retired in one place and forgotten in the other is how a
        // still-valid config becomes an error.
        let from_registry: Vec<&str> = aterm_render::DISPLAY_FACE_LEGACY_IDS
            .iter()
            .map(|(legacy, _)| *legacy)
            .collect();
        assert_eq!(
            super::LEGACY_DISPLAY_FONT_IDS,
            from_registry.as_slice(),
            "LEGACY_DISPLAY_FONT_IDS must mirror aterm_render::DISPLAY_FACE_LEGACY_IDS"
        );
        for id in super::LEGACY_DISPLAY_FONT_IDS {
            assert!(
                super::display_font_id_is_accepted(id),
                "{id} must still be accepted"
            );
            assert!(
                !super::DISPLAY_FONT_OPTIONS.contains(id),
                "{id} must not be OFFERED again by Settings"
            );
        }

        // The legacy KEY parses into the current field (serde alias).
        let legacy: Config = toml::from_str("game_font = \"minecraft\"\n").unwrap();
        assert_eq!(legacy.display_font.as_deref(), Some("minecraft"));
        // …and resolves to the RENAMED face, under the renamed scheme.
        assert_eq!(
            legacy.font_family_request().as_deref(),
            Some("display:pixel")
        );
        for (old, new) in [
            ("roblox", "display:chunky"),
            ("minecraft", "display:pixel"),
            ("zelda", "display:engraved"),
            ("animal-crossing", "display:bubble"),
        ] {
            let cfg = Config {
                display_font: Some(old.to_string()),
                ..Config::default()
            };
            assert_eq!(
                cfg.font_family_request().as_deref(),
                Some(new),
                "{old} must resolve to {new}"
            );
        }
        // The deleted face: falls back to the primary font, never an error.
        let gone = Config {
            display_font: Some("mariokart".to_string()),
            font_family: Some("Menlo".to_string()),
            ..Config::default()
        };
        assert_eq!(gone.font_family_request().as_deref(), Some("Menlo"));
        // A legacy MIX canonicalizes to current ids, and mixing a face with its
        // own old name is still one face twice — rejected, not silently doubled.
        let mixed = Config {
            display_font: Some("minecraft+zelda".to_string()),
            ..Config::default()
        };
        assert_eq!(
            mixed.font_family_request().as_deref(),
            Some("display:pixel+engraved")
        );
        let doubled = Config {
            display_font: Some("pixel+minecraft".to_string()),
            font_family: Some("Menlo".to_string()),
            ..Config::default()
        };
        assert_eq!(
            doubled.font_family_request().as_deref(),
            Some("display:pixel"),
            "the duplicate is dropped, not counted twice"
        );
    }
}
