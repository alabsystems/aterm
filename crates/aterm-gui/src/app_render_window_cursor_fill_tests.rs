// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    INACTIVE_CURSOR_FILL, presented_block_fill, window_cursor_fill, window_cursor_keyline_tint,
};
use aterm_core::terminal::CursorStyle;
use aterm_effects::cursor_glow::{BlockFill, BlockFillOwner};

#[test]
fn status_owner_tracks_the_final_single_pane_fill() {
    assert_eq!(
        window_cursor_fill(Some(CursorStyle::Bolt), Some(0x00AA_44FF)),
        Some(0x00AA_44FF),
        "non-focus shape overrides keep their effect fill"
    );
    let owned = BlockFill {
        owner: BlockFillOwner::Rainbow,
        fill: 0x0050_FA7B,
        base: Some(0x0000_00FF),
    };
    let focused_fill = window_cursor_fill(None, Some(owned.fill));
    assert_eq!(
        presented_block_fill(None, focused_fill, Some(owned)),
        Some(owned)
    );

    let inactive_fill = window_cursor_fill(Some(CursorStyle::HollowBlock), Some(owned.fill));
    assert_eq!(inactive_fill, Some(INACTIVE_CURSOR_FILL));
    assert_eq!(
        presented_block_fill(Some(CursorStyle::HollowBlock), inactive_fill, Some(owned)),
        None,
        "the host's inactive outline, not the effect, owns the final caret"
    );
}

/// A focused window's block wears the rainbow's keyline; an unfocused one's hollow outline is the
/// host's, no effect's body, and drops it, as it drops the effect's fill.
#[test]
fn only_a_focused_block_wears_the_keyline() {
    const TINT: u32 = 0x0013_1B13;
    assert_eq!(window_cursor_keyline_tint(None, Some(TINT)), Some(TINT));
    assert_eq!(
        window_cursor_keyline_tint(Some(CursorStyle::Bolt), Some(TINT)),
        Some(TINT),
        "a non-focus shape override keeps the effect's keyline, as it keeps its fill"
    );
    assert_eq!(
        window_cursor_keyline_tint(Some(CursorStyle::HollowBlock), Some(TINT)),
        None,
        "the host's inactive outline wears no keyline"
    );
    assert_eq!(window_cursor_keyline_tint(None, None), None);
}
