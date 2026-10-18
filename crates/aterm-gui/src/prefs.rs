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
//!   * [`apply_prefs_edits`] / [`save_prefs_snapshot_observed`] — write edited values back
//!     NON-DESTRUCTIVELY (preserving the user's other keys, comments, and formatting
//!     via `aterm-toml`; atomic temp-write + rename). The serialized native config
//!     worker returns the exact committed bytes and post-publication proof for
//!     direct admission; the watcher remains an independent external-edit source.
//!
//! Clearing a field to blank REMOVES that key (reverting it to its built-in default)
//! rather than writing an empty string. Save is best-effort: a missing config file is
//! created, an unwritable one is logged, never a panic.

use crate::app_config::{
    Config, DEFAULT_TAB_STATUS_DWELL_MS, DEFAULT_TAB_STATUS_QUIET_AFTER_MS,
    MAX_TAB_STATUS_DWELL_MS, MAX_TAB_STATUS_QUIET_AFTER_MS, MIN_TAB_STATUS_DWELL_MS,
    MIN_TAB_STATUS_QUIET_AFTER_MS,
};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::trail_sound::SoundVoice;
use aterm_phase::Program;

/// The TOML keys the settings model edits, paired with how each should be TYPED
/// when written back ([`apply_prefs_edits`]). The order matches the on-screen row order
/// (see [`editable_fields`]). These are the exact `Config` field names so a Save
/// followed by a reload round-trips through serde (see [`crate::app_config::Config`]).
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
/// The cursor-trail master (`Config::cursor_trail`). Default
/// [`crate::app_config::DEFAULT_DECORATIVE_EFFECTS`]: an opt-OUT everywhere except
/// Windows, where the resident walking cat the default style rides makes it an
/// opt-IN. Settings seeds this row from the RESOLVED config, so the row and every
/// "Inactive: cursor effects off" disclosure below it are platform-correct for free —
/// never re-derive the default from a typed `true`.
pub(crate) const EDIT_CURSOR_TRAIL: &str = "cursor_trail";
pub(crate) const EDIT_CURSOR_TRAIL_STYLE: &str = "cursor_trail_style";
pub(crate) const EDIT_TRAIL_SOUNDS: &str = "trail_sounds";
/// Tone-melody toggle (`Config::tone_melody`, default ON): the trail-sound
/// melody leans with the typed line's inferred mood (the tiny on-device
/// `aterm_effects::tone` classifier); OFF pins today's neutral melody and
/// stops the classifier entirely.
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
/// THE SUPERVISOR'S CHOICE CHIME (the question-policy contract, 2026-09-24):
/// one quiet synth pip when the in-window supervisor answers Claude Code's
/// question box by policy (`App::chose_chime`). A synth voice like the rest of
/// the Sound box — subordinate to Music effects and scaled by the volume —
/// default ON. Muting it silences the chime only; the band's `◆ chose` flash
/// and the window's rim pulse stay. Parsed and preserved everywhere, inert
/// where the synth has no output.
pub(crate) const EDIT_CHOICE_SOUND: &str = "choice_sound";
/// The curse BONK's two gates, named so the Sound menu, the section router and
/// the visibility allowlist all spell them once. They are `[sparkle_words]`
/// leaves in the FILE (that is where the feature's table lives and the spelling
/// must never change), but they are SFX in the UI.
pub(crate) const EDIT_SPARKLE_BONK: &str = "sparkle_words.profanity.bonk";
pub(crate) const EDIT_SPARKLE_BONK_DETONATION: &str = "sparkle_words.profanity.bonk_detonation";
/// PRISM WAKE's pip (`[output_streak] sound`, default ON —
/// `docs/DESIGN-output-streak-2026-08-30.md` §7's "sound opt-out ladder").
///
/// The output streak answers fresh PROGRAM output with a small per-theme comet
/// and, once per episode, one soft pentatonic pip plus a closing exhale. The
/// visual half and the aural half are ONE feature by the owner's 2026-08-31
/// ruling, so both ship ON; this key is the ear's own opt-out, the way
/// [`EDIT_TRAIL_SOUND_RIFF`] is the riff's — turning it off leaves every comet
/// flying and every other voice at its configured level.
///
/// A `[output_streak]` LEAF in the file (that is where the feature's table
/// lives, and a config key has no alias seam — renaming it would orphan every
/// line already authored), but SFX in the UI, exactly like the two bonk keys
/// above it. [`section_of`] and [`group_of`] both consult [`SOUND_MENU_KEYS`]
/// ahead of their nested-table prefix rules, so this leaf routes to the Sound
/// box on that membership alone and needs no `output_streak.` prefix arm.
///
/// SUBORDINATE, and only ever subtractive: `trail_sounds` × `trail_sound_volume`
/// × raw window focus × `SeriousEffect::TerminalSound` all apply first, and a
/// pip is recorded only on an edge that also minted light — so with
/// `output_streak.enabled` off this key cannot make a sound at all
/// (`Config::output_streak_warning` says so at `--validate-config` time).
pub(crate) const EDIT_OUTPUT_STREAK_SOUND: &str = "output_streak.sound";
/// PRISM WAKE's own switch (`[output_streak] enabled`): the comet over new
/// program output. Its absent-key default is platform-split
/// ([`crate::app_config::DEFAULT_DECORATIVE_EFFECTS`]), and Settings seeds the
/// row from the resolver that owns it. Appearance's Effects box, beside Matrix
/// rain; the pip's row names it while it is Off.
pub(crate) const EDIT_OUTPUT_STREAK_ENABLED: &str = "output_streak.enabled";

/// PRESENCE (round 19, SPEC19 §9): the two `[presence]` leaves the View menu's
/// checkables write — the band row under the tab bar and the colour rim. Both
/// default ON (`Config::presence_band_enabled` / `presence_rim_enabled`); the
/// View menu flips the live bit at the click and persists it through the one
/// config lane by these keys (`App::queue_presence_write`).
pub(crate) const EDIT_PRESENCE_BAND: &str = "presence.band";
pub(crate) const EDIT_PRESENCE_RIM: &str = "presence.rim";

/// THE SOUND MENU (owner ask: "add the volume and SFX menu to settings").
///
/// Every config key that changes what aterm SOUNDS like, and nothing else. One
/// list drives [`section_of`] (they all land on the Sound page), [`group_of`]
/// (the synth voices in one box, the bell in its own), and the native Advanced
/// audit, so a new audible knob cannot be added to one and forgotten in the
/// others — the exact drift that left `tone_melody`, `trail_sound_bed` and both
/// bonk keys reachable only by hand-editing `aterm.toml`.
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
    EDIT_CHOICE_SOUND,
    EDIT_SPARKLE_BONK,
    EDIT_SPARKLE_BONK_DETONATION,
    EDIT_OUTPUT_STREAK_SOUND,
];
/// The tuning a cursor style can read. The Cursor Styles page shows the ones
/// the selected style uses (`native_settings::trail_style_uses`); HDR glow and
/// SDR boost stay Manual-only, since what they do depends on the display.
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
// W5 typography/appearance knobs.
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
/// M4 — admit Cascadia N:1 MERGED ligatures (`Config::merged_ligatures`), a Bool,
/// default OFF. Registered so `edit_kind` types it correctly (a serde
/// `Option<bool>` written as a TOML string would corrupt the config on reload).
pub(crate) const EDIT_MERGED_LIGATURES: &str = "merged_ligatures";
pub(crate) const EDIT_BOLD_IS_BRIGHT: &str = "bold_is_bright";
/// Where slots 16-255 come from (`Config::palette_generate`).
pub(crate) const EDIT_PALETTE_GENERATE: &str = "palette_generate";
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
/// Whether aterm posts its OWN notices as desktop notifications
/// (`Config::desktop_alerts`, default OFF — owner, 2026-09-28): the herald's
/// escalations, the consent path's and the update-health
/// banner. An ordinary Settings ▸ Security row, in its own "Notifications"
/// box beside the Permissions box that holds `allow_notifications` (the
/// PROGRAMS' notifications) — the two notification switches on one page.
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

/// The PHOSPHOR matrix-rain master switch — the `[matrix_rain]` table's `enabled`
/// key. The Settings ROW comes from its [`NESTED_LEAVES`] registry entry
/// (Appearance ▸ Matrix rain, Bool, default OFF); this named constant exists for
/// the code that addresses the key directly — the semantic-snapshot dotted
/// projection and the tests (including their `raw_bool_value` fixture
/// projection). Runtime per-session overrides (View ▸ Matrix Rain, `aterm-ctl
/// rain`) win over this durable bit until the session ends.
pub(crate) const EDIT_MATRIX_RAIN_ENABLED: &str = "matrix_rain.enabled";

/// The `[packages]` toolchain-manager switches a person has, addressed as dotted keys
/// (the same nested writer as [`EDIT_MATRIX_RAIN_ENABLED`]). Both are Bools, both
/// default ON (2026-09-23, the one-path collapse): `enabled` is Automatic updates —
/// THE switch, which folded in the retired `auto_update` — and
/// [`EDIT_PACKAGES_AUTO_INSTALL`] is the ONE install consent, which folded in the
/// retired `seed_install`. Writing either drops its retired spelling from the file
/// ([`apply_prefs_edits`]). Housed in the search-only Packages section: the rows render
/// on the special Settings ▸ Packages page (which no `section()` registry page owns),
/// while Search and Modified still find them through the ordinary registry.
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
/// The retired `[packages] auto_update`: folded into [`EDIT_PACKAGES_ENABLED`] — a `false`
/// keeps Automatic updates off whatever `enabled` says.
pub(crate) const RETIRED_PACKAGES_AUTO_UPDATE: &str = "packages.auto_update";
/// The retired `[packages] seed_install`: read as [`EDIT_PACKAGES_AUTO_INSTALL`] while that
/// is unset.
pub(crate) const RETIRED_PACKAGES_SEED_INSTALL: &str = "packages.seed_install";
/// `[update] enabled` — "Check for aterm updates automatically" (Settings ▸ Software
/// Update's Automatic updates card, Search and Modified), default ON. It
/// replaces `$ATERM_NO_AUTO_UPDATE` (2026-09-23): off stops the background checker only
/// (Check for Updates and Update Now still work), read once per process, so it applies
/// from the next launch.
pub(crate) const EDIT_UPDATE_ENABLED: &str = "update.enabled";
/// `[reroute] announce` — whether a rerouted upstream Rust tool (`cargo`, `rustc`,
/// `tlc`) prints its signpost before it runs, default ON. The owner's 2026-09-08 ask
/// ("a printed message … that could be suppressed with a flag") made a setting on
/// 2026-09-23, replacing `$ATERM_REROUTE_QUIET`; the co-located atpkg reads it.
pub(crate) const EDIT_REROUTE_ANNOUNCE: &str = "reroute.announce";
/// The aterm WRAPPER's durable master switch — the `[harness]` table's
/// `enabled` key (design `docs/DESIGN-aterm-wrapper-2026-09-17.md` §4.6.2).
/// A Bool, default ON, registered through [`NESTED_LEAVES`] like the other
/// dotted leaves.
///
/// It is in `aterm.toml` on purpose: *"the kill switch may never depend on the
/// thing it kills"*, so it keeps working when the harness tree is missing,
/// broken or mid-update. Every finer key sits beside it in the same table
/// (`aterm_agent::supervise::config::KEYS`), each at full power by default
/// and able only to LIMIT it (owner, 2026-09-24); they are written in the
/// file (Manual) and the row says which are (`HarnessPolicy::words`).
pub(crate) const EDIT_HARNESS_ENABLED: &str = "harness.enabled";
/// `[harness] autopilot` — THE DEFAULT AUTOPILOT
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §2.1): how far the harness
/// carries a session that has no Autopilot word of its own. An Enum of the
/// supervisor's own words in the chip's cycle order ([`HARNESS_AUTOPILOT_WORDS`],
/// `aterm_agent::supervise::Autopilot::ALL`), `cruise` by default (full
/// power), registered through [`NESTED_LEAVES`] beside the master switch, so
/// it is a Settings row, a Search hit and a Manual key at once.
///
/// `cruise`: every point answered and the work continued until the agent's
/// clear final statement. `land`: the work wrapped up cleanly — the land
/// instruction once, at most two nudges, then nothing typed but the land
/// answer to a question asked in prose, which the nudges' bound does not
/// cover. `off`: the harness keeps its hands off (plain Claude Code; the
/// window's host stops the session's worker). Each step down takes power
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
/// `[harness] answer_questions` — the supervisor's QUESTION ANSWER (owner
/// directive of 2026-09-25: *"the harness must not stall on escalating to
/// the user … the harness must choose the recommended option(s) and
/// continue automatically"*). A Bool, ON by default, registered through
/// [`NESTED_LEAVES`] beside the approval level, so it is a Settings row, a
/// Search hit and a Manual key at once.
///
/// WHETHER, never how: ON, a Claude Code question dialog (AskUserQuestion)
/// is ANSWERED, as [`EDIT_HARNESS_QUESTION_PRIORITY`] (the row beside it)
/// says. Under its default, `recommended`, that is the dialog's recommended
/// option — option 1 when none is marked; every recommended option of a
/// multi-select; the review tab submitted — by Enter on the focused row once
/// a person has been quiet (`answer-recommended@v1`), never its free-text
/// row, its chat row or Cancel, and a question or choice the worker asks in
/// prose gets `[harness] answer_text` (`answer@v1`); under `long-term` /
/// `short-term` no option is chosen and the policy's text is written into
/// the free-text row instead (and typed in place of `answer_text`). Either
/// way a question a person has begun answering is theirs. OFF — a limit —
/// every question goes to the menu bar, whatever the priority says.
/// Independent of [`EDIT_HARNESS_APPROVE`] both ways: a question is no
/// permission. The row seeds from the SUPERVISOR's resolved bit
/// ([`Config::harness`]), so it reads a value as the reader takes it: an off
/// spelling is OFF, and a value that is neither on nor off is no setting (ON).
pub(crate) const EDIT_HARNESS_ANSWER_QUESTIONS: &str = "harness.answer_questions";
/// `[harness] question_priority` — HOW the supervisor answers a question
/// dialog while [`EDIT_HARNESS_ANSWER_QUESTIONS`] is on (2026-09-25). An Enum,
/// default `recommended`, registered through [`NESTED_LEAVES`] beside it, so
/// it is a Settings row, a Search hit and a Manual key at once.
///
/// `recommended`: the dialog's recommended option, as above. `long-term` /
/// `short-term`: the supervisor chooses no option — it writes that policy's
/// fixed text into the question's free-text row and submits it, so the
/// worker decides by the policy and says which it chose (a preview tab, or
/// one the text would not fit, gets its recommended option). Never WHETHER:
/// with the answer switch off every question goes to the menu bar whatever
/// this row says. A session's own `aterm ctl @<sid> meta set questions …`
/// word decides over it. A refused value is no setting: the reader
/// ([`Config::harness`]) keeps `recommended` and leaves the answer switch as
/// written, and this row seeds nothing.
pub(crate) const EDIT_HARNESS_QUESTION_PRIORITY: &str = "harness.question_priority";
/// `[harness.claude] enabled` — whether Claude Code's sessions are
/// supervised, under the master switch ([`EDIT_HARNESS_ENABLED`]). A Bool, ON
/// by default; `false` puts Claude Code in the supervisor's `off` list. The
/// row seeds from that list ([`Config::harness_agent_enabled`]), never from
/// the master switch, which is its own row. The window's host enforces it
/// when saved ([`HARNESS_AGENT_TIMING`]).
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

