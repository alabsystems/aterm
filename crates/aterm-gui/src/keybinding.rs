// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! User-rebindable keyboard shortcuts (config `[keybindings]`).
//!
//! Today the app commands (new/close tab, switch tab, split, copy/paste, find,
//! font zoom, new window) are HARDCODED in [`crate::App::on_key`]. This module
//! makes them config-driven WITHOUT regressing the hardcoded path: a
//! `[keybindings]` TOML table maps chord strings (`"cmd+shift+t"`, `"ctrl+a"`)
//! to [`Action`] names, parsed once at load into a `HashMap<Chord, Action>`.
//!
//! `on_key` consults this map FIRST with an O(1) lookup. A **miss** falls
//! through to the existing hardcoded matches, so an empty table (the default
//! when no config is present) costs one hash probe and changes nothing — the
//! suite stays byte-identical. A malformed chord/action is WARNED and SKIPPED
//! (fail-open to defaults), never aborting the launch.
//!
//! The chord representation is intentionally tiny and platform-neutral: a
//! 4-bit modifier mask (cmd/ctrl/alt/shift) plus a normalized [`KeyToken`]
//! (a lowercased character, a digit, or a named key). It is built identically
//! from a parsed config string ([`Chord::parse`]) and from a live winit key
//! event ([`Chord::from_event`]), so a binding the user wrote matches the key
//! they press.

use std::collections::HashMap;

use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

/// Modifier mask bits for a [`Chord`]. A small hand-rolled bitset (rather than
/// pulling in `bitflags`) keeps this module dependency-free; the four bits are
/// the only modifiers a binding can name.
const MOD_CMD: u8 = 1 << 0;
const MOD_CTRL: u8 = 1 << 1;
const MOD_ALT: u8 = 1 << 2;
const MOD_SHIFT: u8 = 1 << 3;

/// The key portion of a [`Chord`], normalized so a config string and a live key
/// event compare equal. Characters are folded to lowercase (so `"T"` and `"t"`
/// name the same physical key; SHIFT is carried separately in the mask).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeyToken {
    /// A printable character key, lowercased (`a`, `=`, `[`).
    Char(char),
    /// A named non-printable key (Enter, Tab, Escape, F1, arrows, …).
    Named(NamedKey),
}

/// A normalized keyboard chord: a modifier mask plus a [`KeyToken`]. Two chords
/// are equal iff they name the same modifiers and key, regardless of how they
/// were spelled (`"cmd+T"` == `"shift+cmd+t"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    mods: u8,
    key: KeyToken,
}

/// An app command a chord can be bound to. Each variant maps 1:1 to an existing
/// hardcoded `on_key` behavior, so a binding does EXACTLY what the built-in key
/// did (no new capability, just a configurable trigger).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Open a new in-window tab (Cmd-T).
    NewTab,
    /// Reopen the most recently closed native tab (Cmd-Shift-T on macOS).
    ReopenClosedTab,
    /// Close the focused pane / tab (Cmd-W).
    CloseTab,
    /// Open a new window — a fresh process (Cmd-N).
    NewWindow,
    /// Cycle to the next tab (Cmd-Shift-]).
    NextTab,
    /// Cycle to the previous tab (Cmd-Shift-[).
    PrevTab,
    /// Switch to tab `n` (1-based, as the user wrote it; Cmd-1..Cmd-9).
    SwitchTab(u8),
    /// Split the focused pane vertically — side by side (Cmd-D).
    SplitVertical,
    /// Split the focused pane horizontally — stacked (Cmd-Shift-D).
    SplitHorizontal,
    /// Copy the selection to the clipboard (Cmd-C).
    Copy,
    /// Paste the clipboard (Cmd-V).
    Paste,
    /// Enter Cmd-F find mode.
    Find,
    /// Grow the live font (Cmd-=).
    FontIncrease,
    /// Reset the font to the launch default (Cmd-0).
    FontReset,
    /// Shrink the live font (Cmd--).
    FontDecrease,
    /// Move keyboard focus to the pane on the left (Cmd-Opt-Left).
    FocusPaneLeft,
    /// Move keyboard focus to the pane on the right (Cmd-Opt-Right).
    FocusPaneRight,
    /// Move keyboard focus to the pane above (Cmd-Opt-Up).
    FocusPaneUp,
    /// Move keyboard focus to the pane below (Cmd-Opt-Down).
    FocusPaneDown,
    /// Toggle zoom of the focused pane — fill the window (Cmd-Shift-Enter).
    TogglePaneZoom,
    /// Scroll the viewport back one page (default Shift-PageUp).
    ScrollPageUp,
    /// Scroll the viewport forward one page (default Shift-PageDown).
    ScrollPageDown,
    /// Scroll the viewport back one line.
    ScrollLineUp,
    /// Scroll the viewport forward one line.
    ScrollLineDown,
    /// Jump the viewport to the oldest scrollback line (default Shift-Home).
    ScrollToTop,
    /// Jump the viewport back to the live bottom (default Shift-End).
    ScrollToBottom,
    /// Jump the viewport to the PREVIOUS (older) shell prompt — the nearest
    /// OSC-133 command mark above the top visible row (default Ctrl-Shift-Up off
    /// macOS; bindable via `[keybindings]` on macOS). Requires shell integration;
    /// inert without marks.
    JumpPrevPrompt,
    /// Jump the viewport to the NEXT (newer) shell prompt (default
    /// Ctrl-Shift-Down off macOS; bindable via `[keybindings]` on macOS).
    JumpNextPrompt,
    /// Focus or create the native Settings tab (default Ctrl+Shift+S off macOS).
    ToggleSettings,
    /// Open the native Settings app's About route (default Ctrl+Shift+A off macOS,
    /// where the app menu carries "About aterm").
    ToggleAbout,
    /// Toggle the sparkle-words master kill (`App::toggle_sparkle_words`) — an
    /// instant panic-off that overrides config without a TOML edit. Unbound by
    /// default; bind it in `[keybindings]`.
    ToggleSparkleWords,
    /// Toggle PHOSPHOR matrix rain for the FRONT SESSION of the focused window
    /// (`App::toggle_matrix_rain`) — a per-session runtime override that wins
    /// over the `[matrix_rain]` config bit in EITHER direction (it can start
    /// rain over a disabled config, unlike the old app-global kill latch), and
    /// toggling the session you're looking at is still the instant panic-off.
    /// Also reachable via View ▸ Matrix Rain / the palette / `aterm-ctl rain`.
    /// Unbound by default; bind it in `[keybindings]`.
    ToggleMatrixRain,
    /// Toggle process-wide serious mode, suppressing sound, trails, and all
    /// decorative effects while preserving their requested settings.
    ToggleSeriousMode,
    /// Toggle the own-rendered, cross-platform command PALETTE overlay
    /// (`App::toggle_palette`; default Ctrl+Shift+P off macOS, ⇧⌘P via the menu on macOS).
    OpenPalette,
    /// Toggle vi (keyboard copy-mode) on the focused pane (VI-1): a keyboard-driven
    /// cursor for motion + selection over the screen and scrollback, no chord bound by
    /// default — bind it in `[keybindings]` (e.g. `ctrl+shift+space = "toggle_vi_mode"`).
    ToggleViMode,
}

