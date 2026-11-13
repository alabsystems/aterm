// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn recorded_issues_never_change_tab_ink() {
    let theme = Theme::default();
    let colors = strip_colors_with_active(theme, None);
    let ink = [1, 2, 3];
    for attention in [
        ChipLevel::Off,
        ChipLevel::Note,
        ChipLevel::Done,
        ChipLevel::Wait,
        ChipLevel::Stop(crate::presence::StopCause::Wall),
    ] {
        let metadata = TabStripMetadata {
            attention,
            ..PLAIN_TAB
        };
        assert_eq!(status_mark_ink(metadata, &colors, theme.into(), ink), ink);
        assert_eq!(
            status_mark_ink(
                TabStripMetadata {
                    dirty: true,
                    ..metadata
                },
                &colors,
                theme.into(),
                ink
            ),
            colors.accent
        );
    }
}