/// `[ai] enabled` — whether the window looks at the AI CLIs aterm manages
/// and says, in Messages, which one needs a person
/// (docs/DESIGN-ai-connections-2026-10-16.md §7.3). A Bool, ON by default,
/// read by the AI layer's own reader ([`crate::app_config::AiPolicy`]); OFF,
/// no look runs and the AI lines end. The `aterm ai` verbs always work.
pub(crate) const EDIT_AI_ENABLED: &str = "ai.enabled";
/// `[ai] preferred` — this machine's main AI CLI (§6.1): the one that asks
/// when a review names none, and the doctor's first row. An Enum over every
/// connection's id ([`aterm_agent::ai::connector::IDS`]); its default is the
/// first CLI this platform has a build of. A choice, not a grant, so a
/// Settings row; a fleet document may set it too, and the row says so.
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

/// `[machine] universal_control` — `"off"` (default) disables macOS Universal Control
/// for this host whenever the `[machine]` settings are applied (`aterm pkg machine apply`,
/// run once a day at the first window or terminal session, and by a pass after an
/// edit); `"leave"` never touches it. Applied by the CO-LOCATED atpkg, which reads the same
/// table; the Security page shows the measured state beside the switch.
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

/// Values intentionally edited only in Manual: lossless collections,
/// host-backed asset sources that require off-thread preparation, and the
/// legacy feline style, which has no honest native choice surface:
/// `style = "paw"` remains a supported compatibility mode, but it is ink-only
/// in cat-art v4. (Keys that do nothing are not registry rows at all; Manual
/// types them from `native_config_language`'s compatibility metadata.)
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
    // [update] — whether the self-updater runs, and its apply policy. The channel
    // (`owner`/`repo`) and `require_team_id` are DEVELOPMENT settings since 2026-09-23:
    // a shipped build reads its compiled channel and pin, so they are not rows here.
    NestedLeaf {
        key: EDIT_UPDATE_ENABLED,
        label: "Check for aterm updates automatically",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: "update.auto_apply",
        label: "Install aterm updates automatically",
        kind: EditKind::Bool,
    },
    // [reroute] — the upstream-Rust signpost line (atpkg reads it).
    NestedLeaf {
        key: EDIT_REROUTE_ANNOUNCE,
        label: "Show a note when cargo or rustc is rerouted",
        kind: EditKind::Bool,
    },
    // [harness] — the supervisor's master switch, the first of its rows.
    // The keys with no row here only limit it and are written in Manual; the
    // row's words name each limit written, by its row where it has one
    // (`HarnessPolicy::words`, which reads this registry for them).
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
    // The question answer (owner, 2026-09-25): ON answers a question dialog,
    // and one asked in prose, as the priority row below says; OFF (a limit)
    // hands every question over. WHETHER only: the label names no way of
    // answering, which is the priority's.
    NestedLeaf {
        key: EDIT_HARNESS_ANSWER_QUESTIONS,
        label: "Answer questions",
        kind: EditKind::Bool,
    },
    // HOW a question is answered (2026-09-25): its recommended option, or the
    // worker's own choice under the long-term or short-term policy.
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
        label: "Profanity supernova chance (%)",
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
    // [matrix_rain] — every scalar knob with an effect (the `enabled` master
    // included). The v1 keys that do nothing still parse, and Manual types
    // them, but they are not rows.
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
    // comet no Settings preview can show, so they stay Manual's, as HDR glow
    // and SDR boost do; `native_config_language` lists them there.
    //
    // The pip's label is written for the SOUND MENU the row appears in, not for
    // the table it lives in — the bonk keys' law: a row two boxes away from the
    // volume slider has to say what it sounds like.
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
    // [presence] (round 19) — the two surfaces a window shows who is driving
    // it on. The View menu's checkables are their primary switch; these rows
    // keep them findable (Search / Modified) and hand-editable with the same
    // typing the writer uses.
    NestedLeaf {
        key: EDIT_PRESENCE_BAND,
        label: "Presence band",
        kind: EditKind::Bool,
    },
    NestedLeaf {
        key: EDIT_PRESENCE_RIM,
        label: "Presence rim",
        kind: EditKind::Bool,
    },
];

/// Registry lookup for a dotted nested key — `None` for top-level keys and
/// unregistered paths.
pub(crate) fn nested_leaf(key: &str) -> Option<&'static NestedLeaf> {
    NESTED_LEAVES.iter().find(|l| l.key == key)
}

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
/// smooth rainbow ribbon, trailed by the full-body cat that WALKS, runs and pounces
/// along the line instead of the flying head (the owner's own machine has run this
/// spelling for weeks, and shipping anything else made the default a stranger to the
/// product). The name has changed three times: the original single-word `nyan` became
/// the two-word `nyan rainbow`, then `rainbow kitty` (the owner's name for the ribbon —
/// it says what you SEE), and the default now names the PET companion on top of it.
/// Every historical spelling still resolves via [`CURSOR_TRAIL_STYLE_ALIASES`], so old
/// configs keep working. `rainbow kitty` and bare `kitty` now name the visible resident;
/// the historical `nyan`/`rainbow` aliases retain the earned flying head. The placeholder
/// hint for the row.
///
/// This is the SINGLE definition of the default: `app_config`, `native_settings` and
/// `settings_preview` read it rather than re-typing the literal, so the next rename is
/// one line.
pub(crate) const DEFAULT_CURSOR_TRAIL_STYLE: &str = "rainbow kitty pet";

/// Selectable `cursor_trail_style` values. Every rainbow entry shares one continuous
/// spectrum, animation clock, fade, and sparkle engine; the raw spelling chooses only
/// companion and geometry. This table is also the save-time and introspection domain.
pub(crate) const CURSOR_TRAIL_STYLES: &[&str] = &[
    "phaser",
    "rainbow kitty pet",
    "rainbow dog pet",
    "rainbow kitty flying",
    "rainbow kitty underline",
    // THE FLAT BODY (2026-09-13): the owner's A/B control against the default
    // comet body and its vivid rail (`docs/design/RAINBOW-KITTY-V2.md` §30) —
    // the same ribbon, the same resident pet, the body as it was before.
    "rainbow kitty flat",
    "comet",
    "lumen",
    "sparkle",
    "fire",
    "laser",
    "water",
    "beam",
    // THE SALVAGE — the v0.28 trail, restored as an option: a thin four-layer
    // comet under a soft square bloom, with the long diagonal jump comet the
    // modern engine's shape gates no longer mint. Offered last among the
    // effects because it is a restoration, not a new look.
    "classic",
    // …and its second face: the same salvaged engine, colouring the tracer
    // from the theme's cursor colour (and live OSC 12) instead of the rolling
    // spectrum. Both were v0.28 looks; neither is a default.
    "classic mono",
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
/// what they SOUND like whatever the trail looks like (`music box` = the
/// rainbow kitty's instrument, in the deleted glass bell's slot — §17.3
/// phase 7; `droplet` = water's plip, …); `mechanical` is the
/// keyboard; `typewriter` / `marimba` / `felt` are the sound-only
/// instruments. Alias spellings (the trail-style names, `mech`, `thock`, …)
/// resolve through [`trail_sound_style_canonical`] /
/// `Config::trail_sound_voice` — accepted on load, never offered.
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
/// `None` for a spelling the runtime would silently play as `auto` — the
/// case the validator flags and the Settings row must not mistake for a
/// custom entry. The [`cursor_trail_style_canonical`] twin, answered by the
/// synth's own parser so the UI, the validator and the engine share one
/// vocabulary.
pub(crate) fn trail_sound_style_canonical(token: &str) -> Option<&'static str> {
    SoundVoice::parse(token).map(SoundVoice::name)
}

