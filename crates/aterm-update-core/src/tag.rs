// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The release-tag grammar, compiled into both the publisher (`aterm-release`) and the
//! updater (`aterm-update`) so they cannot disagree about which releases are candidates. Each
//! side words its own diagnostics from the structured [`TagError`].

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

/// What one release tag is to the current version protocol.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TagKind {
    /// A `vMAJOR.MINOR.PATCH` release, carrying its numeric ordering key.
    Candidate(Vec<u64>),
    /// A `vMAJOR.MINOR` release from the retired scheme (`VERSIONING.md`): still published but
    /// never a candidate, and skipped rather than an error so the archive cannot stall the
    /// channel.
    Legacy,
}

/// Classify one release tag.
///
/// Only canonical `vMAJOR.MINOR.PATCH` is a candidate; exactly two components is the retired
/// scheme ([`TagKind::Legacy`]); anything else is an error, so garbage in the tag namespace
/// fails closed rather than silently narrowing the candidate set.
pub fn parse_release_tag(tag: &str) -> Result<TagKind, TagError> {
    let version = tag.strip_prefix('v').ok_or(TagError::Malformed)?;
    let components: Vec<&str> = version.split('.').collect();
    if components.len() < 2 {
        return Err(TagError::Malformed);
    }
    let mut parsed = Vec::with_capacity(components.len());
    for component in components {
        // Reject empty and leading-zero spellings so a tag has exactly one
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

/// The canonical version string a candidate tag names (`"v0.2.0"` → `"0.2.0"`), or `None`
/// when `tag` is not exactly `v{major}.{minor}.{patch}` for its own numeric key (from
/// [`TagKind::Candidate`]). Re-deriving the string pins the spelling, so `v01.2.3` can never
/// be admitted alongside `v1.2.3`.
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

    /// A canonical three-component tag is a candidate, exactly two components is the retired
    /// scheme, and everything else fails closed.
    #[test]
    fn parse_release_tag_classifies_every_spelling() {
        use TagError::{Malformed, Overflow};
        let rows: &[(&str, Result<TagKind, TagError>)] = &[
            ("v0.10.0", Ok(TagKind::Candidate(vec![0, 10, 0]))),
            ("v0.61", Ok(TagKind::Legacy)),
            ("0.10.0", Err(Malformed)),
            ("V0.10.0", Err(Malformed)),
            ("v", Err(Malformed)),
            ("v0", Err(Malformed)),
            ("v0.x.0", Err(Malformed)),
            ("v0.1.2.3", Err(Malformed)),
            ("v0..10", Err(Malformed)),
            ("v0.10.", Err(Malformed)),
            ("v.10.0", Err(Malformed)),
            ("v0.10.0-rc1", Err(Malformed)),
            // Leading zeros give one release two spellings.
            ("v00.10.0", Err(Malformed)),
            ("v0.010.0", Err(Malformed)),
            ("v0.10.00", Err(Malformed)),
            ("v1.2.99999999999999999999", Err(Overflow)),
        ];
        for (tag, want) in rows {
            assert_eq!(&parse_release_tag(tag), want, "{tag}");
        }
        // Numeric, never lexicographic: 0.2.9 < 0.2.10.
        assert!(parse_release_tag("v0.2.9").unwrap() < parse_release_tag("v0.2.10").unwrap());
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