/// Every bindable action NAME, in a stable order — the canonical discoverable
/// surface for `[keybindings]` config (printed by `--list-actions`). `switch_tab_N`
/// is parameterized (`switch_tab_1`..`switch_tab_9`), shown here as the template.
/// A test asserts every concrete name here parses, so this cannot drift from
/// [`Action::parse`].
pub(crate) const ACTION_NAMES: &[&str] = &[
    "new_tab",
    "reopen_closed_tab",
    "close_tab",
    "new_window",
    "next_tab",
    "prev_tab",
    "switch_tab_1..switch_tab_9",
    "split_vertical",
    "split_horizontal",
    "focus_pane_left",
    "focus_pane_right",
    "focus_pane_up",
    "focus_pane_down",
    "toggle_pane_zoom",
    "copy",
    "paste",
    "find",
    "font_increase",
    "font_decrease",
    "font_reset",
    "scroll_page_up",
    "scroll_page_down",
    "scroll_line_up",
    "scroll_line_down",
    "scroll_to_top",
    "scroll_to_bottom",
    "jump_prev_prompt",
    "jump_next_prompt",
    "toggle_settings",
    "toggle_about",
    "toggle_sparkle_words",
    "toggle_matrix_rain",
    "toggle_serious_mode",
    "open_palette",
    "toggle_vi_mode",
];

/// Built-in Cmd-* shortcuts hardcoded in `App::on_key` + its helpers, as
/// (chord-string, human label). SINGLE source of truth: drives BOTH the shadow
/// detector (diagnostics) AND the `--list-keybinds` built-in section, so the
/// documented set cannot drift from detection. Each chord parses via `Chord::parse`
/// (a test asserts it) in the SAME normalized (base-key + mods) space
/// `Chord::from_event` produces, so a match here == a runtime interception. `cmd+=`
/// and `cmd+shift+=` both map to Font Increase (zoom matches `=`|`+`, no shift gate).
pub(crate) const BUILTIN_CMD_CHORDS: &[(&str, &str)] = &[
    ("cmd+c", "Copy"),
    ("cmd+v", "Paste"),
    ("cmd+f", "Find"),
    ("cmd+s", "Search Forward"),
    ("cmd+r", "Search Backward"),
    ("cmd+n", "New Window"),
    ("cmd+t", "New Tab"),
    ("cmd+w", "Close Tab"),
    ("cmd+d", "Split Vertical"),
    ("cmd+shift+d", "Split Horizontal"),
    ("cmd+shift+]", "Next Tab"),
    ("cmd+shift+[", "Prev Tab"),
    ("cmd+shift+enter", "Toggle Pane Zoom"),
    ("cmd+alt+left", "Focus Pane Left"),
    ("cmd+alt+right", "Focus Pane Right"),
    ("cmd+alt+up", "Focus Pane Up"),
    ("cmd+alt+down", "Focus Pane Down"),
    ("cmd+=", "Font Increase"),
    ("cmd+shift+=", "Font Increase"),
    ("cmd+-", "Font Decrease"),
    ("cmd+0", "Font Reset"),
    ("cmd+1", "Switch to Tab 1"),
    ("cmd+2", "Switch to Tab 2"),
    ("cmd+3", "Switch to Tab 3"),
    ("cmd+4", "Switch to Tab 4"),
    ("cmd+5", "Switch to Tab 5"),
    ("cmd+6", "Switch to Tab 6"),
    ("cmd+7", "Switch to Tab 7"),
    ("cmd+8", "Switch to Tab 8"),
    ("cmd+9", "Switch to Tab 9"),
];

/// If `chord_str` names a built-in Cmd shortcut, return the built-in label it conflicts
/// with (NOTE: macOS menu shortcuts are claimed by the menu, so the rule is shadowed BY
/// the built-in, not the reverse); else
/// `None`. Compares NORMALIZED chords (so `cmd+C` / `shift+cmd+…` spellings agree),
/// and `None` for unparseable input. Used by BOTH `--validate-config` and the
/// `--list-keybinds` inline note so they can't disagree.
#[must_use]
pub(crate) fn builtin_shadow_label(chord_str: &str) -> Option<&'static str> {
    let target = Chord::parse(chord_str).ok()?;
    BUILTIN_CMD_CHORDS
        .iter()
        .find(|(c, _)| Chord::parse(c).is_ok_and(|c| c == target))
        .map(|(_, label)| *label)
}

/// True if `chord_str` is ALSO bound in `[keybindings]` — which `on_key` consults
/// FIRST, so the keybinding wins and a `[key_sequences]` rule on the same chord never
/// fires. Compares normalized chords; unparseable input (flagged elsewhere) is false.
#[must_use]
pub(crate) fn chord_in_keybindings(
    chord_str: &str,
    keybindings: &std::collections::BTreeMap<String, String>,
) -> bool {
    let Ok(target) = Chord::parse(chord_str) else {
        return false;
    };
    keybindings
        .keys()
        .any(|k| Chord::parse(k).is_ok_and(|c| c == target))
}

impl Action {
    /// Parse an action NAME (the value side of a `[keybindings]` entry). Names are
    /// lowercase, `snake_case`, and stable; `switch_tab_<n>` carries the 1-based
    /// target. Returns `None` for an unknown name so the loader can warn + skip.
    #[must_use]
    pub fn parse(name: &str) -> Option<Action> {
        let n = name.trim();
        if let Some(rest) = n.strip_prefix("switch_tab_") {
            // 1..=9 only — matches the hardcoded Cmd-1..Cmd-9 range.
            let idx: u8 = rest.parse().ok()?;
            return (1..=9).contains(&idx).then_some(Action::SwitchTab(idx));
        }
        Some(match n {
            "new_tab" => Action::NewTab,
            "reopen_closed_tab" => Action::ReopenClosedTab,
            "close_tab" => Action::CloseTab,
            "new_window" => Action::NewWindow,
            "next_tab" => Action::NextTab,
            "prev_tab" => Action::PrevTab,
            "split_vertical" => Action::SplitVertical,
            "split_horizontal" => Action::SplitHorizontal,
            "focus_pane_left" => Action::FocusPaneLeft,
            "focus_pane_right" => Action::FocusPaneRight,
            "focus_pane_up" => Action::FocusPaneUp,
            "focus_pane_down" => Action::FocusPaneDown,
            "toggle_pane_zoom" => Action::TogglePaneZoom,
            "copy" => Action::Copy,
            "paste" => Action::Paste,
            "find" => Action::Find,
            "font_increase" => Action::FontIncrease,
            "font_decrease" => Action::FontDecrease,
            "font_reset" => Action::FontReset,
            "scroll_page_up" => Action::ScrollPageUp,
            "scroll_page_down" => Action::ScrollPageDown,
            "scroll_line_up" => Action::ScrollLineUp,
            "scroll_line_down" => Action::ScrollLineDown,
            "scroll_to_top" => Action::ScrollToTop,
            "scroll_to_bottom" => Action::ScrollToBottom,
            "jump_prev_prompt" => Action::JumpPrevPrompt,
            "jump_next_prompt" => Action::JumpNextPrompt,
            "toggle_settings" => Action::ToggleSettings,
            "toggle_about" => Action::ToggleAbout,
            "toggle_sparkle_words" => Action::ToggleSparkleWords,
            "toggle_matrix_rain" => Action::ToggleMatrixRain,
            "toggle_serious_mode" => Action::ToggleSeriousMode,
            "open_palette" => Action::OpenPalette,
            "toggle_vi_mode" => Action::ToggleViMode,
            _ => return None,
        })
    }
}

