// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared manifest of the 24 terminal callback slots (#6657).
//!
//! Single source of truth for the callback/context field names and their
//! adapter aliases.  Both `aterm-core` and `aterm-core-ffi` expand this
//! manifest to drive registration fan-out and all-null smoke assertions,
//! eliminating the drift-prone hand-maintained slot inventories.

/// Invoke `$mac!(term, cbs, callback_field, context_field, adapter)` once
/// per callback slot.
///
/// Each consuming crate defines local aliases matching the `adapter` names
/// (e.g. `use super::basic::aterm_terminal_set_dcs_callback as register_dcs;`).
#[macro_export]
macro_rules! for_each_terminal_callback_slot {
    ($mac:ident, $term:expr, $cbs:expr) => {
        // Basic
        $mac!($term, $cbs, dcs_callback, dcs_context, register_dcs);
        $mac!($term, $cbs, bell_callback, bell_context, register_bell);
        $mac!(
            $term,
            $cbs,
            buffer_activation_callback,
            buffer_activation_context,
            register_buffer_activation
        );
        $mac!(
            $term,
            $cbs,
            kitty_image_callback,
            kitty_image_context,
            register_kitty_image
        );
        // OSC
        $mac!($term, $cbs, title_callback, title_context, register_title);
        $mac!(
            $term,
            $cbs,
            notification_callback,
            notification_context,
            register_notification
        );
        // OSC Dynamic
        $mac!(
            $term,
            $cbs,
            color_change_callback,
            color_change_context,
            register_color_change
        );
        $mac!(
            $term,
            $cbs,
            advanced_notification_callback,
            advanced_notification_context,
            register_advanced_notification
        );
        // OSC Terminal
        $mac!(
            $term,
            $cbs,
            profile_callback,
            profile_context,
            register_profile
        );
        $mac!(
            $term,
            $cbs,
            badge_format_callback,
            badge_format_context,
            register_badge_format
        );
        $mac!(
            $term,
            $cbs,
            colors_callback,
            colors_context,
            register_colors
        );
        $mac!(
            $term,
            $cbs,
            highlight_cursor_line_callback,
            highlight_cursor_line_context,
            register_highlight_cursor_line
        );
        $mac!(
            $term,
            $cbs,
            report_variable_callback,
            report_variable_context,
            register_report_variable
        );
        $mac!(
            $term,
            $cbs,
            report_cell_size_callback,
            report_cell_size_context,
            register_report_cell_size
        );
        $mac!(
            $term,
            $cbs,
            shell_integration_version_callback,
            shell_integration_version_context,
            register_shell_integration_version
        );
        // Semantic
        $mac!(
            $term,
            $cbs,
            semantic_block_callback,
            semantic_block_context,
            register_semantic_block
        );
        $mac!(
            $term,
            $cbs,
            semantic_button_callback,
            semantic_button_context,
            register_semantic_button
        );
        // Window/Shell
        $mac!(
            $term,
            $cbs,
            window_callback,
            window_context,
            register_window
        );
        $mac!($term, $cbs, shell_callback, shell_context, register_shell);
        // Clipboard
        $mac!(
            $term,
            $cbs,
            clipboard_callback,
            clipboard_context,
            register_clipboard
        );
        $mac!(
            $term,
            $cbs,
            copy_to_clipboard_callback,
            copy_to_clipboard_context,
            register_copy_to_clipboard
        );
        $mac!(
            $term,
            $cbs,
            multipart_file_callback,
            multipart_file_context,
            register_multipart_file
        );
        // Protocol
        $mac!($term, $cbs, tmux_callback, tmux_context, register_tmux);
        $mac!(
            $term,
            $cbs,
            ssh_conductor_callback,
            ssh_conductor_context,
            register_ssh_conductor
        );
    };
}
