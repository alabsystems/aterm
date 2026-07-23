// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Callback registry — API discoverability metadata (#1649).
//!
//! Static registry of all terminal callbacks with names, setter methods,
//! triggering events, and type signatures. Extracted from aterm-core (#5663).

/// Category of callback functionality.
///
/// Used to group related callbacks for documentation and discovery.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CallbackCategory {
    /// UI events (bell, title, window ops, notifications).
    Ui = 0,
    /// Clipboard operations (read/write).
    Clipboard = 1,
    /// Shell integration events.
    Shell = 2,
    /// Graphics (images, file transfer).
    Graphics = 3,
    /// External protocols (tmux, SSH conductor, DCS).
    Protocol = 4,
}

/// Metadata describing a terminal callback.
///
/// This struct provides discoverability information for terminal callbacks,
/// allowing consumers to programmatically discover available callbacks.
#[derive(Debug, Clone, Copy)]
pub struct CallbackInfo {
    /// Callback name (e.g., "bell").
    pub name: &'static str,
    /// Setter method name (e.g., "set_bell_callback").
    pub setter: &'static str,
    /// Event that triggers this callback.
    pub event: &'static str,
    /// Type signature (human-readable).
    pub signature: &'static str,
    /// Category of functionality.
    pub category: CallbackCategory,
    /// Whether callback must be Send (always true for Terminal callbacks).
    pub thread_safe: bool,
}

