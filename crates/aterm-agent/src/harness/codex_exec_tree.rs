// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exact descendant identities bound by the source image before replacement.
//! This table never substitutes for the native host's live policy checks.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    thread: String,
    parent: Option<String>,
    turn: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    version: u32,
    digest: String,
    root: String,
    turn: String,
    members: Vec<Member>,
}

fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

impl Manifest {
    #[must_use]
    pub fn decode(json: &str, root: &str, turn: &str) -> Option<Self> {
        let value: Self = aterm_json::from_str(json).ok()?;
        if value.version != 1
            || value.root != root
            || value.turn != turn
            || !identity(root)
            || !identity(turn)
            || value.digest.len() != 64
            || !value
                .digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        let mut seen = HashSet::new();
        for member in &value.members {
            if !identity(&member.thread)
                || member.turn.as_ref().is_some_and(|turn| !identity(turn))
                || seen.contains(&member.thread)
            {
                return None;
            }
            if seen.is_empty() {
                if member.thread != root
                    || member.parent.is_some()
                    || member.turn.as_deref() != Some(turn)
                {
                    return None;
                }
            } else if !member
                .parent
                .as_ref()
                .is_some_and(|parent| seen.contains(parent))
            {
                return None;
            }
            seen.insert(member.thread.clone());
        }
        (!seen.is_empty()).then_some(value)
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

#[derive(Debug)]
pub struct Admission {
    manifest: Manifest,
    admitted: HashSet<String>,
}

impl Admission {
    #[must_use]
    pub fn new(manifest: Manifest) -> Self {
        Self {
            manifest,
            admitted: HashSet::new(),
        }
    }

    #[must_use]
    pub fn matches(&self, manifest: &Manifest) -> bool {
        self.manifest == *manifest
    }

    /// The caller rechecks the current image, account, owner and policy on
    /// every call, including retries. `Some(false)` is an exact retry.
    pub fn admit(&mut self, digest: &str, thread: &str, turn: Option<&str>) -> Option<bool> {
        if digest != self.manifest.digest || thread == self.manifest.root {
            return None;
        }
        self.manifest
            .members
            .iter()
            .find(|member| member.thread == thread && member.turn.as_deref() == turn)?;
        Some(self.admitted.insert(thread.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            version: 1,
            digest: "a".repeat(64),
            root: "root".into(),
            turn: "original".into(),
            members: vec![
                Member {
                    thread: "root".into(),
                    parent: None,
                    turn: Some("original".into()),
                },
                Member {
                    thread: "active".into(),
                    parent: Some("root".into()),
                    turn: Some("child-turn".into()),
                },
                Member {
                    thread: "idle".into(),
                    parent: Some("active".into()),
                    turn: None,
                },
            ],
        }
    }

    fn valid(manifest: &Manifest) -> bool {
        Manifest::decode(
            &aterm_json::to_string(manifest).unwrap(),
            "root",
            "original",
        )
        .is_some()
    }

    #[test]
    fn source_tree_requires_exact_origin_and_ordered_unique_members() {
        assert!(valid(&manifest()));
        let mut changed = manifest();
        changed.members.swap(1, 2);
        assert!(!valid(&changed));
        let mut changed = manifest();
        changed.members.push(changed.members[1].clone());
        assert!(!valid(&changed));
        let mut changed = manifest();
        changed.members[1].parent = Some("elsewhere".into());
        assert!(!valid(&changed));
        let mut changed = manifest();
        changed.members[0].turn = None;
        assert!(!valid(&changed));
        let mut changed = manifest();
        changed.digest = "A".repeat(64);
        assert!(!valid(&changed));
        let mut changed = manifest();
        changed.version = 2;
        assert!(!valid(&changed));
    }

    #[test]
    fn member_admission_is_capsule_bound_and_never_invents_an_idle_turn() {
        let original = manifest();
        let mut ledger = Admission::new(original.clone());
        assert!(ledger.matches(&original));
        assert_eq!(
            ledger.admit(&"b".repeat(64), "active", Some("child-turn")),
            None
        );
        assert_eq!(
            ledger.admit(&original.digest, "active", Some("other")),
            None
        );
        assert_eq!(
            ledger.admit(&original.digest, "missing", Some("child-turn")),
            None
        );
        assert_eq!(
            ledger.admit(&original.digest, "root", Some("original")),
            None
        );
        assert_eq!(
            ledger.admit(&original.digest, "active", Some("child-turn")),
            Some(true)
        );
        assert_eq!(
            ledger.admit(&original.digest, "active", Some("child-turn")),
            Some(false)
        );
        assert_eq!(
            ledger.admit(&original.digest, "idle", Some("invented")),
            None
        );
        assert_eq!(ledger.admit(&original.digest, "idle", None), Some(true));
        assert_eq!(ledger.admit(&original.digest, "idle", None), Some(false));
        let mut changed = original;
        changed.members[1].turn = Some("replacement".into());
        assert!(!ledger.matches(&changed));
    }
}
