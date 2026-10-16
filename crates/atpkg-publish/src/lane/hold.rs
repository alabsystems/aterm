// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The coherence hold: a coherence group moves all-or-nothing, so when one member is not
//! ready for a target, the others are packed and staged but not pinned (Apple) or not
//! signed (Linux) for that target. A partial tuple aborts the whole group on every client
//! of that target, where no rows at all is a clean skip.
//!
//! The Apple lane holds a group as a whole (the `rustc` group ships aarch64 only); the
//! Linux lane per (group, target), so a group complete on one architecture keeps
//! publishing it while the other waits.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};
use crate::triple::Triple;

/// The scope a hold applies to: a whole group (`None`), or one target of it.
pub type Scope = Option<Triple>;

/// Which (group, scope) pairs are held, and by which members.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Holds(BTreeMap<(String, Scope), BTreeSet<String>>);

impl Holds {
    /// No group held.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `member` of `group` is not ready for `scope`: the group holds there.
    pub fn not_ready(&mut self, group: &str, scope: Scope, member: &str) {
        self.0
            .entry((group.to_string(), scope))
            .or_default()
            .insert(member.to_string());
    }

    /// The members holding `group` at `scope`, if it is held there (a whole-group hold
    /// holds every target too).
    #[must_use]
    pub fn holding(&self, group: &str, scope: Scope) -> Option<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for s in [scope, None] {
            if let Some(m) = self.0.get(&(group.to_string(), s)) {
                out.extend(m.iter().cloned());
            }
            if scope.is_none() {
                break;
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// Whether `group` holds at `scope`.
    #[must_use]
    pub fn is_held(&self, group: &str, scope: Scope) -> bool {
        self.holding(group, scope).is_some()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The holds the members' readiness puts on their groups, from `(group, member, scope,
/// ready)` tuples.
#[must_use]
pub fn of_members<'a>(members: impl IntoIterator<Item = (&'a str, &'a str, Scope, bool)>) -> Holds {
    let mut holds = Holds::new();
    for (group, member, scope, ready) in members {
        if !ready {
            holds.not_ready(group, scope, member);
        }
    }
    holds
}

/// Every pinned program's coherence group as clients enforce it: the verified head's
/// `coherence_group`, else the [`crate::programs::PROGRAMS`] table's (a fallback that can
/// only make a hold stricter). A head and table naming different groups refuse the run: a
/// hold computed on the wrong group could pin or sign a partial tuple.
///
/// # Errors
///
/// The divergence, naming both groups.
pub fn groups_of<'a>(
    head: &atpkg::Index,
    pinned: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, Option<String>>> {
    let mut out = BTreeMap::new();
    for name in pinned {
        let from_head = head
            .programs
            .get(name)
            .and_then(|p| p.coherence_group.as_deref());
        let from_table = crate::programs::get(name).and_then(|d| d.group);
        if let (Some(h), Some(t)) = (from_head, from_table)
            && h != t
        {
            return Err(Error::refused(format!(
                "index-{} puts {name} in coherence group `{h}`, but this build's program table \
                 says `{t}` — the hold would be computed on the wrong group; re-install the lane \
                 from a tree whose table matches the channel",
                head.index_build
            )));
        }
        out.insert(
            name.to_string(),
            from_head.or(from_table).map(str::to_string),
        );
    }
    Ok(out)
}
