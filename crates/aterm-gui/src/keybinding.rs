// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! User-rebindable keyboard shortcuts (config `[keybindings]`).

use std::collections::HashMap;

use aterm_window::keyboard::{Key as WinitKey, ModifiersState, NamedKey, PhysicalKey};

/// Modifier mask bits for a [`Chord`]: the only modifiers a binding can name.
const MOD_CMD: u8 = 1 << 0;
const MOD_CTRL: u8 = 1 << 1;
const MOD_ALT: u8 = 1 << 2;
const MOD_SHIFT: u8 = 1 << 3;

/// Normalized key portion of a [`Chord`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum KeyToken {
    /// A printable character key, lowercased (`a`, `=`, `[`).
    Char(char),
    /// A named non-printable key (Enter, Tab, Escape, F1, arrows, …).
    Named(NamedKey),
}

/// A normalized keyboard chord: a modifier mask plus a [`KeyToken`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Chord {
    mods: u8,
    key: KeyToken,
}

/// An app command a chord can be bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Open a new in-window tab (Cmd-T).
    NewTab,
    /// Reopen the most recently closed native tab (Cmd-Shift-T on macOS).
    ReopenClosedTab,
    /// Close the focused pane / tab (Cmd-W).
    CloseTab,
    /// Open a new window — a fresh process (Cmd-N).
    NewWindow,
    /// Cycle to the next tab (Cmd-Shift-).
    NextTab,
    /// Cycle to the previous tab (Cmd-Shift-).
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
    /// Paste the PRIMARY selection — the text last selected, what a middle click pastes (X11 and Wayland) — or the clipboard where there is none (macOS, Windows).
    PasteSelection,
    /// Enter Cmd-F find mode.
    Find,
    /// Grow the live font (Cmd-=).
    FontIncrease,
    /// Reset the font to the launch default (Cmd-0).
    FontReset,
    /// Shrink the live font (Cmd--).
    FontDecrease,
    /// Move keyboard focus to the pane on the left (seeded Alt+Left off macOS; macOS has the hardcoded Cmd-Opt-Left, `App::on_key_pane_focus`, which is not this binding and always claims its chord).
    FocusPaneLeft,
    /// Move keyboard focus to the pane on the right (seeded Alt+Right off macOS, hardcoded Cmd-Opt-Right on macOS), or pass the key through ([`Action::FocusPaneLeft`]).
    FocusPaneRight,
    /// Move keyboard focus to the pane above (seeded Alt+Up off macOS, hardcoded Cmd-Opt-Up on macOS), or pass the key through ([`Action::FocusPaneLeft`]).
    FocusPaneUp,
    /// Move keyboard focus to the pane below (seeded Alt+Down off macOS, hardcoded Cmd-Opt-Down on macOS), or pass the key through ([`Action::FocusPaneLeft`]).
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
    /// Jump the viewport to the previous (older) shell prompt: the nearest OSC-133 command mark above the top visible row (default Ctrl-Shift-Up off macOS).
    JumpPrevPrompt,
    /// Jump the viewport to the next (newer) shell prompt (default Ctrl-Shift-Down off macOS).
    JumpNextPrompt,
    /// Focus or create the native Settings tab (default Ctrl+Shift+S off macOS).
    ToggleSettings,
    /// Open the native Settings app's About route.
    ToggleAbout,
    /// Toggle matrix rain for the front session of the focused window (`App::toggle_matrix_rain`): a per-session override that beats the `[matrix_rain]` config bit in either direction.
    ToggleMatrixRain,
    /// Toggle process-wide serious mode, suppressing sound, trails, and all decorative effects while preserving their requested settings.
    ToggleSeriousMode,
    /// Toggle the command palette overlay (`App::toggle_palette`; default Ctrl+Shift+P off macOS, ⇧⌘P via the menu on macOS).
    OpenPalette,
    /// Open the command palette scoped to its Theme section (View ▸ Choose Theme… — `App::open_theme_palette`): the highlighted theme is previewed live, Enter keeps it, Esc reverts.
    ChooseTheme,
    /// Edit the focused pane's session pin in place on the tab strip (Window ▸ Rename Session…, or double-clicking a tab).
    RenameSession,
    /// Select the visible screen as whole lines (Edit ▸ Select All, `App::select_all`).
    SelectAll,
    /// Toggle borderless full screen (View ▸ Enter Full Screen, `App::toggle_fullscreen`).
    ToggleFullscreen,
    /// Find next: step an open find bar forward, or (bar closed) resume the last accepted query after it (`App::search_find_again`).
    FindNext,
    /// Find previous: the backward twin of [`Action::FindNext`].
    FindPrev,
    /// Open the focused session's ledger (`crate::ledger_key`): runs `aterm drive ledger @<sid> --format html` and opens the file.
    OpenLedger,
}

