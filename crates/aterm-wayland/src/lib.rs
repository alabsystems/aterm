// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party Wayland for aterm.

pub mod cursor;
pub mod globals;
pub mod iface;
pub mod output;
pub mod pointer_frame;
pub mod protocols;
pub mod proxy;
#[cfg(unix)]
pub mod seat;
#[cfg(unix)]
pub mod selection;
pub mod sink;
pub mod sys;
pub mod xdg;
