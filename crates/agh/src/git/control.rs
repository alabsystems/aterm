// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Exact release control-ref transactions. Ordinary branches and immutable
//! product tags remain outside this operation.

use crate::error::{Error, Result};

/// The existing release-claim lease, whose value is its owning commit.
pub const RELEASE_LEASE_REF: &str = "refs/tags/aterm-release-lease";
/// The existing unique publisher token, an annotated tag of that same commit.
pub const PUBLISHER_FENCE_REF: &str = "refs/tags/aterm-release-fence";

/// One exact state transition. `None` means absent, never an omitted guard.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRefChange {
    /// Exactly one of the two canonical control refs.
    #[serde(rename = "ref")]
    pub reference: String,
    /// The exact expected old object, or absence for a create.
    pub before: Option<String>,
    /// The exact new object, or absence for a delete.
    pub after: Option<String>,
}

/// An admitted canonical control transaction. Object kinds, annotated-token
/// ownership and authenticated current state are additionally checked by the
/// native source/wire admission before any ref changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlRefTransaction {
    owner_commit: String,
    changes: Vec<ControlRefChange>,
}

impl ControlRefTransaction {
    /// Admit an existing release protocol transition without authorizing a
    /// general forced update. The only grouped transition is the paired unlock.
    ///
    /// # Errors
    /// Invalid/zero object IDs, unknown/duplicate refs, no-op rows, lease-owner
    /// rotation, or a grouped transition other than the two exact deletes.
    pub fn new(owner_commit: &str, mut changes: Vec<ControlRefChange>) -> Result<Self> {
        let oid =
            |value: &str| crate::path::is_sha(value) && !value.bytes().all(|byte| byte == b'0');
        if !oid(owner_commit) {
            return Err(Error::usage(
                "a control transaction needs one nonzero owner commit ID",
            ));
        }
        if changes.is_empty() || changes.len() > 2 {
            return Err(Error::usage(
                "a control transaction needs one or two exact transitions",
            ));
        }
        changes.sort_unstable_by(|left, right| left.reference.cmp(&right.reference));
        let mut previous = None;
        for change in &changes {
            if !matches!(
                change.reference.as_str(),
                RELEASE_LEASE_REF | PUBLISHER_FENCE_REF
            ) || previous == Some(change.reference.as_str())
            {
                return Err(Error::usage(
                    "a control transaction admits each canonical lease/fence ref once",
                ));
            }
            previous = Some(change.reference.as_str());
            if change.before == change.after {
                return Err(Error::usage(
                    "a control transaction cannot omit a guard as an up-to-date row",
                ));
            }
            if change.before.as_deref().is_some_and(|value| !oid(value))
                || change.after.as_deref().is_some_and(|value| !oid(value))
            {
                return Err(Error::usage(
                    "control transition object IDs must be nonzero raw IDs",
                ));
            }
            if change.reference == RELEASE_LEASE_REF
                && change
                    .before
                    .iter()
                    .chain(&change.after)
                    .any(|value| value != owner_commit)
            {
                return Err(Error::usage(
                    "a release lease can only create or delete its exact owner",
                ));
            }
            if change.reference == PUBLISHER_FENCE_REF
                && change
                    .before
                    .iter()
                    .chain(&change.after)
                    .any(|value| value == owner_commit)
            {
                return Err(Error::usage(
                    "a publisher fence must be a distinct annotated token",
                ));
            }
        }
        if changes.len() == 2
            && changes
                .iter()
                .any(|change| change.before.is_none() || change.after.is_some())
        {
            return Err(Error::usage(
                "the grouped control transition is the paired exact unlock",
            ));
        }
        Ok(Self {
            owner_commit: owner_commit.into(),
            changes,
        })
    }

    /// The one claim commit every lease and annotated token belongs to.
    #[must_use]
    pub fn owner_commit(&self) -> &str {
        &self.owner_commit
    }

    /// The admitted transitions in canonical ref-name order.
    #[must_use]
    pub fn changes(&self) -> &[ControlRefChange] {
        &self.changes
    }

    #[cfg(any(unix, test))]
    pub(crate) fn check_before(&self, lease: Option<&str>, fence: Option<&str>) -> Result<()> {
        if self.changes.len() == 1 {
            let change = &self.changes[0];
            if change.reference == PUBLISHER_FENCE_REF && change.after.is_none() {
                return Ok(());
            }
            if change.reference == RELEASE_LEASE_REF && fence.is_some() {
                return Err(Error::new(
                    crate::Kind::Conflict,
                    "the publisher fence must be absent for a single lease transition",
                ));
            }
            if change.reference == PUBLISHER_FENCE_REF && lease != Some(self.owner_commit()) {
                return Err(Error::new(
                    crate::Kind::Conflict,
                    "the publisher token's claim no longer owns the lease",
                ));
            }
        }
        Ok(())
    }