/// Map a config modifier word to its mask bit. Accepts the common aliases so a
/// user can write `cmd`/`super`/`win`, `opt`/`option`/`alt`/`meta`, `ctrl`/
/// `control`. Returns `None` for an unknown word (the chord is then skipped).
fn modifier_bit(word: &str) -> Option<u8> {
    Some(match word {
        "cmd" | "command" | "super" | "win" | "meta" => MOD_CMD,
        "ctrl" | "control" => MOD_CTRL,
        "alt" | "opt" | "option" => MOD_ALT,
        "shift" => MOD_SHIFT,
        _ => return None,
    })
}

/// Map a config key word (the final `+`-segment) to a [`KeyToken`]. A single
/// character is a `Char`; a multi-letter word is matched against the named-key
/// table (Enter, Tab, arrows, F-keys, …). Returns `None` for an unknown name.
fn key_token(word: &str) -> Option<KeyToken> {
    let mut chars = word.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        // Single character: fold to lowercase so case is carried only by SHIFT.
        return Some(KeyToken::Char(first.to_ascii_lowercase()));
    }
    let named = match word {
        "enter" | "return" => NamedKey::Enter,
        "tab" => NamedKey::Tab,
        "space" => NamedKey::Space,
        "escape" | "esc" => NamedKey::Escape,
        "backspace" => NamedKey::Backspace,
        "delete" | "del" => NamedKey::Delete,
        "insert" | "ins" => NamedKey::Insert,
        "up" => NamedKey::ArrowUp,
        "down" => NamedKey::ArrowDown,
        "left" => NamedKey::ArrowLeft,
        "right" => NamedKey::ArrowRight,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        "pageup" | "pgup" => NamedKey::PageUp,
        "pagedown" | "pgdn" => NamedKey::PageDown,
        "f1" => NamedKey::F1,
        "f2" => NamedKey::F2,
        "f3" => NamedKey::F3,
        "f4" => NamedKey::F4,
        "f5" => NamedKey::F5,
        "f6" => NamedKey::F6,
        "f7" => NamedKey::F7,
        "f8" => NamedKey::F8,
        "f9" => NamedKey::F9,
        "f10" => NamedKey::F10,
        "f11" => NamedKey::F11,
        "f12" => NamedKey::F12,
        _ => return None,
    };
    Some(KeyToken::Named(named))
}

impl Chord {
    /// Parse a chord STRING (the key side of a `[keybindings]` entry), e.g.
    /// `"cmd+shift+t"` or `"ctrl+a"`. Segments are split on `+`, lowercased, and
    /// trimmed; every segment but the LAST is a modifier and the last is the key.
    /// Returns `Err` (a human-readable reason) for an empty string, an unknown
    /// modifier/key, a duplicate or missing key, so the loader can warn + skip.
    pub fn parse(s: &str) -> Result<Chord, String> {
        let lower = s.trim().to_ascii_lowercase();
        if lower.is_empty() {
            return Err("empty chord".to_string());
        }
        let mut mods = 0u8;
        let mut key: Option<KeyToken> = None;
        for seg in lower.split('+') {
            let seg = seg.trim();
            if seg.is_empty() {
                return Err(format!("empty segment in {s:?}"));
            }
            if key.is_some() {
                // A token after the key segment means the key wasn't last.
                return Err(format!("modifier after key in {s:?}"));
            }
            if let Some(bit) = modifier_bit(seg) {
                mods |= bit;
            } else if let Some(tok) = key_token(seg) {
                key = Some(tok);
            } else {
                return Err(format!("unknown chord segment {seg:?} in {s:?}"));
            }
        }
        let key = key.ok_or_else(|| format!("chord {s:?} has no key"))?;
        Ok(Chord { mods, key })
    }

    /// Build the chord a live winit key event represents, for the O(1) lookup in
    /// `on_key`. `logical` is the modifier-independent logical key
    /// (`key_without_modifiers` on macOS) so a binding written as the BASE key
    /// (`cmd+t`) matches even when the OS composed a different glyph. Returns
    /// `None` for a bare modifier press or an unmappable key (which can never be
    /// a binding target).
    #[must_use]
    pub fn from_event(logical: &WinitKey, mods: ModifiersState) -> Option<Chord> {
        let key = match logical {
            WinitKey::Character(s) => {
                let mut chars = s.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None; // multi-char (composed) string is not a chord key
                }
                KeyToken::Char(c.to_ascii_lowercase())
            }
            // A bare modifier key (Shift/Ctrl/Alt/Super/CapsLock) is never a
            // binding TARGET — it is carried in the mask, not as the key — so a
            // modifier-only event is not a chord (it always misses the lookup).
            WinitKey::Named(
                NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::CapsLock,
            ) => return None,
            WinitKey::Named(named) => KeyToken::Named(*named),
            _ => return None,
        };
        let mut mask = 0u8;
        if mods.super_key() {
            mask |= MOD_CMD;
        }
        if mods.control_key() {
            mask |= MOD_CTRL;
        }
        if mods.alt_key() {
            mask |= MOD_ALT;
        }
        if mods.shift_key() {
            mask |= MOD_SHIFT;
        }
        Some(Chord { mods: mask, key })
    }
}

/// The parsed `[keybindings]` table: a chord → action map consulted at the top of
/// `on_key`. Empty (the default with no config) means the lookup is one hash
/// probe that always misses, so the hardcoded path runs unchanged.
#[derive(Clone, Debug, Default)]
pub struct Keybindings {
    map: HashMap<Chord, Action>,
}

