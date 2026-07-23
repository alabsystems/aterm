// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Callback type definitions for terminal events.
//!
//! These type aliases define the function signatures for callbacks the Terminal
//! uses to notify the UI layer of various events (title changes, clipboard
//! operations, buffer switches, etc.).
//!
//! Extracted from `aterm-core::terminal::callbacks` (Part of #5663, Phase 2).

use std::sync::Arc;

use crate::{
    ClipboardOperation, CopyToClipboardOperation, Iterm2CellSize, Iterm2SetColor,
    Iterm2ShellIntegrationVersion, MultipartFileOperation, Notification, RemoteHost, Rgb,
    SemanticBlockEvent, SemanticButtonEvent, ShellEvent, TextSizingOperation, WindowOperation,
    WindowResponse,
};

/// Which type of title operation was requested.
///
/// Maps to OSC 0 (icon+window), OSC 1 (icon only), OSC 2 (window only).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleType {
    /// OSC 0 — set both icon name and window title.
    WindowAndIcon,
    /// OSC 1 — set icon name only.
    IconOnly,
    /// OSC 2 — set window title only.
    WindowOnly,
}

/// Callback type for title changes (legacy, window-only).
pub type TitleCallback = Box<dyn FnMut(&str) + Send>;

/// Callback type for title change events with type discriminator.
///
/// Called when the terminal title changes via OSC 0, 1, or 2.
/// The callback receives the title type and the text.
pub type TitleEventCallback = Box<dyn FnMut(TitleType, &str) + Send>;

/// Callback type for simple desktop notifications (OSC 9).
///
/// Called when the terminal receives a simple notification escape sequence.
/// The UI layer should display a system notification with the message.
pub type NotificationCallback = Box<dyn FnMut(&str) + Send>;

/// Callback type for profile change requests (OSC 1337 SetProfile).
///
/// Called when the terminal receives an OSC 1337 SetProfile=name sequence.
/// The host application decides how to handle profile switching.
pub type SetProfileCallback = Box<dyn FnMut(&str) + Send>;

/// Callback type for badge format requests (OSC 1337 SetBadgeFormat).
///
/// Called when the terminal receives an OSC 1337 SetBadgeFormat=base64 sequence.
/// The decoded format string is passed to the callback. The format can contain
/// Terminal variables like `\(session.name)`.
pub type SetBadgeFormatCallback = Box<dyn FnMut(&str) + Send>;

/// Callback type for Terminal SetColors requests (OSC 1337 SetColors).
///
/// Called when OSC 1337 SetColors=key=value is received.
/// The host can use this to update UI-specific color state.
pub type SetColorsCallback = Box<dyn FnMut(Iterm2SetColor) + Send>;

/// Callback type for Terminal cursor line highlight requests (OSC 1337 HighlightCursorLine).
///
/// The boolean parameter is `true` to enable the highlight, `false` to disable.
pub type HighlightCursorLineCallback = Box<dyn FnMut(bool) + Send>;

/// Callback type for Terminal ReportVariable queries (OSC 1337 ReportVariable).
///
/// The callback receives the decoded variable name and returns an optional value.
/// Returning `None` indicates no response should be sent.
pub type ReportVariableCallback = Box<dyn FnMut(&str) -> Option<String> + Send>;

/// Callback type for Terminal ReportCellSize queries (OSC 1337 ReportCellSize).
///
/// The callback returns a cell size in points, or `None` to ignore the query.
pub type ReportCellSizeCallback = Box<dyn FnMut() -> Option<Iterm2CellSize> + Send>;

/// Callback type for Terminal shell integration version reports (OSC 1337 ShellIntegrationVersion).
///
/// Called when the shell reports its integration version and name.
pub type ShellIntegrationVersionCallback = Box<dyn FnMut(Iterm2ShellIntegrationVersion) + Send>;

/// Operation kind for dynamic color change callbacks.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChangeOp {
    /// The sequence set an explicit color value.
    Set,
    /// The sequence reset the color to its default value.
    Reset,
}

/// Callback type for dynamic color changes (OSC 10/11/12, OSC 110/111/112).
///
/// Called when the terminal's default foreground, background, or cursor color
/// changes via escape sequences. The `u8` parameter indicates which color changed:
/// - `0`: foreground (OSC 10, OSC 110 reset)
/// - `1`: background (OSC 11, OSC 111 reset)
/// - `2`: cursor (OSC 12, OSC 112 reset)
///
/// The `Rgb` parameter is the resulting color value. `ColorChangeOp` tells the
/// consumer whether the sequence set an explicit value or reset back to default.
pub type ColorChangeCallback = Box<dyn FnMut(u8, Rgb, ColorChangeOp) + Send>;

