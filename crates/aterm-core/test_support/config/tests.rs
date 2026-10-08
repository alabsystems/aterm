// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for configuration types.

use super::*;
use aterm_types::CursorStyle;

#[test]
fn test_default_config() {
    let config = TerminalConfig::default();
    assert_eq!(config.cursor_style, CursorStyle::BlinkingBlock);
    assert!(config.cursor_blink);
    assert!(config.cursor_visible);
    // The default line limit caps runaway stdout without every integration
    // setting one.
    assert_eq!(
        config.scrollback_limit,
        Some(aterm_scrollback::DEFAULT_LINE_LIMIT)
    );
    assert!(config.auto_wrap);
}

#[test]
fn test_config_equality() {
    let config1 = TerminalConfig::default();
    let config2 = TerminalConfig::default();
    assert_eq!(config1, config2);

    let config3 = TerminalConfig {
        cursor_blink: false,
        ..TerminalConfig::default()
    };
    assert_ne!(config1, config3);
}

#[test]
fn test_bidi_config_default() {
    let config = BiDiConfig::default();
    assert_eq!(config.mode, BiDiMode::Implicit);
    assert!(config.reorder_nsm);
    assert!(config.is_enabled());
}

#[test]
fn test_bidi_config_disabled() {
    let config = BiDiConfig::disabled();
    assert_eq!(config.mode, BiDiMode::Disabled);
    assert!(!config.is_enabled());
}