/// Documented aliases accepted by both the runtime and Settings. Historical
/// `nyan`/`rainbow` spellings retain the flying companion; geometry aliases map
/// to their explicit picker entries so validation and rendering cannot disagree.
pub(crate) const CURSOR_TRAIL_STYLE_ALIASES: &[(&str, &str)] = &[
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
    // The bare name and the `… tall` spellings draw exactly what the default
    // draws (the tall body is the default geometry), so they are spellings of
    // it rather than entries of their own.
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

/// Resolve a `cursor_trail_style` spelling (canonical or documented alias,
/// case-insensitive, pre-trimmed by the caller) to its canonical option, or
/// `None` for a spelling nothing recognizes — the case that falls back to
/// [`DEFAULT_CURSOR_TRAIL_STYLE`], and that the validator and the load-time
/// warning must flag so the substitution is not silent.
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
    /// value that doesn't parse as a hex colour ([`aterm_types::scheme::parse_hex`]),
    /// so a malformed colour can't be saved (it would otherwise be ignored at load).
    /// The overlay opens the COLOUR WHEEL popover on these rows instead of the
    /// free-text editor (design §7); commits still funnel through this validation.
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
        | EDIT_CURSOR_TRAIL
        | EDIT_TRAIL_SOUNDS
        | EDIT_TONE_MELODY
        | EDIT_TRAIL_SOUND_BED
        | EDIT_TRAIL_SOUND_RIFF
        | EDIT_BELL_SOUND
        | EDIT_CHOICE_SOUND
        | EDIT_CURSOR_BLINK
        | EDIT_CURSOR_MOMENTUM_GLOW
        | EDIT_OPTION_AS_META
        | EDIT_CONFIRM_MULTILINE_PASTE
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
        // CRITICAL: the dotted [packages] maintenance switches must type as Bool —
        // falling to Text would write a TOML string for a serde `Option<bool>`,
        // silently corrupting a table the CO-LOCATED atpkg also parses (the
        // matrix_rain.* leaves are typed by their NESTED_LEAVES rows before this
        // match is consulted).
        | EDIT_PACKAGES_ENABLED
        | EDIT_PACKAGES_AUTO_INSTALL
        // And their RETIRED spellings: Settings removes one (a Modified row's Reset, Reset
        // All), and Undo writes it back — as the Bool atpkg's serde reads, never a string.
        | RETIRED_PACKAGES_AUTO_UPDATE
        | RETIRED_PACKAGES_SEED_INSTALL
        | EDIT_WALLPAPER_TEXT_TINT => EditKind::Bool,
        EDIT_CURSOR_TRAIL_STYLE => EditKind::Enum {
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

/// The `(dark, light)` pair `name` seeds when Follow system is turned on: the
/// name keeps the side it is drawn for (declared or measured, as the catalog
/// admitted it), so turning the mode on never changes what is drawn on that
/// side, and the other side is its catalog pair, or aterm's own theme for
/// that side (Default, Default Light) when it has none, so the switch that
/// now reads On really follows the system. A name the catalog does not know
/// stays on the dark side.
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

/// The value turning Follow system on writes: the configured name (Default
/// when none is set, so an unset theme writes `dark:Default,light:Default
/// Light`) on its own side and its pair on the other. A value already
/// following the system is returned as written.
pub(crate) fn theme_follow_system_value(
    raw: Option<&str>,
    themes: &crate::app_config::ThemeCatalog,
) -> String {
    if let Some(raw) = raw
        && theme_mode(Some(raw)) == ThemeMode::FollowSystem
    {
        return raw.to_string();
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

/// The `theme` value choosing theme `id` writes, the one rule every picker
/// that is not tied to one side shares (the command palette, Settings ▸
/// Themes' Keep and Save as, the Appearance theme picker): while the value is
/// a Follow-system split only the `side` slot changes ([`theme_with_side`])
/// and the side is returned. A split that draws one theme on both sides
/// (`dark:Aterm Day,light:Aterm Day`) is one choice, not two: a theme made for
/// the other appearance replaces it on both sides, and one made for the shown
/// side takes that side alone. A plain name, or no theme at all, is replaced
/// by `id` itself, for every appearance: choosing over an unset theme never
/// starts following the OS (that is the Follow system switch's to do). A theme
/// the catalog does not know counts as made for `side`.
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
    let one_theme_pair = raw
        .and_then(theme_pair_names)
        .is_some_and(|(dark, light)| dark.eq_ignore_ascii_case(&light));
    let made_for_other_side = themes.meta(id).is_some_and(|meta| meta.appearance != side);
    if one_theme_pair && made_for_other_side {
        (theme_split_value(id, id), Some(side))
    } else {
        (theme_with_side(raw, side, id, themes), Some(side))
    }
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
        ThemeMode, theme_always_value, theme_choice_value, theme_follow_system_value,
        theme_is_unset, theme_mode, theme_slots, theme_with_side,
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
            theme_follow_system_value(Some("Gruvbox Dark"), &themes),
            "dark:Gruvbox Dark,light:Gruvbox Light"
        );
        // A light theme keeps the light side.
        assert_eq!(
            theme_follow_system_value(Some("Gruvbox Light"), &themes),
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
                theme_follow_system_value(raw, &themes),
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
                theme_follow_system_value(Some(theme.name), &themes),
                expected
            );
        }
        assert_eq!(
            theme_follow_system_value(Some("Nord"), &themes),
            "dark:Nord,light:Default Light"
        );
        assert_eq!(
            theme_follow_system_value(Some("GitHub Light"), &themes),
            "dark:Default,light:GitHub Light"
        );
        assert_eq!(
            theme_follow_system_value(Some("Office Missing"), &themes),
            "dark:Office Missing,light:Default Light"
        );
        // An existing split is never replaced.
        assert_eq!(
            theme_follow_system_value(Some("dark:Dracula,light:Nord"), &themes),
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

    /// A pair that draws one theme on both sides is one choice: a theme made
    /// for the other appearance replaces it on both sides, one made for the
    /// shown side takes that side alone. A pair with two themes (the control)
    /// only ever changes the shown side.
    #[test]
    fn a_choice_over_a_one_theme_pair_is_one_choice() {
        let themes = builtins();
        let same = Some("dark:Aterm Day,light:Aterm Day");
        assert_eq!(
            theme_choice_value(same, Dark, "Linen", &themes),
            ("dark:Linen,light:Linen".to_string(), Some(Dark))
        );
        assert_eq!(
            theme_choice_value(same, Dark, "Hearth", &themes),
            ("dark:Hearth,light:Aterm Day".to_string(), Some(Dark))
        );
        // A name the catalog does not know counts as made for the shown side.
        assert_eq!(
            theme_choice_value(same, Light, "Harbor Day", &themes).0,
            "dark:Aterm Day,light:Harbor Day"
        );
        let two = Some("dark:Nord,light:Aterm Day");
        assert_eq!(
            theme_choice_value(two, Dark, "Linen", &themes).0,
            "dark:Linen,light:Aterm Day"
        );
        // A slot edit names its side and never takes both.
        assert_eq!(
            theme_with_side(same, Dark, "Linen", &themes),
            "dark:Linen,light:Aterm Day"
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
        let on = theme_follow_system_value(Some("Gruvbox Dark"), &themes);
        assert_eq!(theme_always_value(Some(&on), Dark, &themes), "Gruvbox Dark");
    }
}

/// An error from [`apply_prefs_edits`]: either the existing file is not valid TOML
/// (so a non-destructive edit can't be performed safely), or a supplied value does not
/// parse as the key's declared type. Both are surfaced (logged) by the caller, which
/// then leaves the file untouched rather than risk clobbering it.
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
    /// A value that parses but that the resolver would IGNORE at load: `font_px`
    /// outside `FONT_PX_MIN..=FONT_PX_MAX` (or not finite). `resolve_font_px`
    /// admits only that range and falls through to the default rather than
    /// clamping, so `settings set font_px 0` used to answer `OK saved`, persist
    /// `font_px = 0.0`, and change nothing on screen (`dims` stayed at 16.00 —
    /// measured 2026-09-22). Carries the range the reply must name.
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
///   * `Some(raw)` — SET `key` to `raw`, typed per [`edit_kind`] (`font_px` → float,
///     `scrollback_lines` → integer, `copy_on_select` → bool, the rest → string). An
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
/// `[matrix_rain]` header behind, while a table that keeps a comment stays. A
/// dotted edit REFUSES (fail-closed `Parse` error) when any step of the chain
/// is not a table (e.g. `matrix_rain = 5`): silently clobbering a
/// hand-authored scalar would destroy user data.
///
/// Every OTHER key, every comment, and the document's formatting are PRESERVED, because
/// the edit goes through `aterm-toml`'s format-preserving DOM, not a re-serialize. Only
/// the listed keys change.
///
/// Errors ([`PrefsEditError`]): the existing text isn't valid TOML, or a value doesn't
/// parse as its key's type — in both cases the caller leaves the file untouched.
pub(crate) fn apply_prefs_edits(
    existing_toml: &str,
    edits: &[(&str, Option<String>)],
) -> Result<String, PrefsEditError> {
    let mut doc = existing_toml
        .parse::<aterm_toml::edit::DocumentMut>()
        .map_err(|e| PrefsEditError::Parse(e.to_string()))?;

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
            // new key now decides, and a stale `auto_update = false` left beside a
            // freshly written `enabled = true` would still read as off (the resolvers
            // honour the retired spelling, 2026-09-23).
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

/// The TOML item a Settings row writes for `key` from `raw` (the same
/// conversion as [`apply_prefs_edits`]).
pub(crate) fn typed_toml_item(
    key: &str,
    raw: &str,
) -> Result<aterm_toml::edit::Item, PrefsEditError> {
    typed_item(key, raw)
}

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
/// serialize as `font_px = 18.0  # cozy`, never lose the annotation. (Full-line
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
    // Same-line decor survives a leaf replacement, exactly like the top-level
    // path — the dotted writer is the same non-destructive contract one table
    // down (`listen = "…" # local only` keeps its comment through an edit).
    adopt_inline_decor(table.get(leaf), &mut item);
    table.insert(leaf, item);
    Ok(())
}

/// The WRITABLE domain of an [`EditKind::Integer`] key. This is normally the exact
/// serde-representable range of the key's `Config` field type, not the resolver's
/// semantic clamp range ([`range_of`] mirrors those for sliders; an in-domain value
/// beyond a resolver clamp still saves and is clamped at load, unchanged behaviour).
/// Tab-status policy controls are the deliberate exception: their operational
/// bounds are part of the Settings/control-socket contract, so their writable domain
/// is the narrower resolver range and a saved value always equals the classifier's
/// value. [`typed_item`] rejects a write outside this domain because it would either be
/// valid TOML that serde rejects wholesale, or a value the resolver immediately
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
fn integer_domain(key: &str) -> std::ops::RangeInclusive<i64> {
    match key {
        // Tab-status policy: unlike ordinary sliders, the writer enforces the
        // RESOLVER's domain, so a saved value is exactly the value the next
        // reload classifies with.
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
/// the same thing.
///
/// Classified by spelling, then by sign: a whole number is what the writer's
/// parse admits (`[+-]?digits`), so digits too long for `i64` are still a
/// whole number, past the end their sign points at (not "not a number").
pub(crate) fn integer_requirement(key: &str, raw: &str) -> String {
    let (start, end) = integer_domain(key).into_inner();
    let raw = raw.trim();
    let digits = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        let above = raw
            .parse::<i64>()
            .map_or(!raw.starts_with('-'), |value| value > end);
        return if above {
            format!("at most {}", grouped_digits(end))
        } else if start == 0 {
            "0 or more".to_string()
        } else {
            format!("at least {}", grouped_digits(start))
        };
    }
    if start == 0 && end >= 1_000_000 {
        "a whole number, 0 or more".to_string()
    } else {
        format!(
            "a whole number from {} to {}",
            grouped_digits(start),
            grouped_digits(end)
        )
    }
}

/// `1234567` as `1,234,567`.
pub(crate) fn grouped_digits(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if value < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// Whether a `font_px` value is one the resolver ADMITS: finite and inside
/// `FONT_PX_MIN..=FONT_PX_MAX` — the same predicate
/// `app_config::resolve_font_px_with` filters each source by, so a value this
/// writer accepts is a value the next load applies.
fn font_px_admissible(px: f64) -> bool {
    px.is_finite() && (f64::from(crate::FONT_PX_MIN)..=f64::from(crate::FONT_PX_MAX)).contains(&px)
}

/// The admissible `font_px` range as the reply spells it (`6..=200`).
fn font_px_range() -> String {
    format!("{}..={}", crate::FONT_PX_MIN, crate::FONT_PX_MAX)
}

/// Build the correctly-TYPED `aterm-toml` item for `key` from its raw control text,
/// per [`edit_kind`]. A malformed numeric/bool — or an integer outside the key's
/// serde-representable domain ([`integer_domain`]) — is a
/// [`PrefsEditError::BadValue`] so a Save never writes a value the reload parser
/// would reject, and a `font_px` the resolver would ignore is a
/// [`PrefsEditError::OutOfRange`].
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
                .any(|e| aterm_types::scheme::parse_hex(e).is_none())
        {
            return Err(bad());
        }
        let array: aterm_toml::edit::Array = entries.into_iter().collect();
        return Ok(Item::Value(Value::Array(array)));
    }
    let value = match edit_kind(key) {
        EditKind::Float => {
            let px = trimmed.parse::<f64>().map_err(|_| bad())?;
            // `font_px` is the one float the resolver IGNORES rather than clamps
            // when it is out of range (`app_config::resolve_font_px_with` falls
            // through to the default), so a write outside that range is a save
            // that changes nothing — refused here, with the range, the way a
            // non-numeric value is refused above.
            if key == EDIT_FONT_PX && !font_px_admissible(px) {
                return Err(PrefsEditError::OutOfRange {
                    key: key.to_string(),
                    range: font_px_range(),
                });
            }
            Value::from(px)
        }
        // Sign/width enforcement AFTER the numeric parse: `-1` for a `usize`
        // field or `70000` for a `u16` is perfectly parseable i64 AND valid
        // TOML, but the reload's serde model rejects the whole file over it —
        // the exact corruption this function's contract forbids — so an
        // out-of-domain integer dies here as a BadValue, never in the file.
        EditKind::Integer => match trimmed.parse::<i64>() {
            Ok(n) if integer_domain(key).contains(&n) => Value::from(n),
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
        // An enum value must be one of the allowed options (case-insensitive). Store the
        // CANONICAL spelling from `options` so the file + introspection read cleanly; an
        // out-of-domain value is rejected (BadValue) rather than written as a style the
        // reload parser would silently fall back to (e.g. a typo'd "rainbwo").
        EditKind::Enum { options } => {
            if key == EDIT_DISPLAY_FONT && trimmed.contains('+') {
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
        // A colour must parse as RRGGBB/#RRGGBB; reject a malformed value at Save
        // rather than write a string the config loader would silently ignore.
        EditKind::Color => {
            if aterm_types::scheme::parse_hex(trimmed).is_none() {
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
/// consuming resolvers (`sparkle_deco_config` / `Config::matrix_rain_params` /
/// `update_auto_apply`); the conformance test
/// `nested_leaves_have_agreeing_rows_and_serde_round_trip` forces an arm for every
/// registered leaf, so a new leaf cannot silently fall to the blank fallback.
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
        EDIT_UPDATE_ENABLED => boolean(upd.and_then(|u| u.enabled), true),
        "update.auto_apply" => boolean(upd.and_then(|u| u.auto_apply), true),
        EDIT_REROUTE_ANNOUNCE => boolean(cfg.reroute.as_ref().and_then(|r| r.announce), true),
        // Seeded through the RESOLVER that owns the default, never a literal
        // `true` re-typed here — the streak pip's rule, so the switch and the
        // harness can never start in different positions.
        EDIT_HARNESS_ENABLED => boolean(Some(cfg.harness.policy.enabled), true),
        // The supervisor's own resolved mode, so a refused value shows the
        // default the reader kept; the full-power default is the placeholder,
        // never a seed (a seed is a written override).
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
        // The supervisor's own resolved level, so a value shows as the reader
        // took it (an off its limit, a refused one no setting); the
        // full-power default is the placeholder, never a seed (a seed is a
        // written override).
        EDIT_HARNESS_APPROVE => {
            let level = cfg.harness.policy.approve.word();
            (
                (level != "all").then(|| level.to_string()),
                "all".to_string(),
            )
        }
        // The word the supervisor reads, the default its placeholder (a
        // seed is a written override): a refused word is no setting, read as
        // the default, and the answer switch is left as written.
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
        // [ai]: seeded from the AI layer's RESOLVED value, the fleet document
        // included (`AiPolicy::settings`), so a choice the fleet makes is the
        // row's state; Settings ▸ Harness ▸ AI CLIs says where it came from,
        // and Reset removes only this machine's key.
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
        "matrix_rain.hue" => txt(mr.and_then(|m| m.hue.as_deref()), "matrix (default)"),
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
        // PRISM WAKE's pip seeds its RESOLVED state through the resolver that
        // owns the default, never a literal `true` re-typed here: the owner's
        // 2026-08-31 "default on" ruling has exactly one home
        // (`Config::output_streak_sound_or_default`), so the switch and the
        // engine can never start in different positions.
        EDIT_OUTPUT_STREAK_SOUND => boolean(Some(cfg.output_streak_sound_or_default()), true),
        // PRISM WAKE's switch, from the resolver that owns its platform-split
        // default, so the row starts where the engine does.
        EDIT_OUTPUT_STREAK_ENABLED => boolean(
            Some(cfg.output_streak_enabled_or_default()),
            crate::app_config::DEFAULT_DECORATIVE_EFFECTS,
        ),
        // The presence surfaces seed their RESOLVED state through the resolvers
        // that own the default (both ON), the streak pip's rule.
        EDIT_PRESENCE_BAND => boolean(Some(cfg.presence_band_enabled()), true),
        EDIT_PRESENCE_RIM => boolean(Some(cfg.presence_rim_enabled()), true),
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
    /// The Cursor Styles page's keys: the cursor's style (the page's list)
    /// and the tuning a style reads. The page draws its own rows.
    CursorStyles,
    Typography,
    /// Window sizing, tabs, tab status, and chrome.
    Window,
    /// Keyboard & clipboard behavior (copy-on-select, paste safety, Option-as-Meta,
    /// predictive echo).
    Input,
    /// Keys with no ordinary page: the renderer, focus and recording switches
    /// are Manual-only.
    Performance,
    Terminal,
    Security,
    /// The `[packages]` toolchain-manager maintenance switches. No ordinary
    /// registry page owns this section (the native Packages route is a SPECIAL
    /// page that renders these rows itself), so it surfaces through Search and
    /// the Modified review only.
    Packages,
    /// The aterm WRAPPER's own page (design
    /// `docs/DESIGN-aterm-wrapper-2026-09-17.md` §4.6.2), Settings ▸ Harness
    /// (`SettingsRoute::Harness`, `docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §4). Its rows are FIVE `[harness]` keys — the durable master switch
    /// `harness.enabled`, the default Autopilot `harness.autopilot`
    /// (2026-10-15), the approval level `harness.approve` (2026-09-24), the
    /// question answer `harness.answer_questions` and how it answers,
    /// `harness.question_priority` (2026-09-25) — and one switch per
    /// supervisable agent, `harness.<agent>.enabled`
    /// (`docs/DESIGN-harness-plugins-2026-10-15.md` §4). The finer `[harness]` keys —
    /// each able only to limit — are written in Manual, where the config
    /// language lists them with their full-power defaults. Search and the
    /// Modified review reach every row through the registry, the master
    /// switch's note naming every limit the table writes. The route renders
    /// them on its special page (`native_settings_harness.rs`), the SUPERVISION
    /// card among STATUS, SESSIONS, WHAT THE HARNESS DID, EFFECT OF EACH
    /// AUTOPILOT and SETTINGS IN ATERM.TOML.
    Harness,
    /// aterm's own updater switches (`[update]`). Settings ▸ Software Update
    /// is a SPECIAL page that draws these rows itself, so no ordinary
    /// registry page owns this section; Search and Modified find them.
    Updates,
    /// Every key that changes what aterm sounds like ([`SOUND_MENU_KEYS`]).
    Sound,
}

impl Section {
    /// Display order of the section headers (top → bottom).
    pub(crate) const ORDER: [Section; 13] = [
        Section::Appearance,
        Section::Cursor,
        Section::CursorStyles,
        Section::Sound,
        Section::Typography,
        Section::Window,
        Section::Input,
        Section::Terminal,
        Section::Performance,
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
            Section::Performance => "Performance",
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
    // Nested tables route by PREFIX (one arm per table, not per leaf):
    // the updater's switches are Software Update's; the two costume tables are
    // screen decorations (Appearance).
    if key.starts_with("update.") {
        return Section::Updates;
    }
    // The supervisor's rows answer to the Harness page and to nothing else —
    // they are not an update, a package or a terminal knob. So do the AI
    // CLIs' rows: Settings ▸ Harness ▸ AI CLIs is its sub-page, which draws
    // them (the Harness page's own cards leave them to it).
    if key.starts_with("harness.") || key.starts_with("ai.") {
        return Section::Harness;
    }
    // THE SOUND PAGE (owner ask: "add the volume and SFX menu to settings").
    // Every audible knob in the product answers to ONE page. The curse BONK
    // lives under `[sparkle_words.profanity]` in the file, but it is a sound,
    // so it is routed here BEFORE the decorative-table prefix rule below; the
    // output streak's pip (an `[output_streak]` leaf) rides the same return.
    if SOUND_MENU_KEYS.contains(&key) {
        return Section::Sound;
    }
    if key.starts_with("sparkle_words.")
        || key.starts_with("matrix_rain.")
        || key.starts_with("output_streak.")
    {
        return Section::Appearance;
    }
    // The presence band and rim are WINDOW CHROME — the row under the tab bar
    // and the frame around the grid — so they sit with the tab strip and the
    // tab-status badges, beside the other things a window says about its
    // session.
    if key.starts_with("presence.") {
        return Section::Window;
    }
    // THE CURSOR STYLES PAGE: the style list and the tuning a style reads
    // (the page shows the selected style's own knobs). `cursor_nyan_sprite`,
    // HDR glow and SDR boost are Manual-only; they ride this section for
    // Search and Modified. The cursor's shape is the cursor's own, on Cursor
    // & Motion beside its blink.
    if matches!(
        key,
        EDIT_CURSOR_TRAIL | EDIT_CURSOR_TRAIL_STYLE | EDIT_CURSOR_NYAN_SPRITE
    ) || CURSOR_STYLE_TUNING_KEYS.contains(&key)
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
        // The indexed palette + M5 glass are colour-behaviour too, and the
        // wallpaper backdrop is the same surface (what the window ground shows).
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
        EDIT_WINDOW_PADDING | EDIT_WINDOW_PADDING_TOP | EDIT_RESTORE_SESSION => Section::Window,
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
        | EDIT_PREDICTIVE_ECHO => Section::Input,
        // Focus-linked QoS is a performance knob, not an input one — as are the
        // launch-time renderer choice and the opt-in replay recorder.
        EDIT_FOCUS_BOOST | EDIT_GPU | EDIT_TEMPORAL_RECORDING => Section::Performance,
        // aterm's own desktop notifications ride the Security page beside the
        // programs' (`allow_notifications`): one page answers "who may put a
        // notification on my desktop".
        EDIT_DESKTOP_ALERTS => Section::Security,
        // The [packages] maintenance switches live on the special Packages page;
        // this section keeps them findable (Search/Modified) without also
        // duplicating them onto an ordinary registry page.
        // …and the reroute's announcement: the rerouted toolchain names are the package
        // manager's (atpkg lays and reads them), so the switch rides the same page.
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

/// The GROUP-BOX a control belongs to inside its [`Section`] pane: `(caption, order)`
/// per the settings-v2 design §3.2 table. The caption is the uppercase group header
/// drawn above the rounded box; `order` sorts the groups top→bottom within the section
/// (fields keep their [`editable_fields`] build order within a group). Callers that
/// include an ungrouped key place it in a trailing per-section "General" box.
pub(crate) fn group_of(key: &str) -> (&'static str, u8) {
    // THE SOUND PAGE, before every prefix rule below: the synth voices under
    // the Music effects switch, then the system bell, which that switch and the
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
    // Window › the presence surfaces, beside Tab Status (both answer "what does
    // this window say about who is driving its session").
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
        // The window behind the text: M5 glass (opacity + vibrancy material).
        EDIT_BACKGROUND_OPACITY | EDIT_BACKGROUND_MATERIAL => ("Background", 3),
        // The terminal-tab backdrop image, its legibility dim, and the
        // backdrop-hue glyph tint.
        EDIT_WALLPAPER | EDIT_WALLPAPER_DIM | EDIT_WALLPAPER_TEXT_TINT => ("Wallpaper", 4),
        EDIT_SERIOUS_MODE => ("Effect policy", 0),
        EDIT_CURSOR_STYLE | EDIT_CURSOR_BLINK | EDIT_CURSOR_MOMENTUM_GLOW => ("Cursor", 0),
        EDIT_CURSOR_TRAIL
        | EDIT_CURSOR_TRAIL_MS
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
        EDIT_RESTORE_SESSION => ("Session", 4),
        // Input › clipboard (copy and paste) then keyboard.
        EDIT_COPY_ON_SELECT | EDIT_CONFIRM_MULTILINE_PASTE => ("Clipboard", 0),
        EDIT_OPTION_AS_META | EDIT_PREDICTIVE_ECHO => ("Keyboard", 1),
        // Performance › focus-linked QoS, launch-time renderer choice, and replay.
        EDIT_FOCUS_BOOST | EDIT_GPU | EDIT_TEMPORAL_RECORDING => ("System", 1),
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

/// The Sound page's two notes: the one fact its rows cannot state. Every row but
/// [`EDIT_BELL_SOUND`] is a synth voice under `trail_sounds`, scaled by
/// `trail_sound_volume` (the voice table, `aterm_effects::sound_policy::
/// voice_gain`); the bell is the system alert sound (`lib.rs::on_bell`),
/// which reads neither. Every other caveat is a per-row disclosure
/// (`native_settings::setting_effect_projection`).
const SOUND_NOTE: &str =
    "Music effects turns these on or off together; Sound volume sets how loud they are.";

/// What one Sound voice does, for its own row ([`footnote_of`]). `None` for
/// the master switch and the volume, which share [`SOUND_NOTE`].
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
        EDIT_CHOICE_SOUND => "A short chime when aterm answers a coding agent\u{2019}s question.",
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
const BELL_SOUND_NOTE: &str =
    "The bell is your system's alert sound; Music effects and Sound volume do not change it.";

/// The consequence note for one setting: what its own row cannot say, drawn
/// inside that row under its name ([`row_notes`]), so a note never sits under
/// another row, on a screen, a search result or a Modified list that lacks its
/// own. `None` for rows that need none — notes explain consequences, they never
/// restate the control (an empty color field already says "Theme default").
pub(crate) fn footnote_of(key: &str) -> Option<&'static str> {
    if key == EDIT_BELL_SOUND {
        return Some(BELL_SOUND_NOTE);
    }
    // What each voice sounds like, in plain words, wherever its row shows: a
    // name like "Ambient sound bed" says nothing on its own. The voices that
    // have one leave the shared note to Sound volume, the box's first row.
    if let Some(note) = sound_voice_note(key) {
        return Some(note);
    }
    if SOUND_MENU_KEYS.contains(&key) {
        return Some(SOUND_NOTE);
    }
    if key == EDIT_SECURE_KEYBOARD_ENTRY {
        return Some("Secure Keyboard Entry hides what you type from other apps.");
    }
    if SECURITY_BOOL_KEYS.contains(&key) {
        return Some(
            "Off refuses the program's request. Only the color palette is allowed by default.",
        );
    }
    Some(match key {
        // What sets each toy off, wherever its switch shows: Top Settings,
        // search, Modified.
        "sparkle_words.profanity.enabled" => {
            "Sparkle words: swear words in output burst into sparkles."
        }
        "sparkle_words.feline.enabled" => {
            "Keyword kitties: a little cat peeks out by words like \u{201c}cat\u{201d} and \
             \u{201c}kitty\u{201d}."
        }
        EDIT_CURSOR_TRAIL_COLOR | EDIT_CURSOR_TRAIL_ACCENT => {
            "Used by trails that take a color; blank follows the theme's cursor color."
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
        // Always animate overrides it; the row says so while it does.
        EDIT_LOAD_ADAPTIVE_MOTION => {
            "Drops decorative effects while drawing falls behind. Scrolling stays smooth."
        }
        EDIT_CURSOR_TRAIL_BLOOM
        | EDIT_CURSOR_TRAIL_BLOOM_STRENGTH
        | EDIT_CURSOR_TRAIL_BLOOM_RADIUS
        | EDIT_CURSOR_FIRE_SHIMMER
        | EDIT_HDR_GLOW
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
        // LIVE, and says so: the window's package loop re-reads `[packages] enabled`
        // (the retired `auto_update` folded in) before every pass.
        // Only what is true in every copy: where Check & Update Now works, the
        // Packages page's own note says so.
        EDIT_PACKAGES_ENABLED => {
            "Keeps ALab tools and the coding agents aterm manages up to date, starting at once."
        }
        // ONE install lane: no install ships a copy to install from, so the switch
        // is consent to download. Off, nothing installs that nobody asked for
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
/// What a CLI's own switch does, under the last of them: one note the CLIs
/// share, so it is drawn once.
pub(crate) const AI_USE_NOTE: &str = "Off, aterm never signs that CLI in here, never asks \
     it for a review and says nothing about it.";

/// What supervision does, under the SUPERVISION card's master switch.
pub(crate) const HARNESS_NOTE: &str = "aterm keeps your coding agents working by answering \
     their permission boxes and questions for you.";
/// What an agent's own switch does, under the last of them: one note the
/// agents share, so it is drawn once.
pub(crate) const HARNESS_AGENT_NOTE: &str = "Off, aterm leaves that agent's sessions alone; \
     Supervise coding agents turns supervision off for all of them.";
/// What the default autopilot's modes mean, for a session with none of its
/// own.
pub(crate) const HARNESS_AUTOPILOT_NOTE: &str = "For sessions with no autopilot of their own: \
     Cruise keeps the work moving until the agent says it is done; Land wraps the work up, then \
     stops; Off leaves the session alone.";
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

/// The note each of `keys` (a box's rows, top to bottom) draws inside its
/// row: its own [`footnote_of`], where no later row shares it. A note several
/// rows share (the Sound voices' one, the security switches' one) is drawn
/// once, in the last of them, so it sits in a row it explains and never
/// repeats down the box.
pub(crate) fn row_notes(keys: &[&str]) -> Vec<Option<&'static str>> {
    keys.iter()
        .enumerate()
        .map(|(position, key)| {
            let note = footnote_of(key)?;
            let shared_later = keys[position + 1..]
                .iter()
                .any(|later| footnote_of(later) == Some(note));
            (!shared_later).then_some(note)
        })
        .collect()
}

/// [`application_timing`] of the Harness row: supervision starts or stops
/// on every agent session the moment the file is saved (`harness_host`, on
/// the config reload). There is no launcher half, whatever the 2026-09-23
/// text promised: aterm wraps no agent's launch, so nothing about the switch
/// waits for a new session.
pub(crate) const HARNESS_TIMING: &str = "Supervision starts or stops when saved";

/// [`application_timing`] of the default-Autopilot row: live like the
/// switch — every session with no Autopilot word of its own runs under the
/// saved mode from the host's next pass (an `off` stops its supervisor, and
/// `cruise` or `land` starts one), and a session's own word is left alone.
pub(crate) const HARNESS_AUTOPILOT_TIMING: &str =
    "Applies when saved to every session without an autopilot of its own";

/// [`application_timing`] of the Land at row: the next Autopilot menu, or
/// session row, that opens offers the time saved; a timer already armed
/// keeps its own instant.
pub(crate) const HARNESS_LAND_AT_TIMING: &str =
    "Applies when saved: the Autopilot menu offers it next time it opens";

/// [`application_timing`] of the approval-level row: the host restarts every
/// supervised agent session's worker under the policy the save produced
/// (`harness_host`, on the config reload), and nothing about the launcher
/// changes.
pub(crate) const HARNESS_APPROVE_TIMING: &str =
    "Applies when saved: every supervised agent session restarts its supervisor under it";

/// [`application_timing`] of an agent's own switch: live like the master
/// switch, through the host's admission (`harness_host`, on the config
/// reload) — switched off, that agent's workers stop and its sessions are
/// left to the person; switched on, they are supervised again.
pub(crate) const HARNESS_AGENT_TIMING: &str =
    "Applies when saved: supervision of its sessions stops or starts";

/// When an authored value becomes effective. Most preferences are projected
/// live. Keep this metadata beside the complete preference schema so Advanced,
/// Modified, and the Manual language service cannot disagree about a saved
/// value's lifecycle.
pub(crate) fn application_timing(key: &str) -> Option<&'static str> {
    match key {
        EDIT_COLUMNS | EDIT_LINES => Some("Applies next launch; an update keeps the current size"),
        // NOT `packages.enabled` (Phase 4): the window's package loop re-reads it (the
        // retired `auto_update` folded in) before every pass and every few seconds of a
        // park, so it is live — no timing to disclose. `[update] enabled` is read once per
        // process, so it applies next launch.
        EDIT_GPU | EDIT_UPDATE_ENABLED => Some("Applies next launch"),
        EDIT_ALLOW_KITTY_FILE_TRANSFER | EDIT_TEMPORAL_RECORDING | EDIT_SHELL | EDIT_SHELL_ARGS => {
            Some("Applies to new sessions")
        }
        // The in-GUI supervisor starts or stops on every agent session the
        // moment the file is saved (`harness_host`, on the config reload).
        EDIT_HARNESS_ENABLED => Some(HARNESS_TIMING),
        // The default Autopilot is live the same way, for every session with
        // no word of its own.
        EDIT_HARNESS_AUTOPILOT => Some(HARNESS_AUTOPILOT_TIMING),
        // Offered by the next menu that opens.
        EDIT_HARNESS_LAND_AT => Some(HARNESS_LAND_AT_TIMING),
        // Live the same way (2026-09-24), with no launcher half.
        // So is the question answer (2026-09-25), and how it answers.
        EDIT_HARNESS_APPROVE | EDIT_HARNESS_ANSWER_QUESTIONS | EDIT_HARNESS_QUESTION_PRIORITY => {
            Some(HARNESS_APPROVE_TIMING)
        }
        // Each agent's own switch is live too: the host admits or revokes
        // that agent's workers at its next pass.
        k if agent_switch(k).is_some() => Some(HARNESS_AGENT_TIMING),
        EDIT_HDR_GLOW => Some("Disabling applies now; enabling may require a new window"),
        EDIT_RESTORE_SESSION => Some("Applies when closing or next launch"),
        EDIT_REROUTE_ANNOUNCE => Some("Applies to the next cargo or rustc run"),
        // Universal Control too: the package tool applies `[machine]` on its
        // pass. Apply Now, where there is something to apply, sits under the
        // Security row and says what it does, so this sentence never names a
        // button the page may not show.
        EDIT_PACKAGES_AUTO_INSTALL
        | "packages.account"
        | "packages.exclude"
        | "packages.links"
        | EDIT_MACHINE_UNIVERSAL_CONTROL => Some("Applies at the next ALab tools check"),
        "update.owner" | "update.repo" => Some(
            "A development build only: applies on the next manual or automatic update \
             check; a shipped build reads its compiled channel",
        ),
        "update.auto_apply" => Some("Applies now, including a downloaded update"),
        EDIT_MATRIX_RAIN_ENABLED => {
            Some("Applies live unless this session has a View menu override")
        }
        EDIT_AMBIGUOUS_WIDTH => {
            Some("Applies to newly received text; existing cells keep their current width")
        }
        _ => None,
    }
}

/// [`application_timing`] as the plain words a row shows on its second line:
/// when a saved change takes effect, said as a short clause ("Applies after
/// restart", "Applies to new sessions"). `None` for a key whose change takes
/// effect as soon as it is saved — the norm, which a row never states — and
/// for Matrix rain, whose one exception (a session's View menu override) is
/// its note's to explain.
pub(crate) fn application_timing_badge(key: &str) -> Option<&'static str> {
    if key == EDIT_MATRIX_RAIN_ENABLED {
        return None;
    }
    application_timing(key).and_then(|timing| match timing {
        // Live: supervision and the default autopilot follow the save at
        // once.
        HARNESS_TIMING
        | HARNESS_APPROVE_TIMING
        | HARNESS_AUTOPILOT_TIMING
        | HARNESS_AGENT_TIMING => None,
        "Applies now, including a downloaded update" => None,
        "Applies to new sessions" => Some("Applies to new sessions"),
        "Disabling applies now; enabling may require a new window" => {
            Some("Turning it on may need a new window")
        }
        "Applies when closing or next launch" => Some("Applies when aterm closes or restarts"),
        "Applies to the next cargo or rustc run" => Some("Applies to the next cargo or rustc run"),
        // Apply Now, where there is something to apply, sits under the row
        // and says what it does.
        "Applies at the next ALab tools check"
        | "Applies at the next ALab tools check, or when you press Apply Now" => {
            Some("Applies at the next ALab tools check")
        }
        "Applies to newly received text; existing cells keep their current width" => {
            Some("Applies to new text only")
        }
        "Applies on the next dial" => Some("Applies at the next connection"),
        timing if timing.starts_with("A development build only") => Some("Development builds only"),
        // Next launch, whatever else the sentence adds (columns and lines: an
        // update keeps the current size).
        _ => Some("Applies after restart"),
    })
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
        EDIT_AMBIGUOUS_WIDTH
            | EDIT_MATRIX_RAIN_ENABLED
            | EDIT_HDR_GLOW
            | EDIT_HARNESS_ENABLED
            | EDIT_HARNESS_AUTOPILOT
            | EDIT_HARNESS_LAND_AT
            | EDIT_HARNESS_APPROVE
            | EDIT_HARNESS_ANSWER_QUESTIONS
            | EDIT_HARNESS_QUESTION_PRIORITY
    ) || agent_switch(key).is_some()
        || application_timing(key).is_none()
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
                // The manipulation half is wired there (frame audit #4).
                "Allow programs to resize the window and read its title and size"
            } else {
                // No host callback off-Linux, so this is reports only — but
                // "text-grid-size" undersold them once the engine gained its
                // in-core pixel answers (CSI 14 t text area, CSI 16 t cell
                // box). The Manual entry enumerates; the row says "size".
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
/// seam: macOS observes Reduce Motion live, Windows samples its animations
/// switch when a window attaches, and other platforms have no OS query yet.
/// Returns the Settings note under the Motion row and Manual's help fragment.
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

/// HOW A NUMBER READS: a numeric setting's value as its slider, the
/// preview's badge, a status and its default say it, from the stored `value`
/// and the number as written (`shown`). A 0–1 fraction reads as a
/// percentage, Bloom strength as a percentage of its scale (0.85 is 85%),
/// Bloom size as a multiple (its unit is the renderer's), and every other
/// counted value with its unit. `None` keeps the bare number.
pub(crate) fn numeric_reading(key: &str, value: f64, shown: &str) -> Option<String> {
    let unit = match key {
        EDIT_FONT_PX => "px",
        EDIT_CURSOR_TRAIL_MS => "ms",
        EDIT_CURSOR_TRAIL_LENGTH | EDIT_CURSOR_TRAIL_RADIUS => "cells",
        EDIT_CURSOR_TRAIL_BLOOM_STRENGTH => return Some(format!("{:.0}%", value * 100.0)),
        EDIT_CURSOR_TRAIL_BLOOM_RADIUS => return Some(format!("{shown}\u{00d7}")),
        _ if range_of(key).is_some_and(|range| range.min == 0.0 && range.max == 1.0) => {
            return Some(format!("{:.0}%", value * 100.0));
        }
        _ => return None,
    };
    Some(format!("{shown} {unit}"))
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
        EDIT_CURSOR_TRAIL_INTENSITY => r(0.0, 1.0, 0.05),
        EDIT_CURSOR_TRAIL_RADIUS => r(0.0, 2.0, 0.05),
        EDIT_CURSOR_TRAIL_BLOOM_STRENGTH => r(0.0, 3.0, 0.05),
        EDIT_CURSOR_TRAIL_BLOOM_RADIUS => r(0.5, 8.0, 0.1),
        EDIT_CURSOR_GLOW_SDR_BOOST => r(0.0, 1.0, 0.05),
        EDIT_TAB_STRIP_ROWS => r(0.0, 4.0, 1.0),
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
        EDIT_FONT_SIZE_SCALE => r(0.8, 1.25, 0.01),
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
        // Nested matrix-rain numerics — `Config::matrix_rain_params`'s clamp domains.
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

/// Discovery aliases for the Shell row: the shells a user would SEARCH for on
/// this platform. Keywords exist to find a row by intent, so they must name the
/// shells that actually run here — a Windows first-run typing "powershell" found
/// nothing, because the list was the Unix one ("bash", "zsh") on every target.
/// The Windows entries are exactly the aliases the PTY already resolves by name
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

/// What the Shell field shows when nothing is configured. On Windows "platform
/// default" alone left the by-name aliases (which need no path) undiscoverable;
/// naming them is the whole prompt a user needs.
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

/// Extra search keywords for a control so a fuzzy query finds it by INTENT, not only by
/// its label (e.g. "dark mode" → `window_theme`, "history" → `scrollback_lines`). The
/// label, key, and section name are always part of the search corpus; this augments
/// them. An unknown key contributes no extra keywords (empty slice).
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
        EDIT_CURSOR_TRAIL
        | EDIT_CURSOR_TRAIL_MS
        | EDIT_CURSOR_TRAIL_LENGTH
        | EDIT_CURSOR_TRAIL_INTENSITY
        | EDIT_CURSOR_TRAIL_RADIUS
        | EDIT_CURSOR_TRAIL_RING => &["effect", "motion", "comet", "trail"],
        // THE COMPANION PICKER INHERITS THE CAT'S NAMES. Until 2026-09-16 the
        // only row in Settings carrying "kitty"/"rainbow"/"nyan" as discovery
        // aliases was the typing-wake dial — so a user who searched "kitty" was
        // answered by a control that had not moved a pixel since the v1 wake
        // walk was deleted, while the picker that actually chooses the cat
        // matched nothing. Retiring that dial must not leave the cat
        // unsearchable by its own name, so the aliases move to the row they
        // always described.
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
        // PRISM WAKE's pip. A user hunting this row knows it by WHAT MADE IT
        // HAPPEN ("output", "streak", "comet") or by what it sounds like
        // ("pip", "chime"), never by the design's code name — and "rainbow"
        // earns its place because that is the word the owner used for the
        // effect ("a rainbow streak when in terminal there is output").
        EDIT_OUTPUT_STREAK_SOUND => &[
            "streak", "output", "prism", "comet", "rainbow", "pip", "chime", "sound", "sfx",
        ],
        // The supervisor's chime is found by what it answers.
        EDIT_CHOICE_SOUND => &[
            "choice", "chime", "harness", "question", "answer", "claude", "sound", "sfx",
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
        // The retired spellings a person may still HAVE in a file find the switch that
        // reads them (`auto_update` → this one, `seed_install` → the install consent).
        EDIT_PACKAGES_ENABLED => packages_keywords(),
        // The two switches of Software Update's Automatic updates card, findable by the
        // words that card paints at any width ("Auto-check updates", "Install").
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
        EDIT_PRESENCE_BAND | EDIT_PRESENCE_RIM => &[
            "presence", "band", "rim", "driving", "driven", "hold", "fabric", "manager", "worker",
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
        // The question answer answers to what a person reaches for when the
        // agent keeps stopping on a question: the tool's name, the words of
        // the dialog, and the words for answering it automatically.
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
        // HOW a question is answered: the word "priority", the two policy
        // words, and what a person reaches for to have the worker decide.
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
        // The one word most people will search for here is the vendor's — so
        // the wrapper's switch answers to "claude" as well as to "harness",
        // and to the two words an operator reaches for when they want it to
        // stop.
        k if k.starts_with("harness.") => &[
            "harness",
            "wrapper",
            "claude",
            "agent",
            "disable",
            "off",
            "kill switch",
        ],
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
    // The saved style's own trail colour, where it keeps one (Laser).
    let own_trail_color = crate::app_config::resolve_trail_style(cfg.cursor_trail_style_raw())
        .style
        .and_then(aterm_effects::cursor_fx::own_trail_color);
    let cursor_nyan_sprite = configured_str(cfg.cursor_nyan_sprite.as_deref());
    let foreground = configured_str(cfg.foreground.as_deref());
    let background = configured_str(cfg.background.as_deref());
    let cursor_color = configured_str(cfg.cursor_color.as_deref());
    let selection_color = configured_str(cfg.selection_color.as_deref());
    let selection_foreground = configured_str(cfg.selection_foreground.as_deref());
    let predictive_echo = configured_str(cfg.predictive_echo.as_deref());
    let mut fields = vec![
        // Family before size — the Typography "Font" group-box order (design §3.2).
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
            label: "Mark the active split pane",
            key: EDIT_SPLIT_FOCUS_MARK,
            kind: EditKind::Bool,
            // The checkbox reflects the RESOLVED state (default ON); Save writes
            // the explicit bool.
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
            // Default ON (the checkbox reflects the resolved state directly).
            seed: Some(cfg.cursor_blink.unwrap_or(true).to_string()),
            placeholder: String::new(),
        },
        EditField {
            // The cursor glows with how fast you type and cools when you stop; while
            // warm it does not blink (owner, 2026-09-08: "the blinking cursor is
            // annoying"). Default ON; the checkbox reflects the resolved state.
            label: "Typing momentum glow",
            key: EDIT_CURSOR_MOMENTUM_GLOW,
            kind: EditKind::Bool,
            seed: Some(cfg.cursor_momentum_glow_or_default().to_string()),
            placeholder: String::new(),
        },
        EditField {
            label: "Cursor effects",
            key: EDIT_CURSOR_TRAIL,
            kind: EditKind::Bool,
            // Master on/off for the cursor wake (default ON). The checkbox reflects the
            // RESOLVED state directly, so no placeholder.
            seed: Some(cfg.cursor_trail_or_default().to_string()),
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
            placeholder: cursor_trail_style
                .clone()
                .unwrap_or_else(|| format!("{DEFAULT_CURSOR_TRAIL_STYLE} (default)")),
            seed: cursor_trail_style,
        },
        EditField {
            // The master switch for every synth sound: the typing notes (each
            // style's signature palette on the spawn edge that lights the
            // trail), the ambient bed, the song, the output streak's note, the
            // build verdict, the bonk and the supervisor chime. Default ON.
            // Only the typing notes follow the trail's light (silent with the
            // Off style, reduced motion or an unfocused window); the other
            // sounds keep their own gates. The Sound volume row below trims
            // the level.
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
            // that swells behind fast typing. Default ON, voiced from the live
            // chord — the toggle gates the synth's bed mixer entirely
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
            // sound (`music box`, `droplet`, …), the `mechanical` keyboard,
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
            // THE SUPERVISOR'S CHOICE CHIME (owner: "a little sound … for
            // choosing"): the in-window supervisor answered a question box by
            // policy. A synth voice, under Music effects. Seeded through its
            // resolver.
            label: "Chime when the supervisor answers",
            key: EDIT_CHOICE_SOUND,
            kind: EditKind::Bool,
            seed: Some(cfg.choice_sound_or_default().to_string()),
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
                None => "1.0 (default)".to_string(),
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
            // Unset, the trail draws the saved style's own colour where it
            // has one (Laser's violet), else the live cursor colour.
            placeholder: cursor_trail_color.clone().unwrap_or_else(|| {
                own_trail_color.map_or_else(
                    || "Cursor color (default)".to_string(),
                    |(_, name)| format!("{name} (default)"),
                )
            }),
            seed: cursor_trail_color,
        },
        EditField {
            label: "Trail accent color",
            key: EDIT_CURSOR_TRAIL_ACCENT,
            kind: EditKind::Color,
            placeholder: cursor_trail_accent.clone().unwrap_or_else(|| {
                own_trail_color.map_or_else(
                    || "Brighter cursor color (default)".to_string(),
                    |(_, name)| format!("Brighter {} (default)", name.to_lowercase()),
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
            // Load-adaptive shedding drops decorative effects under sustained RENDER
            // overload. Default ON; the checkbox reflects the resolved state directly.
            // Turn OFF (or set Motion = full) to keep the cursor trail / aurora on
            // regardless of load. Smooth scrolling and the scroll pill are never
            // shed under either setting (`App::effect_policy`).
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
            // `[packages] enabled`: "Keep ALab tools up to date" — THE one switch for the
            // background package lane (the retired `auto_update` folded in, seeded
            // RESOLVED), applied LIVE: the window's package loop re-reads it before every
            // pass and stands down or resumes without a relaunch. Explicit Update Now /
            // Install actions remain available.
            label: "Keep ALab tools up to date",
            key: EDIT_PACKAGES_ENABLED,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_enabled().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // `[packages] auto_install` (dotted key): THE install consent — the
            // first-run fill and new members of the signed set. Default ON, batteries
            // included (the retired `seed_install` folded in, seeded RESOLVED); the
            // co-located atpkg reads the same bit from the same table.
            label: "Install ALab tools",
            key: EDIT_PACKAGES_AUTO_INSTALL,
            kind: EditKind::Bool,
            seed: Some(cfg.packages_auto_install().to_string()),
            placeholder: String::new(),
        },
        EditField {
            // `[machine] universal_control` (dotted key): the co-located atpkg
            // disables macOS Universal Control for this host whenever it applies the
            // `[machine]` settings, unless this says "leave". Enum rows seed the CONFIGURED
            // raw value (blank when unset) with the effective default in the
            // placeholder — the same law as every other non-Bool row.
            // The row sits in the "This Mac" group, so its name needn't say so.
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
    // ---- FULL-COVERAGE batch: the remaining scalar keys, the list rows, and the
    // nested-table leaves — so Manual and `settings set` cover EVERY user setting.
    // Same seeding law as above: configured raw value only
    // (unset = blank), resolved state for Bools, effective default in the
    // placeholder; list rows seed the comma-joined form their Save re-splits.
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
            // Resolved CONFIG state (default ON with CPU fallback); the env/CLI
            // overrides are process-fixed and deliberately not folded in here.
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
        // (The Sound-menu rows — master, volume, palette, melody, bed, riff and
        // the terminal bell — are built together above, so the box paints in
        // the order the owner asked to read it.)
        // (The trail colour/accent, kitty sprite, comet-geometry, bloom, shimmer
        // and HDR/SDR glow ROWS live in the core list above — one row per key.)
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
            // The RESOLVED state (default OFF): the owner turned them off.
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
            label: "Sides and bottom (px)",
            key: EDIT_WINDOW_PADDING,
            kind: EditKind::Float,
            seed: cfg.window_padding.map(|v| format!("{v}")),
            placeholder: match cfg.window_padding {
                Some(v) => format!("{v} px"),
                None => "12 (default)".to_string(),
            },
        },
        EditField {
            label: "Top (px)",
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

/// The result of a Preferences save, so the window can show visible feedback (a status
/// line) rather than silently succeeding/failing. `Saved` means the file actually changed
/// and a reload should follow; `Unchanged` is a true all-no-op Save; `Conflict`
/// preserves the expected and observed disk generations for a retry UI;
/// `PublishedUnverified` requires reconciliation before retry; and `Error`
/// carries a short human message for a pre-publication failure.
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
                    outcome: SaveOutcome::Error(crate::native_config_service::config_read_error(
                        &path, error,
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
        crate::native_document_host::AtomicCommitResult::Failed { stage, message } => {
            let message = format!("{} save failed at {stage:?} ({message})", path.display());
            crate::logging::stderr_line!("aterm-gui: prefs save: {message}; config unchanged");
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
            crate::logging::stderr_line!("aterm-gui: prefs save: {message}");
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

    /// The pet's twin pin: the KITTY-NAMED canonical styles (and their
    /// documented aliases) select the full-body CAT companion, and every other
    /// spelling — the explicit flying head they share a `GlowStyle` with, and
    /// the dog pet they share the pet machinery with — does not.
    ///
    /// TWO ENTRIES, NOT ONE, since 2026-08-26: `rainbow kitty` joined
    /// `rainbow kitty pet` because the owner asked twice for the kitty their
    /// config names and kept getting the flying head. The pin that matters is
    /// unchanged in kind — the picker's kitty entries and the draw path's
    /// predicate agree, and nothing else in the domain drifts into the pet.
    #[test]
    fn the_kitty_named_trail_styles_select_the_full_body_pet() {
        for &s in CURSOR_TRAIL_STYLES {
            assert_eq!(
                GlowStyle::style_names_kitty_pet(s),
                // …and `rainbow kitty underline` since 2026-08-27: it is a
                // GEOMETRY of `rainbow kitty`, and the engine's own
                // `style_names_underline_ribbon` docstring already promised the
                // companion was shared. It answered `false` here only because
                // the predicate is a whole-string equality it did not match, so
                // one appended geometry word swapped the animal on glass
                // (`pet_active=true cat_active=false` tall vs
                // `pet_active=false cat_active=true` underline, same input).
                // …and `rainbow kitty flat` since 2026-09-13, the A/B twin of
                // the default body: it must differ from the default in the
                // body alone, so it draws the same resident.
                // …and `rainbow kitty tall` since 2026-09-21, for the third
                // time and the plainest reason: `… tall` names the geometry
                // the DEFAULT ALREADY DRAWS, so the word changed nothing it
                // named and swapped the animal instead. Both docs a user
                // reads call it "an explicit spelling of the default".
                s == "rainbow kitty pet"
                    || s == "rainbow kitty"
                    || s == "rainbow kitty underline"
                    || s == "rainbow kitty flat"
                    || s == "rainbow kitty tall",
                "style {s:?}"
            );
        }
        for alias in [
            "kitty pet",
            "pet kitty",
            "  Kitty Pet  ",
            // The owner's spelling and the bare word.
            "rainbow kitty",
            "kitty",
            "  Rainbow Kitty  ",
        ] {
            assert!(
                GlowStyle::style_names_kitty_pet(alias),
                "documented alias {alias:?} must select the pet"
            );
        }
        for other in [
            "rainbow",
            "nyan",
            "pet",
            "rainbowkittypet",
            // The explicit flying-head spellings are the escape hatch: they must
            // never be swallowed by the widened kitty list.
            "rainbow kitty flying",
            "flying kitty",
            "kitty flying",
        ] {
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
                s == "rainbow dog pet"
                    || s == "rainbow kitty pet"
                    || s == "rainbow kitty"
                    || s == "rainbow kitty underline"
                    || s == "rainbow kitty flat"
                    || s == "rainbow kitty tall",
                "any-pet union, style {s:?}"
            );
        }
        for alias in ["dog pet", "pet dog", "rainbow puppy pet", "  Dog Pet  "] {
            assert!(
                GlowStyle::style_names_dog_pet(alias),
                "documented alias {alias:?} must select the dog pet"
            );
        }
        for other in [
            "rainbow",
            "dog",
            "puppy",
            "rainbowdogpet",
            "rainbow kitty pet",
            // THE DOG OPT-IN IS UNCHANGED by the kitty widening: no kitty
            // spelling, old or new, may reach the dog skin.
            "rainbow kitty",
            "kitty",
            "rainbow kitty flying",
        ] {
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
    /// would pass for a spelling nothing recognizes — which every other
    /// unrecognized value now falls back TO, so the constant has to be a look.
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
        // The flying head is not gone — it is one explicit line of config away.
        // `rainbow kitty` is NO LONGER that line (owner, twice, 2026-08-26: the
        // style named kitty must draw the kitty); the explicit spellings and
        // every historical alias are.
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
            // …AND THE SAME COMPANION. The style alone stopped being the whole
            // answer once `rainbow kitty` became the walking pet: an alias whose
            // canonical draws a different animal would make the Settings panel
            // name a companion the engine does not draw for that config, which
            // is exactly the split this table exists to prevent (it is why
            // `nyan`/`rainbow`/`nyan rainbow` canonicalise to the flying head).
            assert_eq!(
                GlowStyle::style_names_kitty_pet(alias),
                GlowStyle::style_names_kitty_pet(canonical),
                "alias {alias:?} and its canonical {canonical:?} draw different cats"
            );
            assert_eq!(
                GlowStyle::style_names_dog_pet(alias),
                GlowStyle::style_names_dog_pet(canonical),
                "alias {alias:?} and its canonical {canonical:?} draw different species"
            );
            // …AND THE SAME BODY. The engine draws the tall body unless the
            // spelling names the underline (`ribbon_tall` is
            // `!style_names_underline_ribbon`), and the flat body only when it
            // names it, so an alias whose canonical disagrees on either would
            // silently swap the body the user asked for. (`… tall` spellings
            // name the default's own body, so they share its canonical.)
            assert_eq!(
                GlowStyle::style_names_underline_ribbon(alias),
                GlowStyle::style_names_underline_ribbon(canonical),
                "alias {alias:?} and its canonical {canonical:?} draw different ribbons"
            );
            assert_eq!(
                GlowStyle::style_names_flat_ribbon(alias),
                GlowStyle::style_names_flat_ribbon(canonical),
                "alias {alias:?} and its canonical {canonical:?} draw different bodies"
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

    /// THE TALL RIBBON MUST NOT BE A TRAPDOOR — the pin for the defect measured
    /// on v0.60.0 (build 1787762776), where asking for a look the engine
    /// implements switched the cursor effect OFF.
    ///
    /// `cursor_trail_style_canonical` returning `None` is not "no style"; it is
    /// an unknown style, and `resolve_trail_style` answers that by
    /// substituting the DEFAULT style. So every spelling the ENGINE advertises
    /// has to be a spelling THIS module resolves, or the two halves disagree in
    /// the one direction the user cannot see: an explicit style silently falls
    /// back to the default, and the only word about it is a warning on the
    /// startup banner. (Before 2026-08-29 the same gap switched the whole
    /// cursor effect OFF — `effective=false intensity=0.00` under a
    /// `ribbon_look=tall` diagnostic.)
    ///
    /// Both directions are pinned. Forward: each historical explicit-tall
    /// spelling resolves to a canonical picker option that is itself still tall
    /// (an alias that canonicalised to an underline spelling would re-enable the
    /// trail while silently dropping the requested geometry). Backward: every
    /// every ordinary canonical rainbow-kitty spelling draws the tall default;
    /// only the one explicitly named `underline` selects the
    /// highlighter-plus-under-baseline alternate. Companion selection must not
    /// accidentally change the ribbon geometry.
    ///
    /// The four spellings are written out rather than imported because the
    /// engine exports the predicate, not the list; `cursor_glow`'s own
    /// `rainbow_spellings_share_one_mark_and_differ_only_in_the_shoulder`
    /// holds the other copy.
    #[test]
    fn every_tall_ribbon_spelling_stays_selectable() {
        for tall in [
            "rainbow kitty tall",
            "rainbow tall",
            "tall rainbow",
            "nyan tall",
        ] {
            assert!(
                GlowStyle::style_names_tall_ribbon(tall),
                "{tall:?} is one of the engine's tall spellings"
            );
            let canonical = super::cursor_trail_style_canonical(tall).unwrap_or_else(|| {
                panic!(
                    "{tall:?} resolves to None — the resolver would substitute the DEFAULT style \
                     for a look the engine implements"
                )
            });
            assert!(
                CURSOR_TRAIL_STYLES.contains(&canonical),
                "{tall:?} canonicalises outside the picker domain: {canonical:?}"
            );
            // Tall is the default body: every spelling but the underline
            // draws it (`ribbon_tall` is `!style_names_underline_ribbon`).
            assert!(
                !GlowStyle::style_names_underline_ribbon(canonical)
                    && !GlowStyle::style_names_flat_ribbon(canonical),
                "{tall:?} canonicalises to {canonical:?}, which is NOT tall — the trail would \
                 come back as the flat strip and the look would be silently dropped"
            );
            assert_eq!(
                GlowStyle::parse(tall),
                GlowStyle::RainbowKitty,
                "the tall body is a presentation of the rainbow ribbon, not a style of its own"
            );
        }
        // The predicate records an explicit `tall` token, not resolved
        // geometry: the tall spellings are aliases of the default, which
        // draws tall without matching it, so no listed style says tall.
        for &s in CURSOR_TRAIL_STYLES {
            assert!(!GlowStyle::style_names_tall_ribbon(s), "style {s:?}");
            // The underline spelling is the other half of the same axis: exactly
            // one canonical option SAYS underline, and the paint label reads that
            // predicate off the displayed (canonical) value.
            assert_eq!(
                GlowStyle::style_names_underline_ribbon(s),
                matches!(s, "rainbow kitty underline"),
                "style {s:?}"
            );
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
            assert_eq!(
                super::trail_sound_style_canonical(alias),
                Some(voice.name()),
                "{alias}"
            );
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
        Config, EDIT_BACKGROUND, EDIT_COLUMNS, EDIT_COPY_ON_SELECT, EDIT_CURSOR_COLOR,
        EDIT_CURSOR_STYLE, EDIT_CURSOR_TRAIL_STYLE, EDIT_FONT_FAMILY, EDIT_FONT_PX,
        EDIT_FOREGROUND, EDIT_LIGATURES, EDIT_LINES, EDIT_SCROLLBACK, EDIT_SEARCH_HISTORY_LINES,
        EDIT_SELECTION_COLOR, EDIT_THEME, EditKind, PrefsEditError, apply_prefs_edits,
        editable_fields, keywords_of, range_of,
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
        let c: Config = aterm_toml::from_str(&out).expect("round-trips through serde");
        assert!(c.matrix_rain_enabled());
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

    /// The inline-table spelling (`matrix_rain = { … }`) is table-like too:
    /// dotted edits address into it rather than clobbering it.
    #[test]
    fn dotted_write_addresses_into_an_inline_table() {
        let out = apply_prefs_edits(
            "matrix_rain = { enabled = false, fps = 24 }\n",
            &[(super::EDIT_MATRIX_RAIN_ENABLED, set("true"))],
        )
        .expect("inline-table update");
        let c: Config = aterm_toml::from_str(&out).unwrap();
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
        // its timing, what the policy took)
        type Row = (
            &'static str,
            Option<&'static str>,
            &'static str,
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
                super::HARNESS_TIMING,
                |p| !p.enabled,
            ),
            (
                super::EDIT_HARNESS_AUTOPILOT,
                None,
                "Land",
                "land",
                super::HARNESS_AUTOPILOT_TIMING,
                |p| p.autopilot == Autopilot::Land && p.enabled,
            ),
            (
                super::EDIT_HARNESS_APPROVE,
                None,
                "safe",
                "safe",
                super::HARNESS_APPROVE_TIMING,
                |p| p.approve == Approve::Safe && p.enabled,
            ),
            (
                super::EDIT_HARNESS_ANSWER_QUESTIONS,
                Some("true"),
                "false",
                "false",
                super::HARNESS_APPROVE_TIMING,
                |p| !p.answer_questions && p.approve == Approve::All,
            ),
            (
                super::EDIT_HARNESS_QUESTION_PRIORITY,
                None,
                "Short-Term",
                "short-term",
                super::HARNESS_APPROVE_TIMING,
                |p| p.question_priority == QuestionPriority::ShortTerm && p.answer_questions,
            ),
        ];
        for (key, absent, typed, stored, timing, took) in rows {
            assert_eq!(super::section_of(key), super::Section::Harness, "{key}");
            assert_eq!(super::environment_precedence(key), None, "{key}");
            assert_eq!(super::application_timing(key), Some(timing), "{key}");
            assert!(super::application_has_live_effect(key), "{key}");
            // Applied when saved: the row paints no "Now", and the card's
            // notes say what its choices mean.
            assert_eq!(super::application_timing_badge(key), None, "{key}");
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

        // NEGATIVE CONTROLS: (the line, its row, the seed, whether it limits).
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

    /// EACH AI CLI'S OWN SWITCH (docs/DESIGN-ai-connections-2026-10-16.md
    /// §10): every connection (`aterm_agent::ai::connector::ids`) has its
    /// `ai.<id>.use` row, labelled "Use <product> on this machine" — USE, never
    /// the harness's "Supervise" — read back to it by [`super::ai_use_switch`],
    /// in the Harness section (its AI CLIs sub-page draws it) and live. SET
    /// `false` writes that CLI's `[ai.<id>]` table, which the AI layer's reader
    /// takes as that CLI unwanted and only that one; the row reads it back;
    /// UNSET leaves no table and the row on again. The preferred CLI and the
    /// look's switch round-trip the same way, and every row seeds from the
    /// RESOLVED value, a fleet document beside `aterm.toml` included, this
    /// machine's own key over it. NEGATIVE CONTROLS: every other CLI's row
    /// stays on, and no other key reads as an AI CLI's switch.
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

        // Seeds from the RESOLVED value: a fleet document beside aterm.toml
        // chooses the preferred CLI and turns Muse Code off; this machine's
        // own key outranks it, and Reset (the key removed) follows the fleet
        // again.
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

    /// EACH AGENT'S OWN SWITCH (`docs/DESIGN-harness-plugins-2026-10-15.md`
    /// §3, §4): every supervisable agent ([`Program::ALL`]) has its
    /// `harness.<agent>.enabled` row in the Harness section, labelled with its
    /// product name and read back to it by [`super::agent_switch`]. SET `false`
    /// writes that agent's `[harness.<agent>]` table, and the supervisor's
    /// reader takes it as that agent switched off and only that agent; the row
    /// reads it back; UNSET is full power again, the row back on. NEGATIVE
    /// CONTROLS: every other agent's row stays on throughout, the master
    /// switch off leaves every agent row on — each row is its agent's table,
    /// not `on_for`, which the master switch row already says — and no other
    /// key reads as an agent's switch.
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
            assert_eq!(
                super::application_timing(key),
                Some(super::HARNESS_AGENT_TIMING),
                "{key}"
            );
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

    /// The `[packages]` switches (2026-09-23: TWO, Automatic updates and the one install
    /// consent): Bool-typed dotted keys, sectioned in the search-only Packages section
    /// (which NO ordinary route owns — the special page renders them), grouped and
    /// searchable, seeded RESOLVED — both default ON, and the retired spellings
    /// (`auto_update`, `seed_install`) fold in. Neither retired key is a row.
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

    /// `[update] enabled` and `[reroute] announce` (2026-09-23): the settings that
    /// replaced `$ATERM_NO_AUTO_UPDATE` and `$ATERM_REROUTE_QUIET` are ordinary Bool rows,
    /// default ON, with no environment precedence to disclose — and the development
    /// settings `[update]` owner/repo are NOT rows.
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

    /// Dotted writes into `[packages]` are NON-DESTRUCTIVE around the keys the
    /// CO-LOCATED atpkg consumes from the same table (account/exclude/links): the
    /// switch flips only its own child — and the RETIRED spelling it replaced, which
    /// would otherwise go on deciding (2026-09-23) — and a blank-revert of the last GUI
    /// key keeps the atpkg-owned siblings, which keep the table.
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
        // stays. OCC face: the dotted child projects as its own value.
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
    /// nothing else: the comment and blank lines written ABOVE it are the
    /// person's, and stay where they were — above the next line, or below a
    /// table's last remaining line.
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
    /// comment lines above the reset key survive the reset AND the undo, which
    /// writes the key back as a new line.
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
            format!("{comments}font_family = \"Menlo\"\nfont_px = 14.0\n")
        );
    }

    /// COMMENTS and unrelated keys survive an edit — the whole point of the
    /// non-destructive (aterm-toml DOM) write vs. a re-serialize.
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
        // The edited keys keep their EXACT line shape — value swapped, the
        // double-space alignment and inline comment intact on the same line.
        assert!(out.contains("font_px = 18.0  # cozy size"), "{out}");
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
        let c: Config = aterm_toml::from_str(&out).expect("typed values round-trip");
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

    /// A `font_px` the resolver would IGNORE at load (outside
    /// `FONT_PX_MIN..=FONT_PX_MAX`, or not finite) is refused with the range,
    /// and the file is untouched — `settings set font_px 0` used to answer
    /// `OK saved`, write `font_px = 0.0`, and change nothing (2026-09-22).
    /// The bounds themselves save, as does every other float key untouched
    /// by the range rule.
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
        let c: Config = aterm_toml::from_str(&out).unwrap();
        assert_eq!(c.cursor_trail_style.as_deref(), Some("phaser"));
        // `fire` likewise — the second look the owner asked about.
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

        // The saved file is a valid EXISTING document for the next Save.
        let again = apply_prefs_edits(&out, &[(super::EDIT_FONT_PX, set("14.0"))])
            .expect("a Windows-path config is editable again, not wedged");
        let cfg: Config = aterm_toml::from_str(&again).expect("second Save re-parses");
        assert_eq!(cfg.font_family_bold.as_deref(), Some(bold));
        assert_eq!(cfg.font_px, Some(14.0));
    }

    /// `editable_fields` seeds CONFIGURED values only (unset = blank), in the documented
    /// row order, with the right keys + kinds — what the window maps to controls.
    #[test]
    fn editable_fields_seed_from_config() {
        let c: Config = aterm_toml::from_str(
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
        // The [machine] row follows the same law: the Enum seeds the configured
        // raw value (blank — "off" is the placeholder, never authored by a default).
        assert_eq!(seed(super::EDIT_MACHINE_UNIVERSAL_CONTROL), None);
        let placeholder = fields
            .iter()
            .find(|f| f.key == super::EDIT_MACHINE_UNIVERSAL_CONTROL)
            .map(|f| f.placeholder.as_str());
        assert_eq!(placeholder, Some("off"));
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
        let range = range_of(super::EDIT_TAB_STRIP_ROWS).expect("tab strip range");
        assert_eq!(range.min, 0.0);
        assert_eq!(
            range.max,
            f64::from(crate::app_config::MAX_TAB_STRIP_ROWS),
            "control and shipping config clamp must agree",
        );
    }

    /// A whitespace-only configured string seeds BLANK (None), matching the display
    /// fallback — so re-saving an untouched window doesn't materialise a "   " value.
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
    /// in the last of them, so it never sits under a row it does not explain.
    #[test]
    fn a_row_draws_its_own_note_and_neighbours_share_one_once() {
        assert_eq!(
            super::row_notes(&[super::EDIT_OPTION_AS_META, super::EDIT_PREDICTIVE_ECHO]),
            vec![None, super::footnote_of(super::EDIT_PREDICTIVE_ECHO)]
        );
        assert_eq!(
            super::row_notes(&[super::EDIT_MINIMUM_CONTRAST, super::EDIT_SELECTION_INACTIVE]),
            vec![super::footnote_of(super::EDIT_MINIMUM_CONTRAST), None],
            "Minimum contrast's note stays in its own row"
        );
        // Each voice says what it sounds like in its own row; the shared
        // note goes with the master and the volume, drawn once, in the last.
        let sound = super::row_notes(&[
            super::EDIT_TRAIL_SOUNDS,
            super::EDIT_TRAIL_SOUND_VOLUME,
            super::EDIT_TONE_MELODY,
            super::EDIT_BELL_SOUND,
        ]);
        assert_eq!(
            sound,
            vec![
                None,
                Some(super::SOUND_NOTE),
                super::footnote_of(super::EDIT_TONE_MELODY),
                super::footnote_of(super::EDIT_BELL_SOUND)
            ],
            "the voices' shared note is drawn once, in the last row that shares it"
        );
        assert_eq!(
            super::row_notes(&[
                super::EDIT_TRAIL_SOUNDS,
                super::EDIT_BELL_SOUND,
                super::EDIT_TRAIL_SOUND_VOLUME,
            ]),
            vec![
                None,
                super::footnote_of(super::EDIT_BELL_SOUND),
                Some(super::SOUND_NOTE)
            ],
            "a row with a note of its own between them does not repeat the shared one"
        );
        for voice in [
            super::EDIT_TRAIL_SOUND_STYLE,
            super::EDIT_TONE_MELODY,
            super::EDIT_TRAIL_SOUND_BED,
            super::EDIT_TRAIL_SOUND_RIFF,
            super::EDIT_CHOICE_SOUND,
            super::EDIT_SPARKLE_BONK,
            super::EDIT_SPARKLE_BONK_DETONATION,
            super::EDIT_OUTPUT_STREAK_SOUND,
        ] {
            let note = super::footnote_of(voice);
            assert!(
                note.is_some_and(|note| note != super::SOUND_NOTE),
                "{voice} says what it sounds like: {note:?}"
            );
        }
        assert_eq!(
            super::row_notes(&[super::EDIT_PACKAGES_ENABLED]),
            vec![super::footnote_of(super::EDIT_PACKAGES_ENABLED)],
            "Modified listing one ALab tools switch explains only that switch"
        );
        assert_eq!(
            super::footnote_of(EDIT_FOREGROUND),
            None,
            "the field says Theme default"
        );
    }
}

/// REGISTRY conformance: the proofs behind Manual and `settings set|unset` (the
/// AI-driveability contract) — the dotted-key nested-table writer and the
/// list-row round-trips.
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

    /// AN INTEGER REFUSAL NAMES THE BOUND IT BROKE, in the words the Settings
    /// row uses, so `settings set` says what to type instead of only "invalid
    /// value". A whole number too long for i64 is still a whole number: its
    /// sign says which end it passed.
    #[test]
    fn an_integer_refusal_names_the_bound_it_broke() {
        for (key, raw, words) in [
            (EDIT_COLUMNS, "70000", "must be at most 65,535"),
            (EDIT_COLUMNS, "-1", "must be 0 or more"),
            (
                EDIT_COLUMNS,
                "wide",
                "must be a whole number from 0 to 65,535",
            ),
            (
                EDIT_COLUMNS,
                "99999999999999999999",
                "must be at most 65,535",
            ),
            (
                EDIT_SCROLLBACK,
                "9223372036854775808",
                "must be at most 9,223,372,036,854,775,807",
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
                "must be at least -128",
            ),
            (
                "sparkle_words.profanity.jitter",
                "200",
                "must be at most 127",
            ),
            (
                EDIT_ADJUST_BASELINE,
                "-9223372036854775809",
                "must be at least -9,223,372,036,854,775,808",
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

    /// Every [`EditKind::Integer`] key enforces its declared writable integer
    /// domain at Save: normally the serde field's actual domain, with narrower
    /// operational-policy domains for the tab-status controls. `-1` is rejected
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
        // CREATE: two depths on an empty file, then serde reads them back.
        let out = apply_prefs_edits(
            "",
            &[
                ("update.auto_apply", set("false")),
                ("sparkle_words.profanity.enabled", set("false")),
            ],
        )
        .unwrap();
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
    /// removing through a missing table is a clean no-op, and a NON-table
    /// intermediate refuses the edit (Parse error, file untouched) instead of
    /// clobbering user data.
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
        // A scalar squatting on the table name refuses the write (never doc[..] panics).
        let err =
            apply_prefs_edits("update = 5\n", &[("update.auto_apply", set("false"))]).unwrap_err();
        assert!(matches!(err, PrefsEditError::Parse(_)), "{err}");
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
            super::EDIT_RESTORE_SESSION,
            super::EDIT_TEMPORAL_RECORDING,
            super::EDIT_TRAIL_SOUND_BED,
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
        // The ambient-bed opt-in writes a real TOML bool and resolves ON.
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
        assert_eq!(c.font_family_request().as_deref(), Some("display:pixel"));
        // A MIX round-trips through the writer in canonical joined form, and
        // the resolver canonicalizes it into the `display:` mix family.
        let out = apply_prefs_edits("", &[(super::EDIT_DISPLAY_FONT, set(" pixel + engraved "))])
            .unwrap();
        let mixed: Config = aterm_toml::from_str(&out).unwrap();
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
}
