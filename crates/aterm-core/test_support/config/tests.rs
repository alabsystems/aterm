// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for configuration types.

use super::*;
use crate::platform::FontDescriptor;
use aterm_types::{CursorStyle, Rgb};

#[test]
fn test_default_config() {
    let config = TerminalConfig::default();
    assert_eq!(config.cursor_style, CursorStyle::BlinkingBlock);
    assert!(config.cursor_blink);
    assert!(config.cursor_visible);
    assert_eq!(config.font, FontDescriptor::default());
    // #7929: default scrollback line limit raised to 100_000 to cap
    // runaway stdout without requiring every integration to set a limit.
    assert_eq!(
        config.scrollback_limit,
        Some(aterm_scrollback::DEFAULT_LINE_LIMIT)
    );
    assert!(config.auto_wrap);
}

#[test]
fn test_builder() {
    let config = TerminalConfig::builder()
        .cursor_style(CursorStyle::SteadyUnderline)
        .cursor_blink(false)
        .scrollback_limit(50_000)
        .build();

    assert_eq!(config.cursor_style, CursorStyle::SteadyUnderline);
    assert!(!config.cursor_blink);
    assert_eq!(config.scrollback_limit, Some(50_000));
}

#[test]
fn test_diff_no_changes() {
    let config1 = TerminalConfig::default();
    let config2 = TerminalConfig::default();
    let changes = config1.diff(&config2);
    assert!(changes.is_empty());
}

#[test]
fn test_diff_with_changes() {
    let config1 = TerminalConfig::default();
    let config2 = TerminalConfig::builder()
        .cursor_style(CursorStyle::SteadyBar)
        .cursor_blink(false)
        .build();

    let changes = config1.diff(&config2);
    assert!(changes.contains(&ConfigChange::CursorStyle));
    assert!(changes.contains(&ConfigChange::CursorBlink));
    assert_eq!(changes.len(), 2);
}

#[test]
fn test_diff_single_field_changes() {
    let config1 = TerminalConfig::default();
    let mut auto_wrap_disabled = TerminalConfig::default();
    auto_wrap_disabled.auto_wrap = false;
    let cases = [
        (
            TerminalConfig::builder()
                .cursor_style(CursorStyle::SteadyBar)
                .build(),
            ConfigChange::CursorStyle,
        ),
        (
            TerminalConfig::builder().cursor_blink(false).build(),
            ConfigChange::CursorBlink,
        ),
        (
            TerminalConfig::builder().scrollback_limit(50_000).build(),
            ConfigChange::ScrollbackLimit,
        ),
        (auto_wrap_disabled, ConfigChange::AutoWrap),
    ];

    for (config2, expected_change) in cases {
        let changes = config1.diff(&config2);
        assert_eq!(changes, vec![expected_change]);
    }
}

#[test]
fn test_diff_colors() {
    let config1 = TerminalConfig::default();
    let config2 = TerminalConfig::builder()
        .default_foreground(Rgb::new(200, 200, 200))
        .build();

    let changes = config1.diff(&config2);
    assert!(changes.contains(&ConfigChange::Colors));
    assert_eq!(changes.len(), 1);
}

#[test]
fn test_diff_font() {
    let config1 = TerminalConfig::default();
    let config2 = TerminalConfig::builder().font_family("Menlo").build();

    let changes = config1.diff(&config2);
    assert!(changes.contains(&ConfigChange::Font));
    assert_eq!(changes.len(), 1);
}

#[test]
fn test_config_equality() {
    let config1 = TerminalConfig::default();
    let config2 = TerminalConfig::default();
    assert_eq!(config1, config2);

    let config3 = TerminalConfig::builder().cursor_blink(false).build();
    assert_ne!(config1, config3);
}

#[test]
fn test_bidi_mode_default() {
    let mode = BiDiMode::default();
    assert_eq!(mode, BiDiMode::Implicit);
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

#[test]
fn test_bidi_builder_methods() {
    use aterm_types::ParagraphDirection;

    let config = TerminalConfig::builder()
        .bidi_mode(BiDiMode::Explicit)
        .bidi_direction(ParagraphDirection::Rtl)
        .bidi_reorder_nsm(false)
        .build();

    assert_eq!(config.bidi.mode, BiDiMode::Explicit);
    assert_eq!(config.bidi.direction, ParagraphDirection::Rtl);
    assert!(!config.bidi.reorder_nsm);
}

#[test]
fn test_diff_bidi_change() {
    let config1 = TerminalConfig::default();
    let config2 = TerminalConfig::builder()
        .bidi_mode(BiDiMode::Disabled)
        .build();

    let changes = config1.diff(&config2);
    assert!(changes.contains(&ConfigChange::BiDi));
    assert_eq!(changes.len(), 1);
}

#[test]
fn test_terminal_config_bidi_default() {
    let config = TerminalConfig::default();
    assert_eq!(config.bidi.mode, BiDiMode::Implicit);
    assert!(config.bidi.is_enabled());
}

#[test]
fn test_diff_scrollback_backend_not_diffed() {
    // scrollback_backend is construction-time only — diff() should not report it.
    let config1 = TerminalConfig::default();
    let config2 = TerminalConfig::builder()
        .scrollback_backend(ScrollbackBackend::Disk(DiskBackendConfig::new(
            "/tmp/test.dtrm",
        )))
        .build();

    let changes = config1.diff(&config2);
    assert!(changes.is_empty());
}

#[test]
fn test_scrollback_backend_builder_method() {
    let disk_config = DiskBackendConfig::new("/tmp/scrollback.dtrm")
        .with_hot_limit(500)
        .with_warm_limit(5000);

    let config = TerminalConfig::builder()
        .scrollback_backend(ScrollbackBackend::Disk(disk_config.clone()))
        .build();

    match &config.scrollback_backend {
        ScrollbackBackend::Disk(cfg) => {
            assert_eq!(cfg.path, std::path::PathBuf::from("/tmp/scrollback.dtrm"));
            assert_eq!(cfg.hot_limit, 500);
            assert_eq!(cfg.warm_limit, 5000);
        }
        ScrollbackBackend::Memory => panic!("Expected Disk backend"),
    }
}

#[test]
fn test_scrollback_backend_default_is_memory() {
    let config = TerminalConfig::default();
    assert_eq!(config.scrollback_backend, ScrollbackBackend::Memory);
}
