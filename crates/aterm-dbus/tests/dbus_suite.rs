// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-dbus integration files as modules of one test binary, so the crate links one test
//! executable instead of one per file.

mod support;

mod conn_faults;
mod connection_identity;
mod live_bus;
mod mutations;
mod zbus_messages;
mod zbus_session;
mod zvariant_values;
