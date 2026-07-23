// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC protocol types: notifications, remote host, taskbar progress, text sizing,
//! Terminal reporting, semantic blocks and buttons.
//!
//! Extracted from `aterm-core::terminal::types::osc` to break circular
//! dependencies (Part of #5663, #2341).

mod iterm2;
mod multipart;
mod notifications;
mod remote_host;
mod semantic;
mod text_sizing;

pub use iterm2::{Iterm2CellSize, Iterm2SetColor, Iterm2ShellIntegrationVersion};
pub use multipart::{MULTIPART_FILE_MAX_SIZE, MultipartFileOperation, MultipartFileState};
pub use notifications::{Notification, NotificationUrgency, TaskbarProgress};
pub use remote_host::RemoteHost;
pub use semantic::{
    SemanticBlock, SemanticBlockEvent, SemanticButton, SemanticButtonEvent, SemanticButtonType,
};
pub use text_sizing::{TextSizingAlignment, TextSizingOperation};
