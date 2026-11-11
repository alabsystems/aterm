// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/internals/case.rs and modified by the
// aterm project in 2026. See ../LICENSE-MIT and ../LICENSE-APACHE for the
// upstream licenses.

use super::RenameRule::{self, *};

#[test]
fn rename_variants() {
    for &(original, lower, upper, snake, kebab) in &[
        ("Outcome", "outcome", "OUTCOME", "outcome", "outcome"),
        (
            "VeryTasty",
            "verytasty",
            "VERYTASTY",
            "very_tasty",
            "very-tasty",
        ),
        ("A", "a", "A", "a", "a"),
        ("Z42", "z42", "Z42", "z42", "z42"),
    ] {
        assert_eq!(None.apply_to_variant(original), original);
        assert_eq!(LowerCase.apply_to_variant(original), lower);
        assert_eq!(UpperCase.apply_to_variant(original), upper);
        assert_eq!(PascalCase.apply_to_variant(original), original);
        assert_eq!(SnakeCase.apply_to_variant(original), snake);
        assert_eq!(KebabCase.apply_to_variant(original), kebab);
    }
}

#[test]
fn rename_fields() {
    for &(original, upper, pascal, kebab) in &[
        ("outcome", "OUTCOME", "Outcome", "outcome"),
        ("very_tasty", "VERY_TASTY", "VeryTasty", "very-tasty"),
        ("a", "A", "A", "a"),
        ("z42", "Z42", "Z42", "z42"),
    ] {
        assert_eq!(None.apply_to_field(original), original);
        assert_eq!(UpperCase.apply_to_field(original), upper);
        assert_eq!(PascalCase.apply_to_field(original), pascal);
        assert_eq!(SnakeCase.apply_to_field(original), original);
        assert_eq!(LowerCase.apply_to_field(original), original);
        assert_eq!(KebabCase.apply_to_field(original), kebab);
    }
}

#[test]
fn only_the_five_spellings_parse() {
    assert_eq!(RenameRule::from_str("snake_case"), Some(SnakeCase));
    assert_eq!(RenameRule::from_str("kebab-case"), Some(KebabCase));
    assert_eq!(RenameRule::from_str("camelCase"), Option::None);
    assert_eq!(RenameRule::from_str("Snake_Case"), Option::None);
}