impl Keybindings {
    /// Collect the parsed map PLUS a per-entry WARNING string (unprefixed) for each
    /// skipped chord/action. Shared by [`Self::from_config`] (which eprintln!s them)
    /// and [`Self::from_config_warn`] (which returns them for an in-window notice).
    fn collect(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (HashMap<Chord, Action>, Vec<String>) {
        let mut map = HashMap::new();
        let mut warns = Vec::new();
        if let Some(table) = table {
            for (chord_str, action_str) in table {
                let chord = match Chord::parse(chord_str) {
                    Ok(c) => c,
                    Err(e) => {
                        warns.push(format!("config keybindings: skipping {chord_str:?}: {e}"));
                        continue;
                    }
                };
                let Some(action) = Action::parse(action_str) else {
                    warns.push(format!(
                        "config keybindings: skipping {chord_str:?}: unknown action {action_str:?}"
                    ));
                    continue;
                };
                map.insert(chord, action);
            }
        }
        (map, warns)
    }

    /// Build from the raw config table (chord-string → action-name). Each entry
    /// is parsed independently; a malformed chord OR an unknown action is WARNED
    /// to stderr and SKIPPED, so one bad line never disables the rest and the app
    /// always falls open to the hardcoded defaults. `None`/empty input yields an
    /// empty map (zero behavioral change).
    // Stderr-only ctor: every production caller now uses the `*_warn` variant (for the
    // in-window notice), so this is exercised only by tests — allow dead_code on a plain
    // (non-test) build rather than churn the test call-sites onto `*_warn(..).0`.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn from_config(table: Option<&std::collections::BTreeMap<String, String>>) -> Keybindings {
        let (map, warns) = Self::collect(table);
        for w in &warns {
            eprintln!("aterm-gui: {w}");
        }
        Keybindings { map }
    }

