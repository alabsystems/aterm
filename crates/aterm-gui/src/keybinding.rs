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
    /// Edit the FOCUSED PANE's session pin in place on the tab strip — the
    /// keyboard face of Window ▸ Rename Session… and of double-clicking a tab.
    /// It renames the SESSION (`meta set title`), not the tab: a tab can hold
    /// several split sessions. No chord by default — bind it in `[keybindings]`
    /// (e.g. `ctrl+shift+r = "rename_session"`).
    RenameSession,
    /// Select the entire visible screen as whole lines (Edit ▸ Select All —
    /// `App::select_all`, the same verb the menu row fires). Default
    /// Ctrl+Shift+A off macOS (keyboard audit #5); ⌘A stays a menu equivalent
    /// on macOS.
    SelectAll,
    /// Toggle the window's borderless full-screen state (View ▸ Enter Full
    /// Screen — `App::toggle_fullscreen`, the same winit path the menu row
    /// takes). Default F11 off macOS (keyboard audit #3) — the chord every
    /// Linux/Windows full-screen surface answers; bindable everywhere.
    ToggleFullscreen,
    /// Find NEXT — step an open find bar forward, or (bar closed) resume the
    /// last accepted query after it (`App::search_find_again`), the reducer
    /// Edit ▸ Find Next already drives. Unbound by default: `f3` is the
    /// Windows-app reflex but it is ALSO a live console key (cmd.exe's
    /// repeat-last-command, Far/Midnight Commander's view), so the seed is left
    /// to the user — `f3 = "find_next"` now works.
    FindNext,
    /// Find PREVIOUS — the backward twin of [`Action::FindNext`].
    FindPrev,
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
    "toggle_matrix_rain",
    "toggle_serious_mode",
    "open_palette",
    "toggle_vi_mode",
    "rename_session",
    "select_all",
    "toggle_fullscreen",
    "find_next",
    "find_prev",
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
    // SELECTION CUSTODY Phase 2: the deliberate return to live / to the oldest
    // retained line. Registered here because this table is the single source both
    // `--list-keybinds` and `--validate-config`'s shadow detector read.
    ("cmd+down", "Scroll to Live"),
    ("cmd+up", "Scroll to Top"),
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
/// `--list-keybinds` inline note so they can't disagree. Gated on the suite
/// actually being LIVE ([`crate::app_input::HARDCODED_SUPER_CHORDS`]): on Linux
/// the whole Cmd/Super suite is compiled off, so no chord conflicts with it and
/// this always answers `None` there ([`builtin_shadow_label_when`]).
#[must_use]
pub(crate) fn builtin_shadow_label(chord_str: &str) -> Option<&'static str> {
    builtin_shadow_label_when(chord_str, crate::app_input::HARDCODED_SUPER_CHORDS)
}

/// The testable core of [`builtin_shadow_label`] with the suite gate EXPLICIT.
/// When the hardcoded Cmd/Super suite is compiled off (Linux — keyboard audit
/// #4, `HARDCODED_SUPER_CHORDS`), `on_key` intercepts NONE of these chords, so
/// nothing can be shadowed and every probe answers `None`: a `super+t` binding
/// there is exactly the explicit rebind the audit promised, not a conflict —
/// warning about it would tell a Linux user their working binding fights a
/// built-in that does not exist on their build.
fn builtin_shadow_label_when(chord_str: &str, suite_live: bool) -> Option<&'static str> {
    if !suite_live {
        return None;
    }
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
    // An UNBIND entry (`= "none"`) is not a claim on the chord — it frees it —
    // so it must not report a shadow that will never fire.
    keybindings
        .iter()
        .any(|(k, v)| !is_unbind_action(v) && Chord::parse(k).is_ok_and(|c| c == target))
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
            "toggle_matrix_rain" => Action::ToggleMatrixRain,
            "toggle_serious_mode" => Action::ToggleSeriousMode,
            "open_palette" => Action::OpenPalette,
            "toggle_vi_mode" => Action::ToggleViMode,
            "rename_session" => Action::RenameSession,
            "select_all" => Action::SelectAll,
            // `fullscreen` is the spelling the `window.fullscreen` command id
            // and other terminals' configs use; `toggle_fullscreen` is the
            // ACTION_NAMES canon matching the sibling `toggle_*` names. Both
            // parse, so neither guess costs an "unknown action" warning.
            "fullscreen" | "toggle_fullscreen" => Action::ToggleFullscreen,
            "find_next" => Action::FindNext,
            "find_prev" | "find_previous" => Action::FindPrev,
            _ => return None,
        })
    }
}

