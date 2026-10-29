// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Release-tag grammar shared by `aterm-release` and `aterm-update`.

/// Why a tag is not classifiable under the current version protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagError {
    /// Not a canonical `v`-prefixed dotted run of two or three numeric components.
    Malformed,
    /// Canonically spelled, but a component does not fit in `u64`.
    Overflow,
}

/// What one release tag is to the current version protocol.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TagKind {
    /// A `vMAJOR.MINOR.PATCH` release, carrying its numeric ordering key.
    Candidate(Vec<u64>),
    /// A retired two-component `vMAJOR.MINOR` release, skipped rather than treated as an error.
    Legacy,
}

/// Classify a release tag as [`TagKind::Candidate`], [`TagKind::Legacy`], or [`TagError`].
pub fn parse_release_tag(tag: &str) -> Result<TagKind, TagError> {
    let version = tag.strip_prefix('v').ok_or(TagError::Malformed)?;
    let components: Vec<&str> = version.split('.').collect();
    if !(2..=3).contains(&components.len()) {
        return Err(TagError::Malformed);
    }
    let mut parsed = Vec::with_capacity(components.len());
    for component in components {
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

/// Return `"MAJOR.MINOR.PATCH"` when `tag` is canonically `"v{major}.{minor}.{patch}"`.
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
            ("v00.10.0", Err(Malformed)),
            ("v0.010.0", Err(Malformed)),
            ("v0.10.00", Err(Malformed)),
            ("v1.2.3.99999999999999999999", Err(Malformed)),
            ("v1.2.99999999999999999999", Err(Overflow)),
        ];
        for (tag, want) in rows {
            assert_eq!(&parse_release_tag(tag), want, "{tag}");
        }
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
