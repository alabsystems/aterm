// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Property-based tests for Terminal inline image parsing and dimensions.

use crate::iterm_image::{DimensionSpec, ITERM_MAX_DIMENSION, InlineImageParams};
use proptest::prelude::*;

fn dimension_spec_within_bounds(spec: DimensionSpec) -> bool {
    match spec {
        DimensionSpec::Cells(n) => n <= 10_000,
        DimensionSpec::Pixels(n) => n <= ITERM_MAX_DIMENSION,
        DimensionSpec::Percent(n) => n <= 100,
        DimensionSpec::Auto => true,
    }
}

fn parsed_dimension_spec() -> impl Strategy<Value = DimensionSpec> {
    prop_oneof![
        (0u32..=10_000u32).prop_map(DimensionSpec::Cells),
        (0u32..=ITERM_MAX_DIMENSION).prop_map(DimensionSpec::Pixels),
        (0u8..=100u8).prop_map(DimensionSpec::Percent),
        Just(DimensionSpec::Auto),
    ]
}

proptest! {
    /// Numeric dimensions parse as cells and clamp to 10,000.
    #[test]
    fn proptest_dimension_spec_parse_cells_clamps(input in any::<u32>()) {
        let parsed = DimensionSpec::parse(&input.to_string());
        prop_assert_eq!(parsed, Some(DimensionSpec::Cells(input.min(10_000))));
    }

    /// Pixel dimensions parse as pixels and clamp to ITERM_MAX_DIMENSION.
    #[test]
    fn proptest_dimension_spec_parse_pixels_clamps(input in any::<u32>()) {
        let parsed = DimensionSpec::parse(&format!("{input}px"));
        prop_assert_eq!(
            parsed,
            Some(DimensionSpec::Pixels(input.min(ITERM_MAX_DIMENSION))),
        );
    }

    /// Percent dimensions parse as percent and clamp to 100.
    #[test]
    fn proptest_dimension_spec_parse_percent_clamps(input in any::<u8>()) {
        let parsed = DimensionSpec::parse(&format!("{input}%"));
        prop_assert_eq!(parsed, Some(DimensionSpec::Percent(input.min(100))));
    }

    /// Resolve matches the per-variant reference model and keeps percent-based
    /// results bounded by terminal size.
    #[test]
    fn proptest_dimension_spec_resolve_matches_model(
        spec in parsed_dimension_spec(),
        inherent in any::<u32>(),
        cell_size in any::<u32>(),
        terminal_size in any::<u32>(),
    ) {
        let resolved = spec.resolve(inherent, cell_size, terminal_size);

        match spec {
            DimensionSpec::Auto => prop_assert_eq!(resolved, inherent),
            DimensionSpec::Cells(n) => prop_assert_eq!(resolved, n.saturating_mul(cell_size)),
            DimensionSpec::Pixels(n) => prop_assert_eq!(resolved, n),
            DimensionSpec::Percent(pct) => {
                let expected = u64::from(terminal_size) * u64::from(pct) / 100;
                prop_assert_eq!(resolved, expected as u32);
                prop_assert!(resolved <= terminal_size);
            }
        }
    }

    /// InlineImageParams::parse handles arbitrary key=value pairs without panic
    /// and preserves bounded dimension invariants.
    #[test]
    fn proptest_inline_image_params_parse_never_panics_on_key_value_input(
        pairs in prop::collection::vec(
            (
                "[A-Za-z][A-Za-z0-9_]{0,11}",
                "[\\x20-\\x3A\\x3C-\\x7E]{0,32}"
            ),
            0..32
        )
    ) {
        let params_str = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(";");

        let parsed = InlineImageParams::parse(&params_str);
        prop_assert!(dimension_spec_within_bounds(parsed.width));
        prop_assert!(dimension_spec_within_bounds(parsed.height));
    }
}