impl Action {
    /// Whether the action pastes — the clipboard or the PRIMARY selection.
    #[must_use]
    pub(crate) const fn is_paste(self) -> bool {
        matches!(self, Action::Paste | Action::PasteSelection)
    }
}

/// Every bindable action name, in a stable order (printed by `--list-actions`).
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
    "paste_selection",
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
    "choose_theme",
    "rename_session",
    "select_all",
    "toggle_fullscreen",
    "find_next",
    "find_prev",
    "open_ledger",
];

/// Built-in Cmd-* shortcuts hardcoded in `App::on_key` and its helpers, as (chord-string, label).
pub(crate) const BUILTIN_CMD_CHORDS: &[(&str, &str)] = &[
    ("cmd+c", "Copy"),
    ("cmd+v", "Paste"),
    ("cmd+f", "Find…"),
    ("cmd+s", "Search Forward"),
    ("cmd+r", "Search Backward"),
    ("cmd+n", "New Window"),
    ("cmd+t", "New Terminal Tab"),
    ("cmd+w", "Close Pane or Tab"),
    ("cmd+q", "Quit aterm"),
    ("cmd+shift+t", "Reopen Closed Tab"),
    ("cmd+shift+n", "Move Tab to New Window"),
    ("cmd+shift+m", "Move Tab to Next Window"),
    ("cmd+shift+o", "Open Session in New Window"),
    ("cmd+d", "Split Right"),
    ("cmd+shift+d", "Split Down"),
    ("cmd+shift+l", "Harness Log for This Tab"),
    ("cmd+shift+]", "Show Next Tab"),
    ("cmd+shift+[", "Show Previous Tab"),
    ("cmd+shift+enter", "Toggle Pane Zoom"),
    ("cmd+alt+left", "Focus Pane Left"),
    ("cmd+alt+right", "Focus Pane Right"),
    ("cmd+alt+up", "Focus Pane Up"),
    ("cmd+alt+down", "Focus Pane Down"),
    ("cmd+down", "Scroll to Live"),
    ("cmd+up", "Scroll to Top"),
    ("cmd+=", "Increase Font Size"),
    ("cmd+shift+=", "Increase Font Size"),
    ("cmd+-", "Decrease Font Size"),
    ("cmd+0", "Actual Size"),
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

/// The macOS menu bar's other key equivalents: shortcuts `on_key` never sees, because AppKit hands a key equivalent to the menu before the window gets the key.
pub(crate) const MENU_CMD_CHORDS: &[(&str, &str)] = &[
    ("cmd+,", "Settings…"),
    ("cmd+o", "Open File…"),
    ("cmd+a", "Select All"),
    ("cmd+g", "Find Next"),
    ("cmd+shift+g", "Find Previous"),
    ("cmd+ctrl+f", "Enter Full Screen"),
    ("cmd+ctrl+o", "Overview…"),
    ("cmd+shift+j", "Next That Needs You"),
    ("cmd+shift+p", "Command Palette…"),
    ("cmd+shift+r", "Rewind Pane"),
    ("cmd+m", "Minimize"),
];

/// Whether this build has the macOS menu bar, whose key equivalents ([`MENU_CMD_CHORDS`]) are built-ins too.
pub(crate) const MENU_BAR_CHORDS: bool = cfg!(target_os = "macos");

/// Every built-in Cmd shortcut this build has, in `--list-keybinds` order: [`BUILTIN_CMD_CHORDS`], then [`MENU_CMD_CHORDS`] where `menu_bar` holds.
pub(crate) fn builtin_cmd_chords(
    menu_bar: bool,
) -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    let menu: &'static [(&str, &str)] = if menu_bar { MENU_CMD_CHORDS } else { &[] };
    BUILTIN_CMD_CHORDS.iter().chain(menu)
}

/// The built-in Cmd shortcut label `chord_str` conflicts with, if any (a macOS menu key equivalent reaches the menu first, so there the rule is the one shadowed).
#[must_use]
pub(crate) fn builtin_shadow_label(chord_str: &str) -> Option<&'static str> {
    builtin_shadow_label_when(
        chord_str,
        crate::app_input::HARDCODED_SUPER_CHORDS,
        MENU_BAR_CHORDS,
    )
}