/// Whether a `[keybindings]` VALUE spells "unbind this chord" — `"none"` or
/// `"unbind"` — rather than naming an action. An unbind entry MASKS a platform
/// seed in [`Keybindings::resolved`]/[`Keybindings::resolved_warn`]: the chord
/// falls through to the PTY encoder as if it had never been seeded (keyboard
/// audit #2 — e.g. `"ctrl+tab" = "none"` returns Ctrl+Tab to a kitty-protocol
/// app). Unbinding a chord that was never bound is a harmless no-op, NOT a
/// warning — a config that pins "this chord stays free" should survive a seed
/// table that later grows it.
#[must_use]
pub(crate) fn is_unbind_action(name: &str) -> bool {
    matches!(name.trim(), "none" | "unbind")
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
/// character is a `Char`; `plus`/`minus`/`equal` are word spellings of characters
/// (only `plus` is strictly necessary — see below); any other multi-letter word is
/// matched against the named-key table (Enter, Tab, arrows, F-keys, …). Returns
/// `None` for an unknown name.
fn key_token(word: &str) -> Option<KeyToken> {
    let mut chars = word.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        // Single character: fold to lowercase so case is carried only by SHIFT.
        return Some(KeyToken::Char(first.to_ascii_lowercase()));
    }
    // SPELLED-OUT PUNCTUATION. `+` is the segment separator and `Chord::parse`
    // splits on it with no escape, so `"ctrl++"` is not a hard-to-read chord — it
    // is a parse error ("empty segment"), and the `+` key was therefore not
    // bindable AT ALL. That is not cosmetic off a US layout: on de-DE/Nordic
    // layouts `+` is an unshifted MAIN-ROW key (so `ctrl+shift+=` never fires) and
    // on EVERY layout the numpad `+` arrives as `Character("+")`, which no `=`
    // spelling matches. `minus`/`equal` are spelled alongside it so a config never
    // has to know which one of the three is the special one; both also have plain
    // single-character spellings that keep working.
    match word {
        "plus" => return Some(KeyToken::Char('+')),
        "minus" => return Some(KeyToken::Char('-')),
        "equal" | "equals" => return Some(KeyToken::Char('=')),
        _ => {}
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

    /// Human-readable presentation of this chord — `Ctrl+Shift+C`, `F11`,
    /// `Shift+Insert` — the accelerator-hint spelling the palette/menu rows
    /// show off macOS. ROUND-TRIPS: every output re-parses (case-folded)
    /// through [`Chord::parse`] to this same chord, so a hint is always a
    /// string the user could paste straight into `[keybindings]`. Modifier
    /// order is the Windows/GTK convention (Ctrl, Alt, Shift, Super).
    #[must_use]
    // Reached only through `Keybindings::display_chord_for`, whose caller in
    // `app_palette.rs` is `#[cfg(not(target_os = "macos"))]`; `test` because the
    // round-trip tests below exercise it on every platform.
    #[cfg(any(test, not(target_os = "macos")))]
    pub(crate) fn display(&self) -> String {
        let mut out = String::new();
        if self.mods & MOD_CTRL != 0 {
            out.push_str("Ctrl+");
        }
        if self.mods & MOD_ALT != 0 {
            out.push_str("Alt+");
        }
        if self.mods & MOD_SHIFT != 0 {
            out.push_str("Shift+");
        }
        if self.mods & MOD_CMD != 0 {
            out.push_str("Super+");
        }
        match &self.key {
            // `+` is the parse separator, so its round-trippable spelling is the
            // word form (`ctrl+plus`), title-cased like the named keys.
            KeyToken::Char('+') => out.push_str("Plus"),
            // ASCII-only uppercase, because `parse` folds ASCII-only: the
            // Unicode fold broke the documented round-trip both ways — 'ß'
            // uppercased to the TWO-char "SS" (an unknown word that fails
            // `key_token`), and a one-char map like 'é'→'É' re-parsed to 'É',
            // which `to_ascii_lowercase` never folds back to 'é'.
            KeyToken::Char(c) => out.push(c.to_ascii_uppercase()),
            KeyToken::Named(named) => out.push_str(named_key_display(*named)),
        }
        out
    }
}

/// Display name of a parseable [`NamedKey`] — the reverse of [`key_token`]'s
/// named table, in the spelling that re-parses. Only keys `key_token` can
/// produce reach this in practice (a [`Chord`] in a map was built by `parse` or
/// matched one that was); the `"?"` tail keeps the match total for any future
/// `from_event`-only chord that acquires a display path.
#[cfg(any(test, not(target_os = "macos")))]
fn named_key_display(named: NamedKey) -> &'static str {
    match named {
        NamedKey::Enter => "Enter",
        NamedKey::Tab => "Tab",
        NamedKey::Space => "Space",
        NamedKey::Escape => "Esc",
        NamedKey::Backspace => "Backspace",
        NamedKey::Delete => "Del",
        NamedKey::Insert => "Insert",
        NamedKey::ArrowUp => "Up",
        NamedKey::ArrowDown => "Down",
        NamedKey::ArrowLeft => "Left",
        NamedKey::ArrowRight => "Right",
        NamedKey::Home => "Home",
        NamedKey::End => "End",
        NamedKey::PageUp => "PageUp",
        NamedKey::PageDown => "PageDown",
        NamedKey::F1 => "F1",
        NamedKey::F2 => "F2",
        NamedKey::F3 => "F3",
        NamedKey::F4 => "F4",
        NamedKey::F5 => "F5",
        NamedKey::F6 => "F6",
        NamedKey::F7 => "F7",
        NamedKey::F8 => "F8",
        NamedKey::F9 => "F9",
        NamedKey::F10 => "F10",
        NamedKey::F11 => "F11",
        NamedKey::F12 => "F12",
        _ => "?",
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
    /// Collect the parsed map, the UNBIND chords (`"none"`/`"unbind"` values —
    /// see [`is_unbind_action`]), PLUS a per-entry WARNING string (unprefixed)
    /// for each skipped chord/action. Shared by [`Self::from_config`] (which
    /// eprintln!s the warnings), [`Self::from_config_warn`] (which returns them
    /// for an in-window notice), and [`Self::resolved_warn`] (the only consumer
    /// of the unbinds — a config-only map has no platform seed to mask).
    fn collect(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (HashMap<Chord, Action>, Vec<Chord>, Vec<String>) {
        let mut map = HashMap::new();
        let mut unbinds = Vec::new();
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
                if is_unbind_action(action_str) {
                    unbinds.push(chord);
                    continue;
                }
                let Some(action) = Action::parse(action_str) else {
                    warns.push(format!(
                        "config keybindings: skipping {chord_str:?}: unknown action {action_str:?}"
                    ));
                    continue;
                };
                map.insert(chord, action);
            }
        }
        (map, unbinds, warns)
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
        let (map, _unbinds, warns) = Self::collect(table);
        for w in &warns {
            eprintln!("aterm-gui: {w}");
        }
        Keybindings { map }
    }

    /// Like [`Self::from_config`] but RETURNS the warnings instead of printing them, so
    /// the GUI can surface dropped rules in an in-window notice (stderr is invisible to
    /// a Finder-launched .app). The map is byte-identical to `from_config`.
    // Config-only ctor pair of `from_config`: production moved to `resolved_warn`
    // (which consults `collect` directly so unbinds can mask the platform seeds),
    // leaving both no-defaults ctors to the config-parsing tests — same allowance,
    // same reason as `from_config` above.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn from_config_warn(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (Keybindings, Vec<String>) {
        let (map, _unbinds, warns) = Self::collect(table);
        (Keybindings { map }, warns)
    }

    /// The (chord, action-name) seed table behind [`Self::platform_defaults`]:
    /// pairs parsed through the SAME machinery as user config, so a default can
    /// never diverge from what a user could write. macOS = none.
    ///
    /// `pub(crate)` — not a private local — because two DOCUMENTATION surfaces
    /// print it: `--list-keybinds` (diagnostics.rs) and `--help`'s KEYS section
    /// (cli.rs). Both used to carry hand-written copies; the diagnostics one
    /// printed the macOS `cmd+*` table on Windows (where `cmd` is the Win key —
    /// chords the shell steals) and the help one had drifted to omit several
    /// seeded chords. Generating both from THIS const makes that drift
    /// impossible. Order is presentation order, so keep related chords adjacent.
    #[cfg(target_os = "macos")]
    pub(crate) const PLATFORM_DEFAULT_PAIRS: &'static [(&'static str, &'static str)] = &[];
    #[cfg(not(target_os = "macos"))]
    pub(crate) const PLATFORM_DEFAULT_PAIRS: &'static [(&'static str, &'static str)] = &[
        ("ctrl+shift+c", "copy"),
        ("ctrl+shift+v", "paste"),
        // WINDOWS ONLY: plain Ctrl+V pastes — the Windows-native muscle memory
        // (Windows Terminal ships it, accepting that it shadows quoted-insert
        // ^V; rebindable). On Linux the chord is deliberately NOT seeded
        // (keyboard audit #1): ^V there is readline quoted-insert and vim's
        // visual-block entry — keys a terminal owes its user — and every Linux
        // terminal convention pastes on Ctrl+Shift+V / Shift+Insert, both of
        // which stay seeded. A Linux user who wants the WT behaviour writes one
        // config line: `"ctrl+v" = "paste"`.
        #[cfg(windows)]
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
        // PANE FOCUS. Without these a split was a one-way door off macOS: the
        // splits above were seeded but nothing moved the keyboard between the
        // panes they create, `menu.rs` has no pane-focus MenuAction (so the
        // palette does not expose it either) and there is no control verb — a
        // Ctrl+Shift+D user was left reaching for the mouse. Alt+arrow is what
        // `docs/NATIVE_WINDOWS_DESIGN.md` §9 names ("Alt+←/→ pane nav",
        // milestone W4) and what Windows Terminal binds `moveFocus` to.
        //
        // It does shadow two things, both accepted and both rebindable: the
        // PTY's `ESC[1;3<A-D>` (no stock shell or `less`/`vim` mapping uses
        // it), and — because the find bar is consulted BELOW the keybinding
        // block in `on_key` — ⌥←/⌥→ word motion inside an OPEN find field. The
        // latter is a macOS text-field idiom that Windows spells Ctrl+arrow
        // anyway, and pane nav is reachable from nowhere else.
        ("alt+left", "focus_pane_left"),
        ("alt+right", "focus_pane_right"),
        ("alt+up", "focus_pane_up"),
        ("alt+down", "focus_pane_down"),
        ("ctrl+shift+right", "next_tab"),
        ("ctrl+shift+left", "prev_tab"),
        ("ctrl+pagedown", "next_tab"),
        ("ctrl+pageup", "prev_tab"),
        // The document-window reflex every Windows app answers. Ctrl+Tab is
        // indistinguishable from a bare Tab in the LEGACY encoding, so nothing
        // that works today loses a keystroke; a kitty-protocol app does lose
        // its `ESC[9;5u`, which is the same trade Windows Terminal makes.
        ("ctrl+tab", "next_tab"),
        ("ctrl+shift+tab", "prev_tab"),
        // Jump-to-prompt (OSC-133 marks): the WezTerm/iTerm2 chord. Up/Down
        // are free here (left/right are tab nav), so no shadow.
        ("ctrl+shift+up", "jump_prev_prompt"),
        ("ctrl+shift+down", "jump_next_prompt"),
        // Zoom: Ctrl+= and Ctrl+Shift+= (the latter is "Ctrl++" on a US layout,
        // where `+` is Shift+`=` and the chord lookup runs on the UNSHIFTED base
        // key). `ctrl+plus` is the other half of the same reflex and is NOT
        // redundant: it is the only spelling that catches the numpad `+` (which
        // no layout composes from `=`) and the de-DE/Nordic MAIN-ROW `+`, an
        // unshifted key in its own right. Zoom OUT and RESET need no such twin —
        // `-` and `0` are unshifted on all of those layouts.
        ("ctrl+=", "font_increase"),
        ("ctrl+shift+=", "font_increase"),
        ("ctrl+plus", "font_increase"),
        ("ctrl+-", "font_decrease"),
        ("ctrl+0", "font_reset"),
        ("ctrl+shift+s", "toggle_settings"),
        // The Windows/VS Code/Windows Terminal preferences chord. Nothing in
        // the tree bound `ctrl+,` before this, and no shell or TUI claims it
        // (there is no control code for Ctrl+comma — the PTY encoder drops the
        // modifier and sends a bare `,`, which is not something anyone presses
        // Ctrl for).
        ("ctrl+,", "toggle_settings"),
        // Ctrl+Shift+A is SELECT ALL (keyboard audit #5): the Shift-elevated
        // form of the universal ^A, the same pattern every seeded clipboard
        // chord here follows (^C/^V/^F → Ctrl+Shift+…). It used to open About —
        // but a whole-screen selection is a daily verb and About is a
        // destination, reachable from the palette (Ctrl+Shift+P) and the menu,
        // and still bindable via `toggle_about`.
        ("ctrl+shift+a", "select_all"),
        ("ctrl+shift+p", "open_palette"),
        // F11 full-screen (keyboard audit #3): the chord every Linux/Windows
        // full-screen surface answers (GNOME/KDE convention, Windows Terminal
        // default). It does shadow the raw F11 a TUI app could receive — the
        // same trade GNOME Terminal makes — and both escape hatches are one
        // line: rebind it, or `"f11" = "none"` to return the key to the PTY.
        //
        // F3/Shift+F3 find-again are deliberately NOT seeded even though they
        // are the Windows-app reflex: unlike F11 (a window verb no console app
        // owns), F3 is already owed to the program on the other side of the
        // PTY (cmd.exe retypes the last command on it; Far and Midnight
        // Commander put View there), and Windows Terminal does not bind it
        // either — the same reasoning `ctrl+alt+<digit>` gets below. The
        // ACTIONS parse, so `f3 = "find_next"` is one config line away.
        ("f11", "toggle_fullscreen"),
        // NOTE: jump-to-tab-N is intentionally NOT seeded, on EITHER spelling.
        // Both candidate chords collide with something a keyboard already owes
        // its user, and neither collision is ours to accept by default:
        //
        // * bare Alt+digit (the GNOME-Terminal convention) is readline/emacs/vim
        //   META-DIGIT, the numeric argument — and it fires BEFORE the PTY
        //   encoder, so the digit would never reach the app at all;
        // * Ctrl+Alt+digit (the Windows Terminal chord) IS AltGr on Windows. The
        //   tempting counter-argument — "winit filters the Ctrl and Alt bits out
        //   under AltGr" — is only half true, and the false half is a de-DE
        //   regression: `keyboard_layout.rs:277` reads
        //       let filter_out_altgr = layout.has_alt_graph && key_pressed(VK_RMENU);
        //   so the bits are cleared only while the RIGHT Alt is down. Windows
        //   equally accepts LeftCtrl+LeftAlt as AltGr for composition — that is
        //   in fact how winit DETECTS AltGr (same file, :412-424, comparing the
        //   CONTROL|ALT character against the unmodified one) — so on de-DE the
        //   documented alternate spelling of `{`, `[` and `]`, LCtrl+LAlt+7/8/9,
        //   arrives here as a plain `ctrl+alt+<digit>` chord. Seeding it would
        //   switch tabs while a German user typed a brace into their shell.
        //
        // Windows Terminal ships Ctrl+Alt+digit anyway, so the chord is
        // defensible — but it is an OWNER's call to trade brace input for tab
        // jumping by default, not a polish patch's, and this file already
        // carried a reasoned rejection of jump-to-tab that a default must not
        // silently overturn. Tab nav stays covered by next/prev (Ctrl+Tab,
        // Ctrl+Shift+Left/Right, Ctrl+PageUp/Down), and a user who wants N-jump
        // has both spellings available: `switch_tab_N` parses and dispatches, so
        // `ctrl+alt+1 = "switch_tab_1"` in `[keybindings]` works today.
    ];

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
        let mut map = HashMap::new();
        for (chord_str, action_str) in Self::PLATFORM_DEFAULT_PAIRS {
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
        Self::resolved_warn(table).0
    }

    /// Like [`Self::resolved`] but RETURNS the user-table warnings (the platform
    /// defaults are compile-time-checked and never warn). For the GUI config notice.
    ///
    /// This is also where an UNBIND entry (`"<chord>" = "none"`/`"unbind"`)
    /// takes effect: the chord is REMOVED from the platform-default map before
    /// the user's own bindings overlay, so a masked seed falls through to the
    /// PTY encoder exactly as if it had never been seeded.
    #[must_use]
    pub fn resolved_warn(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (Keybindings, Vec<String>) {
        let mut kb = Keybindings::platform_defaults();
        let (user, unbinds, warns) = Self::collect(table);
        for chord in &unbinds {
            kb.map.remove(chord);
        }
        for (chord, action) in user {
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

    /// The DISPLAY chord for `action` in this EFFECTIVE map, if any — the
    /// accelerator hint the palette's menu rows show off macOS (where the ⌘
    /// key-equivalents are honestly blanked and the rows used to show nothing).
    ///
    /// Preference order: the first [`Self::PLATFORM_DEFAULT_PAIRS`] entry
    /// (presentation order — so Next Tab shows `Ctrl+Shift+Right`, not one of
    /// its aliases) that STILL resolves to `action` in this map — a user rebind
    /// or `"none"` unbind of a seed is therefore honored, never advertised
    /// stale. When no seed survives, the lexicographically smallest display of
    /// any user-bound chord (stable across the `HashMap`'s arbitrary iteration
    /// order). `None` when nothing is bound: the row shows no hint rather than
    /// a chord that does not work.
    #[must_use]
    // The palette hint column; its one caller (`app_palette.rs`) is
    // `#[cfg(not(target_os = "macos"))]` because macOS draws the hint from the
    // native menu instead, and its test carries the SAME gate — so on macOS this
    // is genuinely unreachable and the cfg is exact rather than widened with
    // `test`.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn display_chord_for(&self, action: Action) -> Option<String> {
        for (chord_str, action_str) in Self::PLATFORM_DEFAULT_PAIRS {
            if Action::parse(action_str) == Some(action)
                && let Ok(chord) = Chord::parse(chord_str)
                && self.map.get(&chord) == Some(&action)
            {
                return Some(chord.display());
            }
        }
        self.map
            .iter()
            .filter(|&(_, a)| *a == action)
            .map(|(c, _)| c.display())
            .min()
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
        assert_eq!(
            Action::parse("toggle_sparkle_words"),
            None,
            "the hidden runtime master must not contradict the two Settings toggles"
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
            // The native Settings tab is the ONLY way to reach Settings off
            // macOS, so its default chord is load-bearing.
            assert_eq!(
                kb.lookup(&ch("s"), cs),
                Some(Action::ToggleSettings),
                "Ctrl+Shift+S opens Settings"
            );
            // Keyboard audit #5: Ctrl+Shift+A is SELECT ALL, not About — About
            // moved to the palette/menu (and stays bindable via `toggle_about`).
            assert_eq!(
                kb.lookup(&ch("a"), cs),
                Some(Action::SelectAll),
                "Ctrl+Shift+A selects all"
            );
            // Keyboard audit #3: F11 toggles full-screen (bare chord, no mods).
            assert_eq!(
                kb.lookup(&WinitKey::Named(NamedKey::F11), ModifiersState::empty()),
                Some(Action::ToggleFullscreen),
                "F11 toggles full-screen"
            );
            // Keyboard audit #1: plain Ctrl+V pastes ONLY on Windows (the WT
            // default). On Linux it stays the PTY's — readline quoted-insert
            // and vim visual-block — with Ctrl+Shift+V/Shift+Insert seeded.
            assert_eq!(
                kb.lookup(&ch("v"), ModifiersState::CONTROL),
                if cfg!(windows) {
                    Some(Action::Paste)
                } else {
                    None
                },
                "plain Ctrl+V is a Windows-only paste seed"
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

    /// ZOOM IN IS REACHABLE FROM A `+` KEY. `Chord::parse` splits on `+` with no
    /// escape, so `"ctrl++"` cannot be written at all — which left `+` unbindable
    /// and zoom-in unreachable wherever `+` is not Shift+`=`: the de-DE/Nordic
    /// MAIN-ROW `+`, and the NUMPAD `+` on every layout including en-US (both
    /// arrive as `Character("+")` with no Shift). `plus` is the spelling that fixes
    /// it, for the seed AND for a user's own config.
    #[test]
    fn plus_is_bindable_and_seeded_for_zoom_in() {
        assert_eq!(
            Chord::parse("ctrl+plus").unwrap(),
            Chord {
                mods: MOD_CTRL,
                key: KeyToken::Char('+')
            }
        );
        // The three spellings agree; `minus`/`equal` exist so a config never has to
        // know that only `+` is special.
        assert_eq!(Chord::parse("ctrl+minus"), Chord::parse("ctrl+-"));
        assert_eq!(Chord::parse("ctrl+equal"), Chord::parse("ctrl+="));
        // `ctrl++` remains a parse ERROR (the separator wins) — `plus` is the
        // escape hatch, not a second syntax.
        assert!(Chord::parse("ctrl++").is_err());

        #[cfg(not(target_os = "macos"))]
        {
            let kb = Keybindings::resolved(None);
            // Unshifted `+`: de-DE/Nordic main row, and the numpad everywhere.
            assert_eq!(
                kb.lookup(&ch("+"), ModifiersState::CONTROL),
                Some(Action::FontIncrease),
                "Ctrl+Plus zooms in"
            );
            // The US main row still arrives as the UNSHIFTED base `=` plus Shift,
            // which is the pre-existing seed — unchanged.
            assert_eq!(
                kb.lookup(&ch("="), ModifiersState::CONTROL | ModifiersState::SHIFT),
                Some(Action::FontIncrease),
                "Ctrl+Shift+= still zooms in"
            );
        }
    }

    /// PANE FOCUS AND TAB NAV EXIST OFF macOS. The splits were seeded and their
    /// panes were then unreachable from the keyboard: no `focus_pane_*` default, no
    /// pane-focus MenuAction (so no palette row either) and no control verb. Same
    /// for the two chords every Windows document window answers.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn platform_defaults_seed_pane_focus_and_tab_nav() {
        let kb = Keybindings::resolved(None);
        let named = WinitKey::Named;
        let alt = ModifiersState::ALT;
        assert_eq!(
            kb.lookup(&named(NamedKey::ArrowLeft), alt),
            Some(Action::FocusPaneLeft)
        );
        assert_eq!(
            kb.lookup(&named(NamedKey::ArrowRight), alt),
            Some(Action::FocusPaneRight)
        );
        assert_eq!(
            kb.lookup(&named(NamedKey::ArrowUp), alt),
            Some(Action::FocusPaneUp)
        );
        assert_eq!(
            kb.lookup(&named(NamedKey::ArrowDown), alt),
            Some(Action::FocusPaneDown)
        );
        assert_eq!(
            kb.lookup(&named(NamedKey::Tab), ModifiersState::CONTROL),
            Some(Action::NextTab),
            "Ctrl+Tab"
        );
        assert_eq!(
            kb.lookup(
                &named(NamedKey::Tab),
                ModifiersState::CONTROL | ModifiersState::SHIFT
            ),
            Some(Action::PrevTab),
            "Ctrl+Shift+Tab"
        );
        // JUMP-TO-TAB-N STAYS UNSEEDED ON BOTH SPELLINGS, and that is a decision
        // worth pinning rather than a hole nobody filled. Bare Alt+digit is
        // readline/vim's META-DIGIT numeric argument, which fires before the PTY
        // encoder and would never reach the app; Ctrl+Alt+digit is how Windows
        // spells AltGr for the LeftCtrl+LeftAlt composition, so on de-DE it is the
        // documented alternate spelling of `{`, `[` and `]` (7/8/9). winit clears
        // the Ctrl/Alt bits only while the RIGHT Alt is down
        // (`keyboard_layout.rs:277`), so those braces arrive here as an ordinary
        // ctrl+alt chord and a seed would eat them.
        let ctrl_alt = ModifiersState::CONTROL | ModifiersState::ALT;
        for n in 1..=9u8 {
            assert_eq!(
                kb.lookup(&ch(&n.to_string()), ctrl_alt),
                None,
                "Ctrl+Alt+{n} must stay free — it is AltGr+{n} on de-DE"
            );
            assert_eq!(kb.lookup(&ch(&n.to_string()), ModifiersState::ALT), None);
        }
        // …but the action exists and the chord parses, so a user who wants the
        // Windows Terminal behaviour can have it in one config line.
        let mut table = std::collections::BTreeMap::new();
        table.insert("ctrl+alt+2".to_string(), "switch_tab_2".to_string());
        assert_eq!(
            Keybindings::resolved(Some(&table)).lookup(&ch("2"), ctrl_alt),
            Some(Action::SwitchTab(2)),
            "opt-in still works"
        );
    }

    /// FULL SCREEN IS BINDABLE AT ALL, on BOTH spellings. The sharp edge was
    /// never "F11 does nothing" — it was that a user could not RESCUE it:
    /// before the Action existed, `f11 = "fullscreen"` in `[keybindings]`
    /// parsed the chord and then DROPPED it with an "unknown action" warning.
    /// On macOS a menu-only command costs nothing (the menu bar carries it);
    /// off macOS there is no menu bar, so the asymmetry was a permanent
    /// capability hole.
    #[test]
    fn fullscreen_is_a_bindable_action() {
        // Both spellings: the command-id/other-terminal one and the
        // ACTION_NAMES canon that matches the sibling `toggle_*` names.
        assert_eq!(Action::parse("fullscreen"), Some(Action::ToggleFullscreen));
        assert_eq!(
            Action::parse("toggle_fullscreen"),
            Some(Action::ToggleFullscreen)
        );
        // The config rescue path end to end: no warning, and the chord lands.
        let mut table = std::collections::BTreeMap::new();
        table.insert("ctrl+shift+f11".to_string(), "fullscreen".to_string());
        let (kb, warns) = Keybindings::from_config_warn(Some(&table));
        assert!(warns.is_empty(), "no dropped-rule warning: {warns:?}");
        assert_eq!(
            kb.lookup(
                &WinitKey::Named(NamedKey::F11),
                ModifiersState::CONTROL | ModifiersState::SHIFT
            ),
            Some(Action::ToggleFullscreen),
        );
    }

    /// Find-again is bindable for the same reason — it existed only as a
    /// `MenuAction` too. NOT seeded: `f3` is the Windows-app reflex but it is
    /// also a live console key (cmd.exe retypes the last command on it;
    /// Far/Midnight Commander put View there), so the capability ships and the
    /// chord stays the user's call — `f3 = "find_next"` is one config line.
    #[test]
    fn find_again_is_bindable_but_unseeded() {
        assert_eq!(Action::parse("find_next"), Some(Action::FindNext));
        assert_eq!(Action::parse("find_prev"), Some(Action::FindPrev));
        assert_eq!(Action::parse("find_previous"), Some(Action::FindPrev));
        let kb = Keybindings::resolved(None);
        for f in [NamedKey::F3, NamedKey::F1, NamedKey::F12] {
            assert_eq!(
                kb.lookup(&WinitKey::Named(f), ModifiersState::empty()),
                None,
                "{f:?} stays the program's on the other side of the PTY",
            );
        }
        let mut table = std::collections::BTreeMap::new();
        table.insert("f3".to_string(), "find_next".to_string());
        table.insert("shift+f3".to_string(), "find_prev".to_string());
        let kb = Keybindings::resolved(Some(&table));
        assert_eq!(
            kb.lookup(&WinitKey::Named(NamedKey::F3), ModifiersState::empty()),
            Some(Action::FindNext),
            "the Windows reflex is one config line away",
        );
        assert_eq!(
            kb.lookup(&WinitKey::Named(NamedKey::F3), ModifiersState::SHIFT),
            Some(Action::FindPrev),
        );
    }

    /// F11 and Ctrl+, are seeded off macOS: the universal full-screen reflex
    /// (the same default Windows Terminal ships) and the Windows/VS Code/WT
    /// preferences chord, which was bound nowhere in the tree.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn platform_defaults_seed_fullscreen_and_preferences() {
        let kb = Keybindings::resolved(None);
        assert_eq!(
            kb.lookup(&WinitKey::Named(NamedKey::F11), ModifiersState::empty()),
            Some(Action::ToggleFullscreen),
            "F11 toggles full screen",
        );
        assert_eq!(
            kb.lookup(&ch(","), ModifiersState::CONTROL),
            Some(Action::ToggleSettings),
            "Ctrl+, opens Settings",
        );
        // Still rebindable like every other seed.
        let mut table = std::collections::BTreeMap::new();
        table.insert("f11".to_string(), "open_palette".to_string());
        assert_eq!(
            Keybindings::resolved(Some(&table))
                .lookup(&WinitKey::Named(NamedKey::F11), ModifiersState::empty()),
            Some(Action::OpenPalette),
        );
    }

    /// UNBIND (keyboard audit #2): a `[keybindings]` value of `"none"` (or
    /// `"unbind"`) MASKS a platform seed — the chord misses the map and falls
    /// through to the PTY encoder as if never seeded — and is NOT an unknown
    /// action (no warning). Unbinding a chord that was never bound is a silent
    /// no-op, and the surrounding seeds survive untouched.
    #[test]
    fn unbind_value_masks_platform_seed() {
        let mut table = std::collections::BTreeMap::new();
        table.insert("ctrl+tab".to_string(), "none".to_string());
        table.insert("f11".to_string(), "unbind".to_string());
        table.insert("ctrl+alt+f9".to_string(), "none".to_string()); // never bound: no-op
        let (kb, warns) = Keybindings::resolved_warn(Some(&table));
        assert!(
            warns.is_empty(),
            "unbind is not an unknown action: {warns:?}"
        );
        assert_eq!(
            kb.lookup(&WinitKey::Named(NamedKey::Tab), ModifiersState::CONTROL),
            None,
            "ctrl+tab seed is masked"
        );
        assert_eq!(
            kb.lookup(&WinitKey::Named(NamedKey::F11), ModifiersState::empty()),
            None,
            "f11 seed is masked (both spellings work)"
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            kb.lookup(&ch("c"), ModifiersState::CONTROL | ModifiersState::SHIFT),
            Some(Action::Copy),
            "the other seeds survive an unbind"
        );
        // An unbind in the config-only ctor simply binds nothing (no seed to
        // mask there), and never warns.
        let (kb_cfg, warns_cfg) = Keybindings::from_config_warn(Some(&table));
        assert!(warns_cfg.is_empty());
        assert!(kb_cfg.is_empty());
        // …and an unbind entry is NOT a [key_sequences] shadow claim.
        assert!(!chord_in_keybindings("ctrl+tab", &table));
    }

    /// The accelerator-hint reverse lookup: seeds resolve in PRESENTATION order,
    /// a user rebind/unbind is honored (never a stale hint), an unbound action
    /// yields None, and every display spelling round-trips through
    /// `Chord::parse` so a hint is always pasteable into `[keybindings]`.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn display_chord_for_resolves_effective_hints() {
        let kb = Keybindings::resolved(None);
        assert_eq!(
            kb.display_chord_for(Action::Copy).as_deref(),
            Some("Ctrl+Shift+C")
        );
        assert_eq!(
            kb.display_chord_for(Action::ToggleFullscreen).as_deref(),
            Some("F11")
        );
        assert_eq!(
            kb.display_chord_for(Action::NextTab).as_deref(),
            Some("Ctrl+Shift+Right"),
            "presentation order picks the FIRST seeded spelling, not an alias"
        );
        assert_eq!(
            kb.display_chord_for(Action::ToggleMatrixRain),
            None,
            "an unbound action shows no hint"
        );
        // Unseat BOTH copy seeds and rebind: the hint follows the user's table.
        let mut t = std::collections::BTreeMap::new();
        t.insert("ctrl+shift+c".to_string(), "none".to_string());
        t.insert("ctrl+insert".to_string(), "none".to_string());
        t.insert("ctrl+shift+y".to_string(), "copy".to_string());
        let kb2 = Keybindings::resolved(Some(&t));
        assert_eq!(
            kb2.display_chord_for(Action::Copy).as_deref(),
            Some("Ctrl+Shift+Y")
        );
        // Unseat only the primary: the surviving seed is the honest hint.
        let mut t2 = std::collections::BTreeMap::new();
        t2.insert("ctrl+shift+c".to_string(), "none".to_string());
        let kb3 = Keybindings::resolved(Some(&t2));
        assert_eq!(
            kb3.display_chord_for(Action::Copy).as_deref(),
            Some("Ctrl+Insert")
        );
        // Round-trip: every seed's display re-parses to the same chord.
        for (chord_str, _) in Keybindings::PLATFORM_DEFAULT_PAIRS {
            let chord = Chord::parse(chord_str).unwrap();
            assert_eq!(
                Chord::parse(&chord.display()),
                Ok(chord.clone()),
                "display of {chord_str:?} must round-trip"
            );
        }
    }

    /// Display's uppercase is ASCII-only, because `parse` folds ASCII-only:
    /// the Unicode fold turned `ctrl+ß` into "Ctrl+SS" (two chars — an unknown
    /// word that no longer parses) and `ctrl+é` into "Ctrl+É" (which re-parses
    /// to the DIFFERENT chord key 'É'). Non-ASCII keys display verbatim and
    /// round-trip; ASCII letters keep their uppercase hint spelling.
    #[test]
    fn display_round_trips_non_ascii_chord_keys() {
        for spelling in ["ctrl+ß", "ctrl+é", "ctrl+shift+a", "alt+µ"] {
            let chord = Chord::parse(spelling).unwrap();
            assert_eq!(
                Chord::parse(&chord.display()),
                Ok(chord.clone()),
                "display of {spelling:?} must round-trip"
            );
        }
        assert_eq!(Chord::parse("ctrl+ß").unwrap().display(), "Ctrl+ß");
        assert_eq!(Chord::parse("ctrl+a").unwrap().display(), "Ctrl+A");
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

    /// builtin_shadow_label maps known built-ins (normalizing spelling); None
    /// otherwise. Exercised through the gate-explicit core with the suite LIVE,
    /// so the mapping is asserted identically on every platform.
    #[test]
    fn builtin_shadow_label_maps_known_and_normalizes() {
        assert_eq!(builtin_shadow_label_when("cmd+c", true), Some("Copy"));
        assert_eq!(
            builtin_shadow_label_when("cmd+shift+]", true),
            Some("Next Tab")
        );
        assert_eq!(
            builtin_shadow_label_when("cmd+1", true),
            Some("Switch to Tab 1")
        );
        assert_eq!(
            builtin_shadow_label_when("shift+cmd+]", true),
            Some("Next Tab")
        );
        assert_eq!(builtin_shadow_label_when("cmd+k", true), None);
        assert_eq!(builtin_shadow_label_when("garbage++", true), None);
    }

    /// With the Cmd/Super suite compiled OFF (Linux — keyboard audit #4) NO
    /// chord can shadow it, so every probe answers None — a Linux `super+t`
    /// rebind must not warn "conflicts with built-in New Tab" for a suite that
    /// is not there. The platform wrapper follows `HARDCODED_SUPER_CHORDS`.
    #[test]
    fn builtin_shadow_label_is_silent_when_the_suite_is_gated_off() {
        assert_eq!(builtin_shadow_label_when("cmd+c", false), None);
        assert_eq!(builtin_shadow_label_when("super+t", false), None);
        let expected = if crate::app_input::HARDCODED_SUPER_CHORDS {
            Some("Copy")
        } else {
            None
        };
        assert_eq!(builtin_shadow_label("cmd+c"), expected);
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
