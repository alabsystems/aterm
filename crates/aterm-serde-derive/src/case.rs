// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_derive 1.0.228 src/internals/case.rs and modified by the
// aterm project in 2026: only the five rules aterm writes
// (docs/measured/third-party-2026-10-15/serde-attrs.tsv); `camelCase`,
// `SCREAMING_SNAKE_CASE` and `SCREAMING-KEBAB-CASE` are refused by name.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

//! Code to convert the Rust-styled field/variant (e.g. `my_field`, `MyType`) to the
//! case of the source (e.g. `my-field`, `MY_FIELD`).

use self::RenameRule::*;

/// The different possible ways to change case of fields in a struct, or variants in an enum.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RenameRule {
    /// Don't apply a default rename rule.
    None,
    /// Rename direct children to "lowercase" style.
    LowerCase,
    /// Rename direct children to "UPPERCASE" style.
    UpperCase,
    /// Rename direct children to "PascalCase" style, as typically used for
    /// enum variants.
    PascalCase,
    /// Rename direct children to "snake_case" style, as commonly used for
    /// fields.
    SnakeCase,
    /// Rename direct children to "kebab-case" style.
    KebabCase,
}

/// The rules aterm's models write, by their spelling in `rename_all = "..."`.
pub static RENAME_RULES: &[(&str, RenameRule)] = &[
    ("lowercase", LowerCase),
    ("UPPERCASE", UpperCase),
    ("PascalCase", PascalCase),
    ("snake_case", SnakeCase),
    ("kebab-case", KebabCase),
];

/// Upstream's rules this derive does not implement.
pub static UPSTREAM_ONLY_RULES: &[&str] =
    &["camelCase", "SCREAMING_SNAKE_CASE", "SCREAMING-KEBAB-CASE"];

impl RenameRule {
    pub fn from_str(rename_all_str: &str) -> Option<Self> {
        RENAME_RULES
            .iter()
            .find(|(name, _)| *name == rename_all_str)
            .map(|(_, rule)| *rule)
    }

    /// Apply a renaming rule to an enum variant, returning the version expected in the source.
    pub fn apply_to_variant(self, variant: &str) -> String {
        match self {
            None | PascalCase => variant.to_owned(),
            LowerCase => variant.to_ascii_lowercase(),
            UpperCase => variant.to_ascii_uppercase(),
            SnakeCase => {
                let mut snake = String::new();
                for (i, ch) in variant.char_indices() {
                    if i > 0 && ch.is_uppercase() {
                        snake.push('_');
                    }
                    snake.push(ch.to_ascii_lowercase());
                }
                snake
            }
            KebabCase => SnakeCase.apply_to_variant(variant).replace('_', "-"),
        }
    }

    /// Apply a renaming rule to a struct field, returning the version expected in the source.
    pub fn apply_to_field(self, field: &str) -> String {
        match self {
            None | LowerCase | SnakeCase => field.to_owned(),
            UpperCase => field.to_ascii_uppercase(),
            PascalCase => {
                let mut pascal = String::new();
                let mut capitalize = true;
                for ch in field.chars() {
                    if ch == '_' {
                        capitalize = true;
                    } else if capitalize {
                        pascal.push(ch.to_ascii_uppercase());
                        capitalize = false;
                    } else {
                        pascal.push(ch);
                    }
                }
                pascal
            }
            KebabCase => field.replace('_', "-"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RenameRule::{self, *};

    // Upstream's rename_variants/rename_fields tables, on the five rules kept.
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
}
