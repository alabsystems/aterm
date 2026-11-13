// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Successful driver wording is data, separate from the command and verdict policy.
//! A wording edit therefore uses ordinary disjoint-file landing composition; no Rust
//! source, field or token is exempted from the exact shared-entry rule.

const DESCRIPTIONS: &str = include_str!("objc-pass.tsv");

pub(super) fn description(example: &str) -> Option<&'static str> {
    lookup(
        DESCRIPTIONS,
        &super::OBJC_DRIVES.map(|drive| drive.example),
        example,
    )
}

/// Exactly one two-column row per registered example, in registry order. Validate the
/// whole table before returning any wording: an unknown, duplicate, missing or malformed
/// row cannot leave an earlier driver's apparently valid description usable.
fn lookup<'a>(table: &'a str, examples: &[&str], example: &str) -> Option<&'a str> {
    let mut rows = table.strip_suffix('\n')?.split('\n');
    let mut found = None;
    for (index, expected) in examples.iter().enumerate() {
        if examples[..index].contains(expected) {
            return None;
        }
        let (name, description) = rows.next()?.split_once('\t')?;
        if name != *expected
            || description.trim().is_empty()
            || description.chars().any(char::is_control)
        {
            return None;
        }
        if name == example {
            found = Some(description);
        }
    }
    rows.next().is_none().then_some(found).flatten()
}

#[cfg(test)]
#[path = "objc_pass_tests.rs"]
mod tests;
