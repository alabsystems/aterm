// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The release-tag grammar: ONE classification of what a published `v…` tag
//! means, compiled into both the publisher (`aterm-release`) and the updater
//! client (`aterm-update`).
//!
//! Publisher and fleet disagreeing about which releases are even candidates is
//! a shipping hazard, so the rule lives here and both sides call it instead of
//! restating it. Only the DIAGNOSTIC WORDING is per-caller: this module reports
//! a structured [`TagError`] rather than a sentence, so each side keeps its own
//! error text ("update candidate tag …" vs "published appcast tag …") without
//! owning a second copy of the grammar.

/// Why a tag is not classifiable under the current version protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagError {
    /// Not a `v`-prefixed dotted run of two or three canonically spelled
    /// numeric components: no `v` prefix, non-numeric, empty or leading-zero
    /// components, a bare `v0`, or more than three components.
    Malformed,
    /// Canonically spelled, but a component does not fit in `u64`.
    Overflow,
}

/// What one release tag is to the CURRENT version protocol.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TagKind {
    /// A `vMAJOR.MINOR.PATCH` release on the current version scheme, carrying
    /// its numeric ordering key.
    Candidate(Vec<u64>),
    /// A pre-cut-over `vMAJOR.MINOR` release. The scheme changed to the
    /// unified `MAJOR.MINOR.0` version (`VERSIONING.md`) and the old line was
    /// NOT carried forward — those releases stay published in the archive but
    /// are never candidates, and are skipped rather than treated as errors so
    /// the archive cannot stall the channel.
    Legacy,
}

/// Classify one release tag.
///
/// Only the canonical three-component `vMAJOR.MINOR.PATCH` spelling is a
/// candidate. Exactly two components are the retired scheme
/// ([`TagKind::Legacy`]). Anything else — non-numeric, empty or leading-zero
/// components, a bare `v0`, more than three components — is a hard error:
/// garbage in the tag namespace must fail closed rather than silently narrow
/// the candidate set.
pub fn parse_release_tag(tag: &str) -> Result<TagKind, TagError> {
    let version = tag.strip_prefix('v').ok_or(TagError::Malformed)?;
    let components: Vec<&str> = version.split('.').collect();
    if components.len() < 2 {
        return Err(TagError::Malformed);
    }
    let mut parsed = Vec::with_capacity(components.len());
    for component in components {
        // Reject empty and leading-zero spellings so a tag has exactly ONE
        // canonical form and two tags can never share a numeric order.
        if component.is_empty()
            || !component.bytes().all(|byte| byte.is_ascii_digit())
            || (component.len() > 1 && component.starts_with('0'))
        {
            return Err(TagError::Malformed);
        }
        parsed.push(component.parse::<u64>().map_err(|_| TagError::Overflow)?);
    }
    match parsed.len() {
        2 => Ok(TagKind::Legacy),
        3 => Ok(TagKind::Candidate(parsed)),
        _ => Err(TagError::Malformed),
    }
}

/// The canonical version string a candidate tag names (`"v0.2.0"` → `"0.2.0"`),
/// or `None` when `tag` is not exactly `v{major}.{minor}.{patch}` for its own
/// numeric key.
///
/// `numeric` is the key from [`TagKind::Candidate`], already proved
/// three-component by [`parse_release_tag`]; re-deriving the string here pins
/// the *spelling* too, so `v01.2.3` can never be admitted alongside `v1.2.3`.
#[must_use]
pub fn canonical_version(tag: &str, numeric: &[u64]) -> Option<String> {
    match numeric {
        [major, minor, patch] if tag == format!("v{major}.{minor}.{patch}") => {
            Some(format!("{major}.{minor}.{patch}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_three_component_tags_are_candidates() {
        assert_eq!(
            parse_release_tag("v0.10.0"),
            Ok(TagKind::Candidate(vec![0, 10, 0]))
        );
        // Numeric, never lexicographic: 0.2.9 < 0.2.10.
        assert!(parse_release_tag("v0.2.9").unwrap() < parse_release_tag("v0.2.10").unwrap());
    }

    #[test]
    fn two_component_tags_are_the_retired_scheme() {
        assert_eq!(parse_release_tag("v0.61"), Ok(TagKind::Legacy));
    }

    #[test]
    fn everything_else_fails_closed() {
        for malformed in [
            "0.10.0",
            "V0.10.0",
            "v",
            "v0",
            "v0.x.0",
            "v0.1.2.3",
            "v0..10",
            "v0.10.",
            "v.10.0",
            "v0.10.0-rc1",
            // Leading zeros give one release two spellings.
            "v00.10.0",
            "v0.010.0",
            "v0.10.00",
        ] {
            assert_eq!(
                parse_release_tag(malformed),
                Err(TagError::Malformed),
                "{malformed}"
            );
        }
        assert_eq!(
            parse_release_tag("v1.2.99999999999999999999"),
            Err(TagError::Overflow)
        );
    }

    #[test]
    fn canonical_version_pins_the_spelling_and_the_component_count() {
        assert_eq!(
            canonical_version("v0.2.0", &[0, 2, 0]),
            Some("0.2.0".to_string())
        );
        assert_eq!(canonical_version("v0.02.0", &[0, 2, 0]), None);
        assert_eq!(canonical_version("v0.2", &[0, 2]), None);
    }
}
