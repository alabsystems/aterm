// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: verify fail-closed behavior when init_mode() is never called.
//!
//! This test runs in its own binary (integration test), so the global OnceLock
//! has NOT been initialized. All gates should default to Containment mode
//! (maximally restrictive).

use aterm_containment::{
    CommandCapability, ContainmentMode, ContainmentPolicy, FsCapability, InputCapability,
    McpCapability, NetworkCapability, OutputCapability, PluginCapability, ProcessCapability,
};

#[test]
fn mode_or_containment_defaults_to_containment_when_uninit() {
    // init_mode() has NOT been called in this binary.
    assert_eq!(aterm_containment::try_current_mode(), None);

    let mode = aterm_containment::mode_or_containment();
    assert_eq!(mode, ContainmentMode::Containment);
}

#[test]
fn uninitialized_mode_gets_most_restrictive_capabilities() {
    let mode = aterm_containment::mode_or_containment();
    let caps = ContainmentPolicy::capabilities(mode);

    assert_eq!(caps.network, NetworkCapability::None);
    assert_eq!(caps.fs, FsCapability::TmpOnly);
    assert_eq!(caps.process, ProcessCapability::NoFork);
    assert_eq!(caps.mcp, McpCapability::Disabled);
    assert_eq!(caps.plugins, PluginCapability::Disabled);
    assert_eq!(caps.output, OutputCapability::Filtered);
    assert_eq!(caps.input, InputCapability::Filtered);
    assert_eq!(caps.command, CommandCapability::NoCommands);
}