fn builtin_shadow_label_when(
    chord_str: &str,
    suite_live: bool,
    menu_bar: bool,
) -> Option<&'static str> {
    if !suite_live {
        return None;
    }
    let target = Chord::parse(chord_str).ok()?;
    builtin_cmd_chords(menu_bar)
        .find(|(c, _)| Chord::parse(c).is_ok_and(|c| c == target))
        .map(|(_, label)| *label)
}

/// True if `chord_str` is also bound in `[keybindings]`, which `on_key` consults first, so a `[key_sequences]` rule on the same chord never fires.
#[must_use]
pub(crate) fn chord_in_keybindings(
    chord_str: &str,
    keybindings: &std::collections::BTreeMap<String, String>,
) -> bool {
    let Ok(target) = Chord::parse(chord_str) else {
        return false;
    };
    keybindings
        .iter()
        .any(|(k, v)| !is_unbind_action(v) && Chord::parse(k).is_ok_and(|c| c == target))
}

impl Action {
    /// Parse an action name (the value side of a `[keybindings]` entry); `switch_tab_<n>` carries the 1-based target.
    #[must_use]
    pub fn parse(name: &str) -> Option<Action> {
        let n = name.trim();
        if let Some(rest) = n.strip_prefix("switch_tab_") {
            if !rest.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
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
            "paste_selection" | "paste_primary" => Action::PasteSelection,
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
            "choose_theme" => Action::ChooseTheme,
            "rename_session" => Action::RenameSession,
            "select_all" => Action::SelectAll,
            "fullscreen" | "toggle_fullscreen" => Action::ToggleFullscreen,
            "find_next" => Action::FindNext,
            "find_prev" | "find_previous" => Action::FindPrev,
            "open_ledger" => Action::OpenLedger,
            _ => return None,
        })
    }

    /// The direction a `focus_pane_*` action moves focus; `None` for every other action.
    #[must_use]
    pub(crate) const fn pane_focus_dir(self) -> Option<crate::pane::FocusDir> {
        use crate::pane::FocusDir;
        match self {
            Action::FocusPaneLeft => Some(FocusDir::Left),
            Action::FocusPaneRight => Some(FocusDir::Right),
            Action::FocusPaneUp => Some(FocusDir::Up),
            Action::FocusPaneDown => Some(FocusDir::Down),
            _ => None,
        }
    }
}

/// Whether a `[keybindings]` value spells "unbind this chord" (`"none"` or `"unbind"`).
#[must_use]
pub(crate) fn is_unbind_action(name: &str) -> bool {
    matches!(name.trim(), "none" | "unbind")
}

fn modifier_bit(word: &str) -> Option<u8> {
    Some(match word {
        "cmd" | "command" | "super" | "win" | "meta" => MOD_CMD,
        "ctrl" | "control" => MOD_CTRL,
        "alt" | "opt" | "option" => MOD_ALT,
        "shift" => MOD_SHIFT,
        _ => return None,
    })
}

fn key_token(word: &str) -> Option<KeyToken> {
    let mut chars = word.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        return Some(KeyToken::Char(first.to_ascii_lowercase()));
    }
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

const SHIFT_COMPOSED_ASCII: &[char] = &[
    '~', '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '_', '{', '}', '|', ':', '"', '<', '>',
    '?',
];

/// The base-key spelling of a shift-composed glyph on a US layout, for the caution's "write it as" half.
#[must_use]
pub(crate) fn unshifted_us_spelling(c: char) -> Option<char> {
    let pairs = [
        ('~', '`'),
        ('!', '1'),
        ('@', '2'),
        ('#', '3'),
        ('$', '4'),
        ('%', '5'),
        ('^', '6'),
        ('&', '7'),
        ('*', '8'),
        ('(', '9'),
        (')', '0'),
        ('_', '-'),
        ('{', '['),
        ('}', ']'),
        ('|', '\\'),
        (':', ';'),
        ('"', '\''),
        ('<', ','),
        ('>', '.'),
        ('?', '/'),
    ];
    pairs
        .iter()
        .find_map(|(shifted, base)| (*shifted == c).then_some(*base))
}

