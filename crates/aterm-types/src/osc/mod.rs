// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC protocol types: notifications, remote host, taskbar progress and the
//! shell-integration version report.
//!
//! Extracted from `aterm-core::terminal::types::osc` to break circular
//! dependencies (Part of #5663, #2341).

mod iterm2;
mod notifications;
mod remote_host;

pub use iterm2::Iterm2ShellIntegrationVersion;
pub use notifications::{Notification, NotificationUrgency, TaskbarProgress};
pub use remote_host::RemoteHost;
