// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Taskbar progress and desktop notification types.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Taskbar progress state for ConEmu OSC 9;4 (`ESC ] 9 ; 4 ; <state> ; <progress> ST`),
/// shown in the taskbar or dock.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TaskbarProgress {
    /// Hidden (state 0).
    Hidden,
    /// Normal progress (state 1, 0..=100).
    Normal(u8),
    /// Error state (state 2, 0..=100).
    Error(u8),
    /// Indeterminate progress (state 3).
    Indeterminate,
    /// Paused state (state 4, 0..=100).
    Paused(u8),
}

/// Notification urgency level (OSC 99 `u` parameter).
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotificationUrgency {
    /// Low urgency (`u=0`).
    Low,
    /// Normal urgency (`u=1`).
    #[default]
    Normal,
    /// Critical urgency (`u=2`).
    Critical,
}

impl NotificationUrgency {
    /// Parse urgency from the OSC 99 `u` parameter.
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

/// Desktop notification (OSC 99, kitty protocol), aggregated by notification ID.
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
    /// Whether this notification has a title or body.
    #[inline]
    #[must_use]
    pub const fn has_content(&self) -> bool {
        self.title.is_some() || self.body.is_some()
    }
}
