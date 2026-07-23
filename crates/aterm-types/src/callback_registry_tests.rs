// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn callback_count_returns_registry_length() {
    assert_eq!(callback_count(), CALLBACK_REGISTRY.len());
    assert!(callback_count() > 0, "registry should not be empty");
}

#[test]
fn callback_info_returns_valid_entries() {
    // First entry should exist and be "bell"
    let first = callback_info(0).expect("first callback should exist");
    assert_eq!(first.name, "bell");
    assert_eq!(first.setter, "set_bell_callback");

    // Last entry should exist and be "kvp"
    let last_idx = callback_count() - 1;
    let last = callback_info(last_idx).expect("last callback should exist");
    assert_eq!(last.name, "kvp");
    assert_eq!(last.setter, "set_kvp_callback");

    // Out of bounds should return None
    assert!(callback_info(callback_count()).is_none());
    assert!(callback_info(usize::MAX).is_none());
}

#[test]
fn callback_by_name_finds_callbacks() {
    // Find existing callbacks
    let bell = callback_by_name("bell").expect("bell callback should exist");
    assert_eq!(bell.setter, "set_bell_callback");

    let clipboard = callback_by_name("clipboard").expect("clipboard callback should exist");
    assert_eq!(clipboard.category, CallbackCategory::Clipboard);

    let shell = callback_by_name("shell").expect("shell callback should exist");
    assert_eq!(shell.category, CallbackCategory::Shell);

    // Non-existent callback
    assert!(callback_by_name("nonexistent").is_none());
    assert!(callback_by_name("").is_none());
}

#[test]
fn all_callbacks_have_required_fields() {
    for (i, info) in CALLBACK_REGISTRY.iter().enumerate() {
        assert!(!info.name.is_empty(), "callback {i} has empty name");
        assert!(!info.setter.is_empty(), "callback {i} has empty setter");
        assert!(!info.event.is_empty(), "callback {i} has empty event");
        assert!(
            !info.signature.is_empty(),
            "callback {i} has empty signature"
        );
        assert!(
            info.setter.starts_with("set_"),
            "callback {} setter '{}' should start with 'set_'",
            i,
            info.setter
        );
        assert!(
            info.setter.ends_with("_callback"),
            "callback {} setter '{}' should end with '_callback'",
            i,
            info.setter
        );
    }
}

#[test]
fn callback_categories_are_valid() {
    // Check that at least one callback exists in each category
    let categories = [
        CallbackCategory::Ui,
        CallbackCategory::Clipboard,
        CallbackCategory::Shell,
        CallbackCategory::Graphics,
        CallbackCategory::Protocol,
    ];

    for category in categories {
        let count = CALLBACK_REGISTRY
            .iter()
            .filter(|info| info.category == category)
            .count();
        assert!(
            count > 0,
            "category {category:?} should have at least one callback"
        );
    }
}

#[test]
fn all_callbacks_are_thread_safe() {
    // All terminal callbacks require Send bound
    for info in CALLBACK_REGISTRY {
        assert!(
            info.thread_safe,
            "callback '{}' should be thread_safe",
            info.name
        );
    }
}

#[test]
fn callback_names_are_unique() {
    let mut names: Vec<&str> = CALLBACK_REGISTRY.iter().map(|info| info.name).collect();
    let original_len = names.len();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), original_len, "callback names should be unique");
}

/// Regression test for #4718: color_change was missing from the registry.
#[test]
fn color_change_is_discoverable() {
    let info = callback_by_name("color_change").expect("color_change should exist in registry");
    assert_eq!(info.setter, "set_color_change_callback");
    assert_eq!(info.category, CallbackCategory::Ui);
    assert_eq!(info.signature, "FnMut(u8, Rgb, ColorChangeOp)");
}

/// Part of #6312: title_event v3 callback is discoverable.
#[test]
fn title_event_is_discoverable() {
    let info = callback_by_name("title_event").expect("title_event should exist in registry");
    assert_eq!(info.setter, "set_title_event_callback");
    assert_eq!(info.category, CallbackCategory::Ui);
    assert_eq!(info.signature, "FnMut(TitleType, &str)");
}

/// Part of #6312: kvp pass-through callback is discoverable.
#[test]
fn kvp_is_discoverable() {
    let info = callback_by_name("kvp").expect("kvp should exist in registry");
    assert_eq!(info.setter, "set_kvp_callback");
    assert_eq!(info.category, CallbackCategory::Protocol);
    assert_eq!(info.signature, "FnMut(&str, Option<&str>) -> bool");
}

/// Verify that all FFI struct callback names have corresponding registry entries.
///
/// The FFI struct (AtermTerminalCallbacks) exposes 24 callbacks. Each must
/// be discoverable through the registry API so FFI consumers can enumerate
/// available callbacks (#4718).
#[test]
fn ffi_struct_callbacks_all_in_registry() {
    // These are the 24 callback names matching AtermTerminalCallbacks fields.
    let ffi_callbacks = [
        "dcs",
        "bell",
        "buffer_activation",
        "kitty_image",
        "title",
        "notification",
        "color_change",
        "advanced_notification",
        "profile",
        "badge_format",
        "set_colors",
        "highlight_cursor_line",
        "report_variable",
        "report_cell_size",
        "shell_integration_version",
        "semantic_block",
        "semantic_button",
        "window",
        "shell",
        "clipboard",
        "copy_to_clipboard",
        "multipart_file",
        "tmux",
        "ssh_conductor",
    ];

    for name in &ffi_callbacks {
        assert!(
            callback_by_name(name).is_some(),
            "FFI struct callback '{name}' missing from CALLBACK_REGISTRY"
        );
    }
}
