// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::RenderInput;

#[test]
fn cursor_effect_style_override_is_content_cloned_and_cleared_with_overlays() {
    use crate::terminal::CursorStyle;

    let mut source = RenderInput::empty();
    source.cursor_style = CursorStyle::SteadyUnderline;
    source.cursor_effect_style_override = Some(CursorStyle::Bolt);

    let cloned = source.clone();
    assert_eq!(cloned.cursor_effect_style_override, Some(CursorStyle::Bolt));
    assert_eq!(cloned, source);

    let mut reused = RenderInput::empty();
    reused.clone_from(&source);
    assert_eq!(reused.cursor_effect_style_override, Some(CursorStyle::Bolt));
    assert_eq!(reused, source);

    let mut terminal_only = source.clone();
    terminal_only.cursor_effect_style_override = None;
    assert_ne!(
        source, terminal_only,
        "a stationary effect cursor shape changes rendered content"
    );

    source.clear_overlays();
    assert_eq!(source.cursor_effect_style_override, None);
    assert_eq!(
        source.cursor_style,
        CursorStyle::SteadyUnderline,
        "clearing host overlays must restore, not overwrite, DECSCUSR"
    );
    assert_eq!(source, terminal_only);
}