/// Whether `chord` names a key that a live press can never produce on a US/UK layout — see [`SHIFT_COMPOSED_ASCII`].
#[must_use]
pub(crate) fn shift_composed_key(chord: &Chord) -> Option<char> {
    match chord.key {
        KeyToken::Char(c) if SHIFT_COMPOSED_ASCII.contains(&c) => Some(c),
        _ => None,
    }
}

impl Chord {
    /// Parse a chord string (the key side of a `[keybindings]` entry), e.g. `"cmd+shift+t"`.
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
                return Err("write the + key as plus".to_string());
            }
            if key.is_some() {
                return Err("the key must come last".to_string());
            }
            if let Some(bit) = modifier_bit(seg) {
                mods |= bit;
            } else if let Some(tok) = key_token(seg) {
                key = Some(tok);
            } else {
                return Err(format!("unknown key or modifier {seg:?}"));
            }
        }
        let key = key.ok_or_else(|| "no key".to_string())?;
        Ok(Chord { mods, key })
    }

    /// Build the chord a live winit key event represents.
    #[must_use]
    pub(crate) fn from_event(logical: &WinitKey, mods: ModifiersState) -> Option<Chord> {
        let key = match logical {
            WinitKey::Character(s) => {
                let mut chars = s.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                KeyToken::Char(c.to_ascii_lowercase())
            }
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

    /// The chords a live key press spells, in match order: its layout key ([`Self::from_event`]), then its [`chord_layout_twin`] — so on a Russian layout Ctrl+Shift+'с' is tried as `ctrl+shift+с` (a binding the user wrote in Cyrillic still wins) and then as `ctrl+shift+c` (Copy).
    #[must_use]
    pub(crate) fn from_press(
        logical: &WinitKey,
        physical: PhysicalKey,
        mods: ModifiersState,
    ) -> [Option<Chord>; 2] {
        let exact = Chord::from_event(logical, mods);
        let twin = exact.as_ref().and_then(|chord| {
            chord_layout_twin(logical, physical, mods).map(|twin| Chord {
                mods: chord.mods,
                key: KeyToken::Char(twin),
            })
        });
        [exact, twin]
    }

    /// Human-readable presentation of this chord (`Ctrl+Shift+C`, `F11`, `Shift+Insert`), the accelerator hint the palette/menu rows show off macOS.
    #[must_use]
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
            KeyToken::Char('+') => out.push_str("Plus"),
            KeyToken::Char(c) => out.push(c.to_ascii_uppercase()),
            KeyToken::Named(named) => out.push_str(named_key_display(*named)),
        }
        out
    }
}

/// Display name of a parseable [`NamedKey`]: the reverse of [`key_token`]'s named
/// table, in the spelling that re-parses.
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

/// The US-QWERTY letter a shortcut press on a non-Latin layout names, or `None`: `Some('c')` for Ctrl+Shift+Russian 'с' on the physical C key.
#[must_use]
pub(crate) fn chord_layout_twin(
    logical: &WinitKey,
    physical: PhysicalKey,
    mods: ModifiersState,
) -> Option<char> {
    if !(mods.control_key() || mods.alt_key() || mods.super_key()) {
        return None;
    }
    aterm_winit_keymap::layout_letter_twin_for(logical, physical)
}