/// Terminal callback registry.
///
/// # Available Callbacks
///
/// All callbacks are set via `Terminal::set_<name>_callback()` methods.
/// See individual callback documentation for event details and parameters.
///
/// # Lifecycle Rules
///
/// 1. **Synchronous invocation**: Callbacks run during `process()` calls
/// 2. **No reentrancy**: Do not call Terminal methods from callbacks (deadlock risk)
/// 3. **Thread safety**: All callbacks require `Send` bound
/// 4. **Ownership**: String/slice parameters are borrowed for the callback duration
///
/// # Categories
///
/// - **UI**: Visual events (bell, title, window, notifications)
/// - **Clipboard**: Copy/paste operations
/// - **Shell**: Shell integration (prompt detection, command events)
/// - **Graphics**: Images and file transfer
/// - **Protocol**: tmux control mode, SSH conductor, DCS passthrough
///
/// # Example
///
/// ```rust
/// use aterm_types::{callback_by_name, callback_count};
///
/// assert!(callback_count() > 0);
/// let bell = callback_by_name("bell").expect("bell callback metadata should exist");
/// assert_eq!(bell.name, "bell");
/// ```
pub const CALLBACK_REGISTRY: &[CallbackInfo] = &[
    // UI Category
    CallbackInfo {
        name: "bell",
        setter: "set_bell_callback",
        event: "BEL character received",
        signature: "FnMut()",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "cursor_style",
        setter: "set_cursor_style_callback",
        event: "DECSCUSR cursor style change (CSI Ps SP q)",
        signature: "FnMut(CursorStyle)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "buffer_activation",
        setter: "set_buffer_activation_callback",
        event: "Alternate screen buffer toggle (DECSET 1049, etc.)",
        signature: "FnMut(bool)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "title",
        setter: "set_title_callback",
        event: "OSC 0/2 window title change",
        signature: "FnMut(&str)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "title_event",
        setter: "set_title_event_callback",
        event: "OSC 0/1/2 title change with type discriminator",
        signature: "FnMut(TitleType, &str)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "notification",
        setter: "set_notification_callback",
        event: "OSC 9 simple desktop notification",
        signature: "FnMut(&str)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "color_change",
        setter: "set_color_change_callback",
        event: "OSC dynamic color set/reset (fg/bg/cursor/palette/selection)",
        signature: "FnMut(u8, Rgb, ColorChangeOp)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "advanced_notification",
        setter: "set_advanced_notification_callback",
        event: "OSC 99 (kitty) rich notification",
        signature: "FnMut(Notification)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "window",
        setter: "set_window_callback",
        event: "CSI t (XTWINOPS) window manipulation",
        signature: "FnMut(WindowOperation) -> Option<WindowResponse>",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "remote_host",
        setter: "set_remote_host_callback",
        event: "OSC 1337 RemoteHost change (SSH connect/disconnect)",
        signature: "FnMut(Option<&RemoteHost>)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "text_sizing",
        setter: "set_text_sizing_callback",
        event: "OSC 66 (kitty) text sizing",
        signature: "FnMut(TextSizingOperation)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "profile",
        setter: "set_profile_callback",
        event: "OSC 1337 SetProfile=name",
        signature: "FnMut(&str)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "badge_format",
        setter: "set_badge_format_callback",
        event: "OSC 1337 SetBadgeFormat=base64",
        signature: "FnMut(&str)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "set_colors",
        setter: "set_colors_callback",
        event: "OSC 1337 SetColors=key=value",
        signature: "FnMut(Iterm2SetColor)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "highlight_cursor_line",
        setter: "set_highlight_cursor_line_callback",
        event: "OSC 1337 HighlightCursorLine=yes/no",
        signature: "FnMut(bool)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "report_variable",
        setter: "set_report_variable_callback",
        event: "OSC 1337 ReportVariable=base64",
        signature: "FnMut(&str) -> Option<String>",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "report_cell_size",
        setter: "set_report_cell_size_callback",
        event: "OSC 1337 ReportCellSize",
        signature: "FnMut() -> Option<Iterm2CellSize>",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    CallbackInfo {
        name: "shell_integration_version",
        setter: "set_shell_integration_version_callback",
        event: "OSC 1337 ShellIntegrationVersion=Pn;Ps",
        signature: "FnMut(Iterm2ShellIntegrationVersion)",
        category: CallbackCategory::Ui,
        thread_safe: true,
    },
    // Clipboard Category
    CallbackInfo {
        name: "clipboard",
        setter: "set_clipboard_callback",
        event: "OSC 52 clipboard read/write",
        signature: "FnMut(ClipboardOperation) -> Option<String>",
        category: CallbackCategory::Clipboard,
        thread_safe: true,
    },
    CallbackInfo {
        name: "copy_to_clipboard",
        setter: "set_copy_to_clipboard_callback",
        event: "OSC 1337 CopyToClipboard/EndCopy",
        signature: "FnMut(CopyToClipboardOperation)",
        category: CallbackCategory::Clipboard,
        thread_safe: true,
    },
    // Shell Category
    CallbackInfo {
        name: "shell",
        setter: "set_shell_callback",
        event: "Shell integration events (OSC 133/633)",
        signature: "FnMut(ShellEvent)",
        category: CallbackCategory::Shell,
        thread_safe: true,
    },
    // Graphics Category
    CallbackInfo {
        name: "kitty_image",
        setter: "set_kitty_image_callback",
        event: "Kitty graphics protocol image received",
        signature: "FnMut(u32, u32, u32, Arc<[u8]>)",
        category: CallbackCategory::Graphics,
        thread_safe: true,
    },
    CallbackInfo {
        name: "multipart_file",
        setter: "set_multipart_file_callback",
        event: "OSC 1337 multipart file transfer complete",
        signature: "FnMut(MultipartFileOperation)",
        category: CallbackCategory::Graphics,
        thread_safe: true,
    },
    // Protocol Category
    CallbackInfo {
        name: "tmux",
        setter: "set_tmux_callback",
        event: "tmux control mode notifications",
        signature: "FnMut(&TmuxNotification)",
        category: CallbackCategory::Protocol,
        thread_safe: true,
    },
    CallbackInfo {
        name: "ssh_conductor",
        setter: "set_ssh_conductor_callback",
        event: "SSH conductor mode events",
        signature: "FnMut(&SshConductorEvent)",
        category: CallbackCategory::Protocol,
        thread_safe: true,
    },
    CallbackInfo {
        name: "dcs",
        setter: "set_dcs_callback",
        event: "DCS passthrough sequence",
        signature: "FnMut(&[u8], u8)",
        category: CallbackCategory::Protocol,
        thread_safe: true,
    },
    CallbackInfo {
        name: "semantic_block",
        setter: "set_semantic_block_callback",
        event: "OSC 1337 Block/UpdateBlock events",
        signature: "FnMut(SemanticBlockEvent)",
        category: CallbackCategory::Protocol,
        thread_safe: true,
    },
    CallbackInfo {
        name: "semantic_button",
        setter: "set_semantic_button_callback",
        event: "OSC 1337 Button events",
        signature: "FnMut(SemanticButtonEvent)",
        category: CallbackCategory::Protocol,
        thread_safe: true,
    },
    CallbackInfo {
        name: "kvp",
        setter: "set_kvp_callback",
        event: "OSC 1337 generic KVP pass-through (all commands)",
        signature: "FnMut(&str, Option<&str>) -> bool",
        category: CallbackCategory::Protocol,
        thread_safe: true,
    },
];

/// Get the number of registered callbacks.
#[inline]
pub const fn callback_count() -> usize {
    CALLBACK_REGISTRY.len()
}

/// Get callback info by index.
///
/// Not a `const fn`: the total `slice::get` (which discharges the Trust gate's
/// bounds obligation, unlike a manual `index < len` guard) is not yet callable
/// in const context, and no caller needs this in a const expression.
#[inline]
#[must_use]
pub fn callback_info(index: usize) -> Option<&'static CallbackInfo> {
    CALLBACK_REGISTRY.get(index)
}

/// Find callback info by name.
pub fn callback_by_name(name: &str) -> Option<&'static CallbackInfo> {
    CALLBACK_REGISTRY.iter().find(|info| info.name == name)
}

#[cfg(test)]
#[path = "callback_registry_tests.rs"]
mod tests;
