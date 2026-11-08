// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The publication export's local mainline prerequisite, shared by its producer and caller.
//! This read never fetches: the producer rechecks it before running the engine's guards.

/// The exact local tracking ref the publication engine requires.
pub const REF: &str = "refs/remotes/origin/main";

/// A local Git read's hang detector, not a network or build deadline.
pub const READ_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Why a revision cannot yet be sent to the publication export producer.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Git read both commits and the revision is not an ancestor of the tracking ref.
    NotOnMain,
    /// Git could not establish their ancestry; this is no finding about the revision.
    Unread(String),
}

impl Refusal {
    /// The prerequisite as this clone's existing refs establish it.
    #[must_use]
    pub fn why(&self, revision: &str) -> String {
        match self {
            Self::NotOnMain => format!(
                "{revision} is not an ancestor of {REF} as this clone last fetched it; the \
                 publication engine stages only mainline commits"
            ),
            Self::Unread(why) => format!("cannot read {revision}'s ancestry against {REF}: {why}"),
        }
    }

    /// The source action needed before retrying; pushing a feature branch is insufficient.
    #[must_use]
    pub const fn remedy(&self) -> &'static str {
        match self {
            Self::NotOnMain => {
                "land the commit on origin/main, or run `git fetch origin` if it is there \
                 already, then run this check again; pushing a feature branch does not qualify"
            }
            Self::Unread(_) => {
                "restore this clone's readable commit objects and refs/remotes/origin/main \
                 (`git fetch origin` if needed), then run this check again"
            }
        }
    }
}

/// Require a revision to be on this clone's locally fetched mainline. `git` performs this one
/// read with its caller's bounded Git adapter and returns its exit code and diagnostic text.
///
/// # Errors
/// Git exit 1 means the revision is not on main; any other failure leaves ancestry unread.
pub fn require(
    revision: &str,
    git: impl FnOnce(&[&str]) -> Result<(Option<i32>, String), String>,
) -> Result<(), Refusal> {
    if revision.is_empty() {
        return Err(Refusal::Unread(
            "no source revision was supplied".to_owned(),
        ));
    }
    let (code, diagnostic) =
        git(&["merge-base", "--is-ancestor", revision, REF]).map_err(Refusal::Unread)?;
    match code {
        Some(0) => Ok(()),
        Some(1) => Err(Refusal::NotOnMain),
        code => Err(Refusal::Unread(format!(
            "git merge-base ended with {code:?}: {}",
            diagnostic.trim()
        ))),
    }
}
