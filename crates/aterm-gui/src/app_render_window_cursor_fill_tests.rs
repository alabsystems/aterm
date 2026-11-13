// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{INACTIVE_CURSOR_FILL, presented_block_fill, window_cursor_fill};
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