#[cfg(any(not(target_os = "macos"), test))]
macro_rules! non_macos_default_pairs {
    (@build [$($paste:expr),+ $(,)?], $zoom:expr, [$($pre_tab:expr),* $(,)?], [$($post_tab:expr),* $(,)?]) => {
        &[
            ("ctrl+shift+c", "copy"),
            ("ctrl+shift+v", "paste"),
            $($paste,)+
            ("ctrl+insert", "copy"),
            ("ctrl+shift+t", "new_tab"),
            ("ctrl+shift+w", "close_tab"),
            ("ctrl+shift+n", "new_window"),
            ("ctrl+shift+f", "find"),
            ("ctrl+shift+d", "split_vertical"),
            ("ctrl+shift+e", "split_horizontal"),
            $zoom,
            ("alt+left", "focus_pane_left"),
            ("alt+right", "focus_pane_right"),
            ("alt+up", "focus_pane_up"),
            ("alt+down", "focus_pane_down"),
            $($pre_tab,)*
            ("ctrl+pagedown", "next_tab"),
            ("ctrl+pageup", "prev_tab"),
            $($post_tab,)*
            ("ctrl+shift+up", "jump_prev_prompt"),
            ("ctrl+shift+down", "jump_next_prompt"),
            ("ctrl+=", "font_increase"),
            ("ctrl+shift+=", "font_increase"),
            ("ctrl+plus", "font_increase"),
            ("ctrl+-", "font_decrease"),
            ("ctrl+0", "font_reset"),
            ("ctrl+shift+s", "toggle_settings"),
            ("ctrl+,", "toggle_settings"),
            ("ctrl+shift+a", "select_all"),
            ("ctrl+shift+p", "open_palette"),
            ("ctrl+shift+l", "open_ledger"),
            ("f11", "toggle_fullscreen"),
        ]
    };
    (windows) => {
        non_macos_default_pairs!(
            @build
            [("ctrl+v", "paste"), ("shift+insert", "paste")],
            ("ctrl+shift+z", "toggle_pane_zoom"),
            [("ctrl+tab", "next_tab"), ("ctrl+shift+tab", "prev_tab")],
            []
        )
    };
    (linux) => {
        non_macos_default_pairs!(
            @build
            [("shift+insert", "paste_selection"), ("ctrl+shift+insert", "paste")],
            ("ctrl+shift+return", "toggle_pane_zoom"),
            [("ctrl+shift+right", "next_tab"), ("ctrl+shift+left", "prev_tab")],
            [("ctrl+tab", "next_tab"), ("ctrl+shift+tab", "prev_tab")]
        )
    };
    (other) => {
        non_macos_default_pairs!(
            @build
            [("shift+insert", "paste")],
            ("ctrl+shift+return", "toggle_pane_zoom"),
            [("ctrl+shift+right", "next_tab"), ("ctrl+shift+left", "prev_tab")],
            [("ctrl+tab", "next_tab"), ("ctrl+shift+tab", "prev_tab")]
        )
    };
}

/// Chord-to-action map combining platform defaults and `[keybindings]` overrides.
#[derive(Clone, Debug, Default)]
pub(crate) struct Keybindings {
    map: HashMap<Chord, Action>,
}

impl Keybindings {
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