/// Callback type for advanced desktop notifications (OSC 99 - kitty protocol).
///
/// Called when the terminal receives a complete notification via OSC 99.
/// The notification may include title, body, urgency, and an ID for updates.
pub type AdvancedNotificationCallback = Box<dyn FnMut(Notification) + Send>;

/// Callback type for DCS sequences.
pub type DcsCallback = Box<dyn FnMut(&[u8], u8) + Send>;

/// Callback type for buffer activation events.
///
/// Called when switching between the main and alternate screen buffers.
/// The boolean parameter is `true` when switching to the alternate screen,
/// `false` when switching back to the main screen.
pub type BufferActivationCallback = Box<dyn FnMut(bool) + Send>;

/// A decoded Kitty graphics protocol image ready for display.
///
/// Bundles the image metadata and pixel data into a single struct,
/// replacing the previous 4-parameter callback signature.
#[derive(Debug, Clone)]
pub struct KittyImageData {
    /// Unique image identifier from the Kitty protocol.
    pub id: u32,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Raw RGBA pixel data (4 bytes per pixel, length = width * height * 4).
    ///
    /// Shared via `Arc<[u8]>` for zero-copy access. The image remains stored
    /// in the terminal's Kitty graphics storage.
    pub data: Arc<[u8]>,
}

/// Called when a Kitty graphics image is received and stored.
///
/// The callback receives a [`KittyImageData`] with the image ID, dimensions,
/// and RGBA pixel data. Invoked after successful image transmission
/// (action=t,T,q+t).
pub type KittyImageCallback = Box<dyn FnMut(KittyImageData) + Send>;

/// Callback type for clipboard operations (OSC 52).
///
/// The callback receives the clipboard operation and should return the clipboard
/// content for query operations (or None if clipboard access is denied/unavailable).
/// For set operations, the return value is ignored.
pub type ClipboardCallback = Box<dyn FnMut(ClipboardOperation) -> Option<String> + Send>;

/// Callback type for window operations (CSI t - XTWINOPS).
///
/// The callback receives the window operation and should return a response
/// for report operations. For manipulation operations (iconify, move, etc.),
/// the return value is ignored.
pub type WindowCallback = Box<dyn FnMut(WindowOperation) -> Option<WindowResponse> + Send>;

/// Callback type for OSC 1337 named pasteboard operations.
///
/// Called when OSC 1337 CopyToClipboard/EndCopy sequences complete text capture,
/// or when a direct Copy=base64 sequence is received.
pub type CopyToClipboardCallback = Box<dyn FnMut(CopyToClipboardOperation) + Send>;

/// Callback type for OSC 1337 multipart file transfers.
///
/// Called when a multipart file transfer completes (successfully or with failure).
pub type MultipartFileCallback = Box<dyn FnMut(MultipartFileOperation) + Send>;

/// Callback type for remote host change events (OSC 1337 RemoteHost).
///
/// Called when the remote host changes (SSH connect or disconnect).
/// The parameter is `None` when returning to a local session.
pub type RemoteHostCallback = Box<dyn FnMut(Option<&RemoteHost>) + Send>;

/// Callback type for text sizing events (OSC 66 - Kitty protocol).
///
/// Called when a text sizing sequence is received. The callback receives
/// the parsed text sizing operation including scale, width, alignment,
/// and the text content.
pub type TextSizingCallback = Box<dyn FnMut(TextSizingOperation) + Send>;

/// Callback type for semantic block events (OSC 1337 Block/UpdateBlock).
///
/// Called when blocks are opened, closed, or their fold state changes.
/// The UI can use this to render code block decorations with fold controls.
pub type SemanticBlockCallback = Box<dyn FnMut(SemanticBlockEvent) + Send>;

/// Callback type for semantic button events (OSC 1337 Button).
///
/// Called when buttons are created or all custom buttons are disabled.
/// The UI can use this to render copy buttons on code blocks or custom
/// interactive buttons.
pub type SemanticButtonCallback = Box<dyn FnMut(SemanticButtonEvent) + Send>;

/// Callback type for shell integration events.
pub type ShellCallback = Box<dyn FnMut(ShellEvent) + Send>;

/// Callback type for generic OSC 1337 key-value pair pass-through.
///
/// Called for ALL OSC 1337 commands before internal processing. The callback
/// receives the parsed command name and optional value. Return `true` to
/// continue with aterm-core's internal handling, `false` to skip it.
///
/// This covers all 34+ Terminal OSC 1337 sub-commands with a single callback,
/// allowing the host to intercept or override any of them.
pub type KvpCallback = Box<dyn FnMut(&str, Option<&str>) -> bool + Send>;