    #[cfg(any(unix, test))]
    pub(crate) fn check_fence_header(&self, body: &[u8]) -> Result<()> {
        let mut object = None;
        let mut kind = None;
        for line in body
            .split(|byte| *byte == b'\n')
            .take_while(|line| !line.is_empty())
        {
            if let Some(value) = line.strip_prefix(b"object ")
                && object.replace(value).is_some()
            {
                return Err(Error::refused(
                    "a control token has duplicate object headers",
                ));
            }
            if let Some(value) = line.strip_prefix(b"type ")
                && kind.replace(value).is_some()
            {
                return Err(Error::refused("a control token has duplicate type headers"));
            }
        }
        if object != Some(self.owner_commit.as_bytes()) || kind != Some(b"commit".as_slice()) {
            return Err(Error::refused(
                "a publisher token must directly annotate its exact owner commit",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_control_transitions_cannot_become_a_general_force_operation() {
        let owner = "1".repeat(40);
        let token = "2".repeat(40);
        let next = "3".repeat(40);
        let row = |reference: &str, before: Option<&str>, after: Option<&str>| ControlRefChange {
            reference: reference.into(),
            before: before.map(str::to_owned),
            after: after.map(str::to_owned),
        };
        for changes in [
            vec![row(RELEASE_LEASE_REF, None, Some(&owner))],
            vec![row(RELEASE_LEASE_REF, Some(&owner), None)],
            vec![row(PUBLISHER_FENCE_REF, None, Some(&token))],
            vec![row(PUBLISHER_FENCE_REF, Some(&token), Some(&next))],
            vec![row(PUBLISHER_FENCE_REF, Some(&token), None)],
            vec![
                row(RELEASE_LEASE_REF, Some(&owner), None),
                row(PUBLISHER_FENCE_REF, Some(&token), None),
            ],
        ] {
            assert!(ControlRefTransaction::new(&owner, changes).is_ok());
        }
        for changes in [
            Vec::new(),
            vec![row("refs/heads/main", Some(&owner), Some(&next))],
            vec![row("refs/tags/v1", Some(&token), None)],
            vec![row(RELEASE_LEASE_REF, Some(&owner), Some(&next))],
            vec![row(RELEASE_LEASE_REF, Some(&owner), Some(&owner))],
            vec![row(RELEASE_LEASE_REF, None, None)],
            vec![row(PUBLISHER_FENCE_REF, Some(&owner), Some(&next))],
            vec![
                row(PUBLISHER_FENCE_REF, None, Some(&token)),
                row(PUBLISHER_FENCE_REF, Some(&token), None),
            ],
            vec![
                row(RELEASE_LEASE_REF, None, Some(&owner)),
                row(PUBLISHER_FENCE_REF, None, Some(&token)),
            ],
        ] {
            assert!(ControlRefTransaction::new(&owner, changes).is_err());
        }
        assert!(
            ControlRefTransaction::new(
                &"0".repeat(40),
                vec![row(RELEASE_LEASE_REF, None, Some(&owner))]
            )
            .is_err()
        );
    }

    #[test]
    fn observed_state_and_annotation_bind_the_exact_control_owner() {
        let owner = "1".repeat(40);
        let token = "2".repeat(40);
        let other = "3".repeat(40);
        let make = |after| {
            ControlRefTransaction::new(
                &owner,
                vec![ControlRefChange {
                    reference: PUBLISHER_FENCE_REF.into(),
                    before: Some(token.clone()),
                    after,
                }],
            )
            .unwrap()
        };
        let replace = make(Some(other.clone()));
        assert!(replace.check_before(Some(&owner), Some(&token)).is_ok());
        assert!(replace.check_before(None, Some(&token)).is_err());
        assert!(replace.check_before(Some(&other), Some(&token)).is_err());
        let delete = make(None);
        assert!(delete.check_before(None, Some(&token)).is_ok());
        assert!(delete.check_before(Some(&other), Some(&token)).is_ok());
        assert!(
            replace
                .check_fence_header(
                    format!("object {owner}\ntype commit\ntag token\n\nidentity\n").as_bytes()
                )
                .is_ok()
        );
        for body in [
            format!("object {other}\ntype commit\n\n"),
            format!("object {owner}\ntype tag\n\n"),
            format!("object {owner}\ntype commit\nobject {owner}\n\n"),
        ] {
            assert!(replace.check_fence_header(body.as_bytes()).is_err());
        }
    }
}