    /// Build from the raw config table (chord-string → action-name).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_config(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Keybindings {
        let (map, _unbinds, warns) = Self::collect(table);
        for w in &warns {
            crate::logging::stderr_line!("aterm-gui: {w}");
        }
        Keybindings { map }
    }

    /// [`Self::from_config`], returning the warnings instead of printing them.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_config_warn(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (Keybindings, Vec<String>) {
        let (map, _unbinds, warns) = Self::collect(table);
        (Keybindings { map }, warns)
    }

    /// The (chord, action-name) seed table behind [`Self::platform_defaults`], parsed by the same machinery as user config; empty on macOS.
    #[cfg(target_os = "macos")]
    pub(crate) const PLATFORM_DEFAULT_PAIRS: &'static [(&'static str, &'static str)] = &[];
    #[cfg(windows)]
    pub(crate) const PLATFORM_DEFAULT_PAIRS: &'static [(&'static str, &'static str)] =
        non_macos_default_pairs!(windows);
    #[cfg(target_os = "linux")]
    pub(crate) const PLATFORM_DEFAULT_PAIRS: &'static [(&'static str, &'static str)] =
        non_macos_default_pairs!(linux);
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    pub(crate) const PLATFORM_DEFAULT_PAIRS: &'static [(&'static str, &'static str)] =
        non_macos_default_pairs!(other);

    fn from_pairs(pairs: &[(&str, &str)]) -> Keybindings {
        let mut map = HashMap::new();
        for &(chord_str, action_str) in pairs {
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

    /// The platform's built-in keybindings. macOS has none: the hardcoded Cmd chords in `on_key` are the convention there.
    #[must_use]
    pub(crate) fn platform_defaults() -> Keybindings {
        Self::from_pairs(Self::PLATFORM_DEFAULT_PAIRS)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn macos_defaults_for_test() -> Keybindings {
        Self::from_pairs(&[])
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn linux_defaults_for_test() -> Keybindings {
        Self::from_pairs(non_macos_default_pairs!(linux))
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn windows_defaults_for_test() -> Keybindings {
        Self::from_pairs(non_macos_default_pairs!(windows))
    }

    /// The effective keybindings: [`Self::platform_defaults`] with `[keybindings]` overlaid (a user entry wins).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn resolved(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Keybindings {
        Self::resolved_warn(table).0
    }

    /// Like `Self::resolved` but returns the user-table warnings (seeds never warn).
    #[must_use]
    pub(crate) fn resolved_warn(
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

    /// Whether no bindings are configured (the macOS no-config default).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// O(1) lookup: the [`Action`] bound to a live key press, or `None` (miss → fall through to the hardcoded `on_key` matches).
    #[must_use]
    pub(crate) fn action_for(
        &self,
        logical: &WinitKey,
        physical: PhysicalKey,
        mods: ModifiersState,
    ) -> Option<Action> {
        if self.map.is_empty() {
            return None;
        }
        Chord::from_press(logical, physical, mods)
            .into_iter()
            .flatten()
            .find_map(|chord| self.map.get(&chord).copied())
    }

    /// [`Self::action_for`] with no physical key (so no layout twin): the tests' spelling of a chord.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn lookup(&self, logical: &WinitKey, mods: ModifiersState) -> Option<Action> {
        self.action_for(logical, NO_PHYSICAL_KEY, mods)
    }

    /// The DISPLAY chord for `action` in this EFFECTIVE map, if any — the accelerator hint the palette's menu rows show off macOS, where the ⌘ key-equivalents are blanked.
    #[must_use]
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

/// User `[key_sequences]` chord-to-raw-bytes map.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeySequences {
    map: HashMap<Chord, Vec<u8>>,
}

impl KeySequences {
    /// Build from the raw config table (chord-string -> byte-string).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_config(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> KeySequences {
        let (map, warns) = Self::collect(table);
        for w in &warns {
            crate::logging::stderr_line!("aterm-gui: {w}");
        }
        KeySequences { map }
    }

    /// Like `Self::from_config` but returns the warnings, so the GUI can surface dropped rules.
    #[must_use]
    pub(crate) fn from_config_warn(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> (KeySequences, Vec<String>) {
        let (map, warns) = Self::collect(table);
        (KeySequences { map }, warns)
    }

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

    /// Whether no sequences are configured (the default) — lets `on_key` skip the chord-build entirely on the hot path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The raw bytes a key chord is bound to, or `None` (miss -> fall through to the default encoder).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn lookup(&self, logical: &WinitKey, mods: ModifiersState) -> Option<&[u8]> {
        let chord = Chord::from_event(logical, mods)?;
        self.map.get(&chord).map(Vec::as_slice)
    }
}

/// No physical key: a chord the tests spell, which has no layout twin.
#[cfg(test)]
const NO_PHYSICAL_KEY: PhysicalKey =
    PhysicalKey::Unidentified(aterm_window::keyboard::NativeKeyCode::Unidentified);

/// The outcome of consulting the two user chord maps for a key event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChordResolution {
    Action(Action),
    Sequence(Vec<u8>),
    FallThrough,
}

/// Resolve a live key press against the user's two chord maps, applying the keybindings-before-key_sequences precedence.
#[must_use]
pub(crate) fn resolve_chord(
    base: &WinitKey,
    physical: PhysicalKey,
    mods: ModifiersState,
    kb: &Keybindings,
    ks: &KeySequences,
    mut pane_focus_moves: impl FnMut(crate::pane::FocusDir) -> bool,
) -> ChordResolution {
    if kb.map.is_empty() && ks.map.is_empty() {
        return ChordResolution::FallThrough;
    }
    for chord in Chord::from_press(base, physical, mods)
        .into_iter()
        .flatten()
    {
        if let Some(&action) = kb.map.get(&chord) {
            match action.pane_focus_dir() {
                Some(dir) if pane_focus_passes_through(base) && !pane_focus_moves(dir) => {}
                _ => return ChordResolution::Action(action),
            }
        }
        if let Some(bytes) = ks.map.get(&chord) {
            return ChordResolution::Sequence(bytes.clone());
        }
    }
    ChordResolution::FallThrough
}

/// Whether a `focus_pane_*` binding on `base` may give its press to the program when
/// no pane lies that way ([`resolve_chord`]): only on an arrow key.
#[must_use]
const fn pane_focus_passes_through(base: &WinitKey) -> bool {
    matches!(
        base,
        WinitKey::Named(
            NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::ArrowUp | NamedKey::ArrowDown
        )
    )
}

/// Upper bound on a `[key_sequences]` value, in bytes.
pub(crate) const MAX_KEY_SEQUENCE_BYTES: usize = 1024;

/// Expand a config byte-string (a `[key_sequences]` value) into the exact bytes to send to the PTY.
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
                if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(format!("bad \\u{{{hex}}}"));
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
#[path = "keybinding_tests.rs"]
mod tests;