    /// Like [`Self::from_config`] but RETURNS the warnings instead of printing them, so
    /// the GUI can surface dropped rules in an in-window notice (stderr is invisible to
    /// a Finder-launched .app). The map is byte-identical to `from_config`.
    #[must_use]
    pub fn from_config_warn(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (Keybindings, Vec<String>) {
        let (map, warns) = Self::collect(table);
        (Keybindings { map }, warns)
    }

    /// The platform's BUILT-IN default keybindings. macOS ships an EMPTY table — the
    /// hardcoded Cmd-* chords in `on_key` ARE the convention there, so the no-config
    /// path stays byte-identical. Every other platform (Linux/X11) seeds the standard
    /// terminal chords, because there is no Cmd key and the hardcoded chords gate on
    /// the physical Super (Windows) key, which desktop environments routinely grab —
    /// leaving a fresh user with no working keystroke to copy, paste, or open a tab.
    /// User `[keybindings]` entries are overlaid on top (see [`Self::resolved`]), so
    /// any of these can be rebound.
    #[must_use]
    pub fn platform_defaults() -> Keybindings {
        // (chord, action) pairs parsed through the SAME machinery as user config, so
        // a default can never diverge from what a user could write. macOS = none.
        #[cfg(target_os = "macos")]
        let pairs: &[(&str, &str)] = &[];
        #[cfg(not(target_os = "macos"))]
        let pairs: &[(&str, &str)] = &[
            ("ctrl+shift+c", "copy"),
            ("ctrl+shift+v", "paste"),
            // The Windows-native muscle memory (Windows Terminal defaults): plain
            // Ctrl+V pastes — the deliberate WT tradeoff (it shadows bash's
            // quoted-insert ^V; rebindable) — plus the classic Insert pair.
            ("ctrl+v", "paste"),
            ("shift+insert", "paste"),
            ("ctrl+insert", "copy"),
            ("ctrl+shift+t", "new_tab"),
            ("ctrl+shift+w", "close_tab"),
            ("ctrl+shift+n", "new_window"),
            ("ctrl+shift+f", "find"),
            ("ctrl+shift+d", "split_vertical"),
            ("ctrl+shift+e", "split_horizontal"),
            ("ctrl+shift+return", "toggle_pane_zoom"),
            ("ctrl+shift+right", "next_tab"),
            ("ctrl+shift+left", "prev_tab"),
            ("ctrl+pagedown", "next_tab"),
            ("ctrl+pageup", "prev_tab"),
            // Jump-to-prompt (OSC-133 marks): the WezTerm/iTerm2 chord. Up/Down
            // are free here (left/right are tab nav), so no shadow.
            ("ctrl+shift+up", "jump_prev_prompt"),
            ("ctrl+shift+down", "jump_next_prompt"),
            // Zoom: Ctrl+= and Ctrl+Shift+= (the latter is "Ctrl++" on most layouts).
            ("ctrl+=", "font_increase"),
            ("ctrl+shift+=", "font_increase"),
            ("ctrl+-", "font_decrease"),
            ("ctrl+0", "font_reset"),
            ("ctrl+shift+s", "toggle_settings"),
            ("ctrl+shift+a", "toggle_about"),
            ("ctrl+shift+p", "open_palette"),
            // NOTE: jump-to-tab-N is intentionally NOT seeded. The GNOME-Terminal
            // Alt+1..9 convention would shadow readline/emacs/vim META-DIGIT numeric
            // arguments (Alt+digit), a real regression for TUI users — and it fires
            // BEFORE the PTY encoder, so the digit never reaches the app. Tab nav is
            // covered by next/prev (Ctrl+Shift+Left/Right, Ctrl+PageUp/Down); users
            // who want N-jump can bind switch_tab_N themselves in [keybindings].
        ];
        let mut map = HashMap::new();
        for (chord_str, action_str) in pairs {
            // These are compile-time constants; a parse failure is a build-time bug,
            // so assert in debug yet fall open (skip) in release rather than panic.
            match (Chord::parse(chord_str), Action::parse(action_str)) {
                (Ok(c), Some(a)) => {
                    map.insert(c, a);
                }
                _ => debug_assert!(
                    false,
                    "built-in default keybinding {chord_str:?} is malformed"
                ),
            }
        }
        Keybindings { map }
    }

    /// The EFFECTIVE keybindings the running app uses: the platform built-in
    /// defaults ([`Self::platform_defaults`]) with the user's `[keybindings]` table
    /// overlaid on top (a user entry for the same chord WINS). This is what
    /// `App::new` installs — distinct from [`Self::from_config`], which returns ONLY
    /// the parsed config (no defaults) and backs the config-parsing tests.
    #[cfg_attr(not(test), allow(dead_code))] // test-only now; App::new uses resolved_warn
    #[must_use]
    pub fn resolved(table: Option<&std::collections::BTreeMap<String, String>>) -> Keybindings {
        let mut kb = Keybindings::platform_defaults();
        for (chord, action) in Keybindings::from_config(table).map {
            kb.map.insert(chord, action);
        }
        kb
    }

    /// Like [`Self::resolved`] but RETURNS the user-table warnings (the platform
    /// defaults are compile-time-checked and never warn). For the GUI config notice.
    #[must_use]
    pub fn resolved_warn(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (Keybindings, Vec<String>) {
        let mut kb = Keybindings::platform_defaults();
        let (user, warns) = Keybindings::from_config_warn(table);
        for (chord, action) in user.map {
            kb.map.insert(chord, action);
        }
        (kb, warns)
    }

    /// Whether NO bindings are configured (the default). `on_key` can skip even
    /// the chord-build when this is true, keeping the no-config path allocation-
    /// and probe-free.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// O(1) lookup: the [`Action`] bound to a live key event, or `None` (miss →
    /// fall through to the hardcoded `on_key` matches).
    #[must_use]
    pub fn lookup(&self, logical: &WinitKey, mods: ModifiersState) -> Option<Action> {
        if self.map.is_empty() {
            return None;
        }
        let chord = Chord::from_event(logical, mods)?;
        self.map.get(&chord).copied()
    }
}

/// A user `[key_sequences]` table: a chord -> RAW BYTES map. This is aterm's
/// terminal-side INPUT POLICY — an override that makes a chord send exactly the
/// bytes the user chose, regardless of what the running app negotiated. It is
/// consulted in `on_key` AFTER `[keybindings]` (app actions) and BEFORE the default
/// key encoder, so an explicit rule always wins. Empty (the default) is a no-op.
///
/// The built-in `Shift+Enter -> LF` lives in the legacy ENCODER (legacy-only, so a
/// protocol-aware app still gets `ESC[13;2u`); a `[key_sequences]` entry for the
/// same chord overrides it unconditionally.
#[derive(Clone, Debug, Default)]
pub struct KeySequences {
    map: HashMap<Chord, Vec<u8>>,
}

impl KeySequences {
    /// Build from the raw config table (chord-string -> byte-string). Each value's
    /// `\n \r \t \e \0 \a \b \f \v \\ \xNN \u{NNNN}` escapes are expanded to bytes,
    /// and any other char contributes its UTF-8 bytes — so BOTH a TOML basic string
    /// (`"\n"`, `"[A"`) and a TOML literal string (`'\e[A'`) work. A malformed
    /// chord OR byte-string is WARNED + SKIPPED (fail-open), like `[keybindings]`.
    #[cfg_attr(not(test), allow(dead_code))] // stderr-only ctor; prod uses from_config_warn
    #[must_use]
    pub fn from_config(table: Option<&std::collections::BTreeMap<String, String>>) -> KeySequences {
        let (map, warns) = Self::collect(table);
        for w in &warns {
            eprintln!("aterm-gui: {w}");
        }
        KeySequences { map }
    }

    /// Like [`Self::from_config`] but RETURNS the warnings instead of printing them
    /// (for the GUI's in-window config notice). The map is byte-identical.
    #[must_use]
    pub fn from_config_warn(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (KeySequences, Vec<String>) {
        let (map, warns) = Self::collect(table);
        (KeySequences { map }, warns)
    }

    /// Collect the parsed map PLUS a per-entry WARNING string (unprefixed) for each
    /// skipped chord/value. Shared by `from_config` (eprintln) and `from_config_warn`.
    fn collect(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (HashMap<Chord, Vec<u8>>, Vec<String>) {
        let mut map = HashMap::new();
        let mut warns = Vec::new();
        if let Some(table) = table {
            for (chord_str, bytes_str) in table {
                let chord = match Chord::parse(chord_str) {
                    Ok(c) => c,
                    Err(e) => {
                        warns.push(format!("config key_sequences: skipping {chord_str:?}: {e}"));
                        continue;
                    }
                };
                match parse_byte_sequence(bytes_str) {
                    Ok(bytes) if bytes.is_empty() => {
                        // Empty value -> zero bytes -> silently dead-keys the chord;
                        // almost always a typo, so warn + skip (fail-open).
                        warns.push(format!(
                            "config key_sequences: skipping {chord_str:?}: \
                             empty value would silently disable the key"
                        ));
                    }
                    Ok(bytes) => {
                        map.insert(chord, bytes);
                    }
                    Err(e) => {
                        warns.push(format!("config key_sequences: skipping {chord_str:?}: {e}"));
                    }
                }
            }
        }
        (map, warns)
    }

    /// Whether no sequences are configured (the default) — lets `on_key` skip the
    /// chord-build entirely on the hot path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The raw bytes a live key event is bound to, or `None` (miss -> fall through to
    /// the default encoder).
    #[must_use]
    pub fn lookup(&self, logical: &WinitKey, mods: ModifiersState) -> Option<&[u8]> {
        if self.map.is_empty() {
            return None;
        }
        let chord = Chord::from_event(logical, mods)?;
        self.map.get(&chord).map(Vec::as_slice)
    }
}

/// The outcome of consulting the two user chord maps for a key event. Captures MAP
/// PRECEDENCE ONLY: `[keybindings]` (app actions) win over `[key_sequences]` (raw-byte
/// overrides); an unbound chord is `FallThrough`. The match-arm ORDERING against the
/// hardcoded Cmd shortcuts, and the find-overlay gate, are policy that `App::on_key`
/// owns — this function structurally cannot see them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChordResolution {
    Action(Action),
    Sequence(Vec<u8>),
    FallThrough,
}

/// Resolve a live key event against the user's two chord maps, applying the
/// keybindings-before-key_sequences precedence. Pure: reuses each map's own
/// empty-guarded O(1) `lookup`. See [`ChordResolution`] for what it does NOT decide.
#[must_use]
pub(crate) fn resolve_chord(
    base: &WinitKey,
    mods: ModifiersState,
    kb: &Keybindings,
    ks: &KeySequences,
) -> ChordResolution {
    if let Some(action) = kb.lookup(base, mods) {
        return ChordResolution::Action(action);
    }
    if let Some(bytes) = ks.lookup(base, mods) {
        return ChordResolution::Sequence(bytes.to_vec());
    }
    ChordResolution::FallThrough
}

/// Upper bound on a single `[key_sequences]` value, in bytes. A key chord never
/// legitimately sends a kilobyte; the cap keeps the SYNCHRONOUS PTY write the seam
/// performs for a mapped chord bounded — a multi-KB value would otherwise block the
/// winit event loop until the ~8 KiB tty buffer drains (the hazard the paste path
/// mitigates with `MAX_PASTE_BYTES` + a detached thread; here a small cap suffices
/// since the bytes come from the user's own config, not an external stream).
pub(crate) const MAX_KEY_SEQUENCE_BYTES: usize = 1024;

/// Expand a config byte-string (a `[key_sequences]` value) into the exact bytes to
/// send to the PTY. Backslash escapes are interpreted so a user can write control
/// bytes in a TOML *literal* string (single quotes, no TOML escape processing); a
/// TOML *basic* string that already produced a control char passes it through as its
/// UTF-8 bytes. Returns `Err(reason)` for a malformed escape or an oversized value.
pub(crate) fn parse_byte_sequence(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    let mut buf = [0u8; 4];
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        let esc = chars.next().ok_or("dangling backslash")?;
        match esc {
            'n' => out.push(0x0a),
            'r' => out.push(0x0d),
            't' => out.push(0x09),
            'e' => out.push(0x1b),
            '0' => out.push(0x00),
            'a' => out.push(0x07),
            'b' => out.push(0x08),
            'f' => out.push(0x0c),
            'v' => out.push(0x0b),
            '\\' => out.push(b'\\'),
            '"' => out.push(b'"'),
            '\'' => out.push(b'\''),
            'x' => {
                let h1 = chars.next().ok_or("\\x needs two hex digits")?;
                let h2 = chars.next().ok_or("\\x needs two hex digits")?;
                let hi = h1
                    .to_digit(16)
                    .ok_or_else(|| format!("bad hex digit {h1:?} after \\x"))?;
                let lo = h2
                    .to_digit(16)
                    .ok_or_else(|| format!("bad hex digit {h2:?} after \\x"))?;
                out.push((hi * 16 + lo) as u8);
            }
            'u' => {
                // \u{HEX} — a Unicode scalar, emitted as UTF-8.
                if chars.next() != Some('{') {
                    return Err("\\u must be written \\u{HEX}".to_string());
                }
                let mut hex = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(h) => hex.push(h),
                        None => return Err("unterminated \\u{".to_string()),
                    }
                }
                let cp = u32::from_str_radix(&hex, 16).map_err(|_| format!("bad \\u{{{hex}}}"))?;
                let ch = char::from_u32(cp).ok_or_else(|| format!("invalid scalar U+{hex}"))?;
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            other => return Err(format!("unknown escape \\{other}")),
        }
    }
    if out.len() > MAX_KEY_SEQUENCE_BYTES {
        return Err(format!(
            "value is {} bytes; a key sequence is capped at {MAX_KEY_SEQUENCE_BYTES}",
            out.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    #[test]
    fn every_action_name_parses() {
        // ACTION_NAMES is the advertised list; it must not drift from `parse`.
        for &name in ACTION_NAMES {
            if let Some(template) = name.strip_suffix("1..switch_tab_9") {
                // Parameterized entry: verify the whole 1..=9 range parses.
                let base = template; // "switch_tab_"
                for n in 1..=9 {
                    assert!(
                        Action::parse(&format!("{base}{n}")).is_some(),
                        "{base}{n} must parse"
                    );
                }
            } else {
                assert!(Action::parse(name).is_some(), "action '{name}' must parse");
            }
        }
    }

    fn ch(c: &str) -> WinitKey {
        WinitKey::Character(SmolStr::new(c))
    }

    /// A basic Cmd+Shift+T chord parses to the cmd+shift mask and the `t` key
    /// (lowercased — case is carried by the SHIFT bit, not the character).
    #[test]
    fn parse_basic_chord() {
        let c = Chord::parse("cmd+shift+t").unwrap();
        assert_eq!(c.mods, MOD_CMD | MOD_SHIFT);
        assert_eq!(c.key, KeyToken::Char('t'));
    }

    /// Modifier order does not matter and case is folded: these all parse equal.
    #[test]
    fn chord_order_and_case_insensitive() {
        let a = Chord::parse("cmd+shift+t").unwrap();
        let b = Chord::parse("Shift+CMD+T").unwrap();
        let c = Chord::parse("shift + cmd + t").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    /// Modifier aliases (super/win/meta → cmd; opt/option → alt; control → ctrl).
    #[test]
    fn modifier_aliases() {
        assert_eq!(
            Chord::parse("super+a").unwrap(),
            Chord::parse("cmd+a").unwrap()
        );
        assert_eq!(
            Chord::parse("option+a").unwrap(),
            Chord::parse("alt+a").unwrap()
        );
        assert_eq!(
            Chord::parse("control+a").unwrap(),
            Chord::parse("ctrl+a").unwrap()
        );
    }

    /// Named keys parse to their `NamedKey`, with common aliases.
    #[test]
    fn named_keys_parse() {
        assert_eq!(
            Chord::parse("ctrl+enter").unwrap().key,
            KeyToken::Named(NamedKey::Enter)
        );
        assert_eq!(
            Chord::parse("cmd+up").unwrap().key,
            KeyToken::Named(NamedKey::ArrowUp)
        );
        assert_eq!(
            Chord::parse("alt+f4").unwrap().key,
            KeyToken::Named(NamedKey::F4)
        );
        assert_eq!(
            Chord::parse("esc").unwrap().key,
            KeyToken::Named(NamedKey::Escape)
        );
    }

    /// Malformed chords are rejected with a reason (the loader warns + skips).
    #[test]
    fn malformed_chords_rejected() {
        assert!(Chord::parse("").is_err());
        assert!(Chord::parse("cmd+").is_err());
        assert!(Chord::parse("cmd+nope+x").is_err()); // unknown modifier-position word
        assert!(Chord::parse("cmd").is_err()); // modifier with no key
        assert!(Chord::parse("notakey").is_err()); // multi-letter non-named word
        assert!(Chord::parse("a+b").is_err()); // key not last
    }

    /// Action names parse, including the indexed `switch_tab_<n>` form (1..=9).
    #[test]
    fn action_names_parse() {
        assert_eq!(Action::parse("new_tab"), Some(Action::NewTab));
        assert_eq!(
            Action::parse("reopen_closed_tab"),
            Some(Action::ReopenClosedTab)
        );
        assert_eq!(
            Action::parse("split_horizontal"),
            Some(Action::SplitHorizontal)
        );
        assert_eq!(Action::parse("switch_tab_3"), Some(Action::SwitchTab(3)));
        assert_eq!(Action::parse("copy"), Some(Action::Copy));
        assert_eq!(
            Action::parse("toggle_matrix_rain"),
            Some(Action::ToggleMatrixRain)
        );
        assert_eq!(Action::parse("unknown_action"), None);
        assert_eq!(Action::parse("switch_tab_0"), None); // out of 1..=9
        assert_eq!(Action::parse("switch_tab_99"), None);
    }

    /// A live winit event builds the SAME chord a config string parsed to, so a
    /// user's `cmd+t` matches the key they press (case-folded, base logical key).
    #[test]
    fn event_chord_matches_parsed() {
        let parsed = Chord::parse("cmd+t").unwrap();
        let live = Chord::from_event(&ch("t"), ModifiersState::SUPER).unwrap();
        assert_eq!(parsed, live);
        // The OS may report the upper-case glyph under Shift; the lookup folds it.
        let parsed_shift = Chord::parse("cmd+shift+t").unwrap();
        let live_shift =
            Chord::from_event(&ch("T"), ModifiersState::SUPER | ModifiersState::SHIFT).unwrap();
        assert_eq!(parsed_shift, live_shift);
    }

    /// A bare modifier press (no key) is not a chord.
    #[test]
    fn bare_modifier_event_is_none() {
        assert!(
            Chord::from_event(&WinitKey::Named(NamedKey::Shift), ModifiersState::SHIFT).is_none()
        );
    }

    /// An EMPTY table is a no-op map: `is_empty` is true and every lookup misses,
    /// so the hardcoded `on_key` path is reached unchanged (the no-regression
    /// invariant — zero cost when nothing is configured).
    #[test]
    fn empty_table_never_matches() {
        let kb = Keybindings::from_config(None);
        assert!(kb.is_empty());
        assert_eq!(kb.lookup(&ch("t"), ModifiersState::SUPER), None);
    }

    /// A populated table resolves a configured chord to its action and still
    /// misses on unbound chords.
    #[test]
    fn config_table_resolves_and_misses() {
        let mut table = std::collections::BTreeMap::new();
        table.insert("cmd+shift+n".to_string(), "new_tab".to_string());
        table.insert("ctrl+a".to_string(), "find".to_string());
        let kb = Keybindings::from_config(Some(&table));
        assert!(!kb.is_empty());
        assert_eq!(
            kb.lookup(&ch("n"), ModifiersState::SUPER | ModifiersState::SHIFT),
            Some(Action::NewTab)
        );
        assert_eq!(
            kb.lookup(&ch("a"), ModifiersState::CONTROL),
            Some(Action::Find)
        );
        // An unbound chord misses → on_key falls through to its hardcoded match.
        assert_eq!(kb.lookup(&ch("t"), ModifiersState::SUPER), None);
    }

    /// A malformed chord OR an unknown action is SKIPPED (fail-open), leaving the
    /// rest of the table intact — one bad line never disables the others.
    #[test]
    fn bad_entries_skipped_rest_kept() {
        let mut table = std::collections::BTreeMap::new();
        table.insert("cmd+t".to_string(), "new_tab".to_string());
        table.insert("garbage++".to_string(), "find".to_string()); // bad chord
        table.insert("cmd+x".to_string(), "no_such_action".to_string()); // bad action
        let kb = Keybindings::from_config(Some(&table));
        assert_eq!(
            kb.lookup(&ch("t"), ModifiersState::SUPER),
            Some(Action::NewTab)
        );
        assert_eq!(kb.lookup(&ch("x"), ModifiersState::SUPER), None); // skipped
        // Only the one valid binding survived.
        assert_eq!(kb.map.len(), 1);
    }

    /// Linux daily-driver default: with no user config, [`Keybindings::resolved`]
    /// must seed the standard terminal chords so a fresh user can copy/paste/open a
    /// tab WITHOUT the Super key (which desktop environments grab). macOS keeps an
    /// empty default (its hardcoded Cmd-* path is the convention there). A user
    /// `[keybindings]` entry overlays on top and wins.
    #[test]
    fn platform_defaults_seed_terminal_chords() {
        let kb = Keybindings::resolved(None);
        #[cfg(target_os = "macos")]
        assert!(
            kb.is_empty(),
            "macOS keeps an empty default keybinding table"
        );
        #[cfg(not(target_os = "macos"))]
        {
            let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
            assert_eq!(
                kb.lookup(&ch("c"), cs),
                Some(Action::Copy),
                "Ctrl+Shift+C copies"
            );
            assert_eq!(
                kb.lookup(&ch("v"), cs),
                Some(Action::Paste),
                "Ctrl+Shift+V pastes"
            );
            assert_eq!(
                kb.lookup(&ch("t"), cs),
                Some(Action::NewTab),
                "Ctrl+Shift+T new tab"
            );
            // The in-window overlays are the ONLY way to reach Settings/About off
            // macOS (no menu bar), so their default chords are load-bearing.
            assert_eq!(
                kb.lookup(&ch("s"), cs),
                Some(Action::ToggleSettings),
                "Ctrl+Shift+S opens Settings"
            );
            assert_eq!(
                kb.lookup(&ch("a"), cs),
                Some(Action::ToggleAbout),
                "Ctrl+Shift+A opens About"
            );
            // Plain Ctrl+C is NOT bound (stays SIGINT to the PTY).
            assert_eq!(kb.lookup(&ch("c"), ModifiersState::CONTROL), None);
            // A user override of a seeded chord wins.
            let mut table = std::collections::BTreeMap::new();
            table.insert("ctrl+shift+c".to_string(), "find".to_string());
            let kb2 = Keybindings::resolved(Some(&table));
            assert_eq!(
                kb2.lookup(&ch("c"), cs),
                Some(Action::Find),
                "user [keybindings] overlay must win over the platform default"
            );
        }
    }

    // ---- [key_sequences] input policy ------------------------------------

    /// Backslash escapes expand to bytes (TOML literal-string style), and any
    /// other char — including a control char a TOML basic string already expanded —
    /// passes through as its UTF-8 bytes.
    #[test]
    fn key_sequence_parses_escapes() {
        assert_eq!(parse_byte_sequence("\\n").unwrap(), vec![0x0a]);
        assert_eq!(
            parse_byte_sequence("\\e[13;2u").unwrap(),
            b"\x1b[13;2u".to_vec()
        );
        assert_eq!(parse_byte_sequence("\\x1b\\x0d").unwrap(), vec![0x1b, 0x0d]);
        assert_eq!(
            parse_byte_sequence("\\u{1b}OP").unwrap(),
            b"\x1bOP".to_vec()
        );
        assert_eq!(parse_byte_sequence("\n").unwrap(), vec![0x0a]); // pre-expanded char
        assert_eq!(parse_byte_sequence("ab").unwrap(), b"ab".to_vec());
    }

    /// Malformed escapes are rejected (the loader warns + skips that entry).
    #[test]
    fn key_sequence_rejects_bad_escapes() {
        assert!(parse_byte_sequence("\\q").is_err()); // unknown escape
        assert!(parse_byte_sequence("\\x1").is_err()); // short hex
        assert!(parse_byte_sequence("\\xzz").is_err()); // bad hex
        assert!(parse_byte_sequence("\\u1b").is_err()); // missing brace
        assert!(parse_byte_sequence("\\").is_err()); // dangling backslash
    }

    /// A `[key_sequences]` table parses to chord -> bytes, looks up by live event,
    /// misses on unbound chords, and skips a malformed line (fail-open).
    #[test]
    fn key_sequences_from_config_and_lookup() {
        let mut table = std::collections::BTreeMap::new();
        table.insert("shift+enter".to_string(), "\\n".to_string());
        table.insert("f5".to_string(), "\\e[15~".to_string());
        table.insert("garbage++".to_string(), "x".to_string()); // bad chord -> skipped
        let ks = KeySequences::from_config(Some(&table));
        assert!(!ks.is_empty());
        assert_eq!(
            ks.lookup(&WinitKey::Named(NamedKey::Enter), ModifiersState::SHIFT),
            Some(&[0x0a][..]),
            "shift+enter -> LF"
        );
        assert_eq!(
            ks.lookup(&WinitKey::Named(NamedKey::F5), ModifiersState::empty()),
            Some(&b"\x1b[15~"[..]),
            "f5 -> ESC[15~"
        );
        // Unbound chord (plain Enter) misses -> default encoder runs.
        assert_eq!(
            ks.lookup(&WinitKey::Named(NamedKey::Enter), ModifiersState::empty()),
            None
        );
        assert_eq!(ks.map.len(), 2, "the bad chord was skipped");
    }

    /// An absent table is a no-op: empty + every lookup misses (zero-cost default).
    #[test]
    fn empty_key_sequences_is_noop() {
        let ks = KeySequences::from_config(None);
        assert!(ks.is_empty());
        assert_eq!(
            ks.lookup(&WinitKey::Named(NamedKey::Enter), ModifiersState::SHIFT),
            None
        );
    }

    /// An empty value is a likely typo (it would silently dead-key the chord), so
    /// from_config WARNS + SKIPS it rather than inserting a zero-byte rule.
    #[test]
    fn key_sequence_empty_value_is_skipped() {
        let mut table = std::collections::BTreeMap::new();
        table.insert("ctrl+x".to_string(), String::new());
        table.insert("ctrl+y".to_string(), "\\n".to_string());
        let ks = KeySequences::from_config(Some(&table));
        assert_eq!(ks.map.len(), 1, "the empty value was skipped");
        assert_eq!(
            ks.lookup(&ch("x"), ModifiersState::CONTROL),
            None,
            "empty -> not mapped, falls through to the encoder"
        );
        assert_eq!(
            ks.lookup(&ch("y"), ModifiersState::CONTROL),
            Some(&[0x0a][..])
        );
    }

    /// A value over the byte cap is rejected (keeps the synchronous PTY write bounded);
    /// the loader then warns + skips it. A value exactly at the cap is accepted.
    #[test]
    fn key_sequence_oversized_value_rejected() {
        let at_cap = "a".repeat(MAX_KEY_SEQUENCE_BYTES);
        assert_eq!(
            parse_byte_sequence(&at_cap).unwrap().len(),
            MAX_KEY_SEQUENCE_BYTES
        );
        let too_big = "a".repeat(MAX_KEY_SEQUENCE_BYTES + 1);
        assert!(parse_byte_sequence(&too_big).is_err());
    }

    /// `\u{HEX}` scalar-range handling is panic-free: a bad scalar is Err (warned +
    /// skipped); a valid astral scalar expands to its UTF-8 bytes.
    #[test]
    fn key_sequence_unicode_brace_edge_cases() {
        assert!(parse_byte_sequence("\\u{}").is_err()); // empty brace
        assert!(parse_byte_sequence("\\u{D800}").is_err()); // lone surrogate
        assert!(parse_byte_sequence("\\u{110000}").is_err()); // over the scalar range
        // U+1F600 GRINNING FACE -> 4 UTF-8 bytes.
        assert_eq!(
            parse_byte_sequence("\\u{1F600}").unwrap(),
            "\u{1F600}".as_bytes().to_vec()
        );
    }

    // ---- gold-plating: resolve_chord, shadow detection, warn-returning loaders ----

    /// resolve_chord captures MAP precedence: keybindings beat key_sequences; a
    /// key_sequences-only chord colliding with a hardcoded Cmd shortcut still resolves
    /// to Sequence (the user table shadows the built-in); unbound -> FallThrough.
    #[test]
    fn resolve_chord_precedence() {
        use std::collections::BTreeMap;
        let mut kbt = BTreeMap::new();
        kbt.insert("cmd+t".to_string(), "new_tab".to_string());
        let mut kst = BTreeMap::new();
        kst.insert("cmd+t".to_string(), "X".to_string());
        let kb = Keybindings::from_config(Some(&kbt));
        let ks = KeySequences::from_config(Some(&kst));
        assert_eq!(
            resolve_chord(&ch("t"), ModifiersState::SUPER, &kb, &ks),
            ChordResolution::Action(Action::NewTab)
        );
        let kb_empty = Keybindings::from_config(None);
        assert_eq!(
            resolve_chord(&ch("t"), ModifiersState::SUPER, &kb_empty, &ks),
            ChordResolution::Sequence(b"X".to_vec())
        );
        let ks_empty = KeySequences::from_config(None);
        assert_eq!(
            resolve_chord(&ch("z"), ModifiersState::SUPER, &kb_empty, &ks_empty),
            ChordResolution::FallThrough
        );
    }

    /// Every BUILTIN_CMD_CHORDS chord string parses (drift/typo guard).
    #[test]
    fn builtin_cmd_chords_all_parse() {
        for (chord, _label) in BUILTIN_CMD_CHORDS {
            assert!(Chord::parse(chord).is_ok(), "{chord} must parse");
        }
    }

    /// builtin_shadow_label maps known built-ins (normalizing spelling); None otherwise.
    #[test]
    fn builtin_shadow_label_maps_known_and_normalizes() {
        assert_eq!(builtin_shadow_label("cmd+c"), Some("Copy"));
        assert_eq!(builtin_shadow_label("cmd+shift+]"), Some("Next Tab"));
        assert_eq!(builtin_shadow_label("cmd+1"), Some("Switch to Tab 1"));
        assert_eq!(builtin_shadow_label("shift+cmd+]"), Some("Next Tab"));
        assert_eq!(builtin_shadow_label("cmd+k"), None);
        assert_eq!(builtin_shadow_label("garbage++"), None);
    }

    /// chord_in_keybindings detects a cross-table collision (normalized).
    #[test]
    fn chord_in_keybindings_detects_cross_table() {
        use std::collections::BTreeMap;
        let mut kb = BTreeMap::new();
        kb.insert("cmd+shift+]".to_string(), "next_tab".to_string());
        assert!(chord_in_keybindings("shift+cmd+]", &kb));
        assert!(!chord_in_keybindings("cmd+x", &kb));
    }

    /// from_config_warn RETURNS the dropped-rule warnings, keeps the good rule, and
    /// reports one warning per offender (bad chord / escape / empty / oversized).
    #[test]
    fn key_sequences_from_config_warn_returns_dropped_rules() {
        use std::collections::BTreeMap;
        let mut t = BTreeMap::new();
        t.insert("ctrl+a".to_string(), "\\n".to_string());
        t.insert("garbage++".to_string(), "x".to_string());
        t.insert("ctrl+b".to_string(), "\\q".to_string());
        t.insert("ctrl+c".to_string(), String::new());
        t.insert("ctrl+d".to_string(), "a".repeat(MAX_KEY_SEQUENCE_BYTES + 1));
        let (ks, warns) = KeySequences::from_config_warn(Some(&t));
        assert_eq!(ks.map.len(), 1, "only the good rule survives");
        assert_eq!(warns.len(), 4, "one warning per dropped rule: {warns:?}");
    }

    /// Keybindings::from_config_warn reports a bad chord + unknown action; resolved_warn
    /// surfaces the same user warnings atop the platform defaults.
    #[test]
    fn keybindings_from_config_warn_reports_unknown_action_and_bad_chord() {
        use std::collections::BTreeMap;
        let mut t = BTreeMap::new();
        t.insert("cmd+t".to_string(), "new_tab".to_string());
        t.insert("garbage++".to_string(), "copy".to_string());
        t.insert("cmd+k".to_string(), "no_such".to_string());
        let (_kb, warns) = Keybindings::from_config_warn(Some(&t));
        assert_eq!(warns.len(), 2, "{warns:?}");
        let (_kb2, warns2) = Keybindings::resolved_warn(Some(&t));
        assert_eq!(warns2.len(), 2);
    }

    /// The eprintln wrapper and the warn-returning variant build IDENTICAL maps.
    #[test]
    fn from_config_still_silent_collects_same() {
        use std::collections::BTreeMap;
        let mut t = BTreeMap::new();
        t.insert("ctrl+a".to_string(), "\\n".to_string());
        t.insert("ctrl+b".to_string(), "\\e[A".to_string());
        let silent = KeySequences::from_config(Some(&t));
        let (warned, _w) = KeySequences::from_config_warn(Some(&t));
        for key in [&ch("a"), &ch("b")] {
            assert_eq!(
                silent.lookup(key, ModifiersState::CONTROL),
                warned.lookup(key, ModifiersState::CONTROL)
            );
        }
    }
}
