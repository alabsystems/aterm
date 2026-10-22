// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Taskbar progress and desktop notification types.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Taskbar progress state for ConEmu OSC 9;4 (`ESC ] 9 ; 4 ; <state> ; <progress> ST`),
/// shown in the taskbar or dock.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TaskbarProgress {
    /// Progress is hidden (state 0).
    Hidden,
    /// Normal progress display (state 1). Value is 0-100.
    Normal(u8),
    /// Error state (state 2). Value is 0-100.
    Error(u8),
    /// Indeterminate progress (state 3).
    Indeterminate,
    /// Paused state (state 4). Value is 0-100.
    Paused(u8),
}

/// Notification urgency level (OSC 99 `u` parameter): 0 low, 1 normal (the default), 2
/// critical.
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotificationUrgency {
    /// Low urgency (u=0). Notification may be suppressed if user is busy.
    Low,
    /// Normal urgency (u=1). Standard notification behavior.
    #[default]
    Normal,
    /// Critical urgency (u=2). Notification should persist until acknowledged.
    Critical,
}

impl NotificationUrgency {
    /// Parse urgency from OSC 99 'u' parameter value.
    #[inline]
    #[must_use]
    pub fn from_param(value: &str) -> Self {
        match value {
            "0" => Self::Low,
            "2" => Self::Critical,
            _ => Self::Normal,
        }
    }
}

/// Desktop notification (OSC 99, kitty protocol), aggregated from the OSC 99 sequences that
/// share a notification ID.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Notification {
    /// Notification ID for updates and close events.
    pub id: Option<String>,
    /// Notification title (short summary).
    pub title: Option<String>,
    /// Notification body (detailed message).
    pub body: Option<String>,
    /// Urgency level (low, normal, critical).
    pub urgency: NotificationUrgency,
}

impl Notification {
    /// Check if this notification has any content to display.
    #[inline]
    #[must_use]
    pub const fn has_content(&self) -> bool {
        self.title.is_some() || self.body.is_some()
    }
}
