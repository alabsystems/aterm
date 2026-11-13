// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn long_labels_wrap_above_values_without_overwriting_the_column() {
    for width in [20, 40, 80] {
        for label in [
            "release.openssl",
            "release.source-signing",
            "a long label with spaces",
        ] {
            let rendered = grid_block_at(width, label, "ready for use");
            let lines: Vec<_> = rendered.lines().collect();
            let first_value = lines
                .iter()
                .position(|row| row.starts_with(&" ".repeat(VALUE_COL)))
                .unwrap();
            assert!(first_value > 0);
            assert_eq!(
                lines[..first_value]
                    .iter()
                    .map(|row| row.trim())
                    .collect::<Vec<_>>()
                    .join(" "),
                label
            );
            assert!(
                lines
                    .iter()
                    .all(|row| row.chars().count() <= width || row.split_whitespace().count() == 1)
            );
        }
    }
    assert_eq!(
        grid_block_at(80, "short", "value"),
        format!("  {:<LABEL_MAX$} value", "short")
    );
    assert!(grid_block_at(80, "release.source-signing", " ").is_empty());
}
