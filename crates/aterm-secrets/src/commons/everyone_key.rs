// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The newest `[everyone]` epoch this Mac holds: its id and key, and nothing else
//! (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.8, §11 A7). A keyed digest of a lab table
//! (`aterm db emails --digest`) and of a fleet_db replica is keyed by it, so two members compare
//! a table under one epoch without a value leaving either Mac, and anyone without the key (a
//! model provider reading an agent's transcript included) cannot test a guess against a digest.
//!
//! The rule is M6's tie-break: the lowest id among the causally newest counting `[everyone]`
//! epochs, when this Mac holds its key. No older epoch stands in, so members with one log name one
//! epoch, and a Mac that lacks the newest key (a wrap gap, a removal) gets `None` and says so
//! rather than digesting under a key the others have moved past. The read writes nothing.

use crate::membership_log::RecordId;
use std::fmt;
use zeroize::Zeroizing;

/// Hang detector for a required read of the authenticated local membership history.
const BUSY_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The newest `[everyone]` epoch's id and key. Its `Debug` prints the id only.
pub struct EveryoneKey {
    /// The epoch record's id.
    pub epoch: RecordId,
    /// Its 32-byte key, zeroized on drop.
    pub key: Zeroizing<[u8; 32]>,
}

impl fmt::Debug for EveryoneKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EveryoneKey")
            .field("epoch", &aterm_codec::hex::encode_lower(&self.epoch))
            .finish_non_exhaustive()
    }
}

/// [`newest_until`] waiting at most [`BUSY_WAIT`] on a writer.
///
/// # Errors
/// As [`newest_until`].
pub fn newest(paths: &crate::client::Paths) -> crate::Result<Option<EveryoneKey>> {
    newest_started(paths, std::time::Instant::now())
}

fn newest_started(
    paths: &crate::client::Paths,
    started: std::time::Instant,
) -> crate::Result<Option<EveryoneKey>> {
    newest_until(paths, started + BUSY_WAIT)
}

/// The newest `[everyone]` epoch this Mac holds, read from its authenticated local membership
/// history under a shared lock: `None` where it has no history or identity, or does not hold the
/// newest epoch's key.
///
/// # Errors
/// The local history cannot be read or authenticated, a writer holds it past `deadline`, or a
/// wrap addressed to this Mac does not open.
pub fn newest_until(
    paths: &crate::client::Paths,
    deadline: std::time::Instant,
) -> crate::Result<Option<EveryoneKey>> {
    Ok(crate::client::newest_everyone_until(paths, deadline)?
        .map(|(epoch, key)| EveryoneKey { epoch, key }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scheduled_writer_preserves_the_newest_everyone_key() {
        let lab = crate::client::two_log_macs_for_test(&[(
            crate::text::Audience::Everyone,
            "epoch-probe",
            b"fixture",
        )]);
        let paths = lab.m22.clone();
        let expected = newest(&paths).unwrap().unwrap();
        let held = std::rc::Rc::new(std::cell::RefCell::new(Some(
            crate::client::test_hold_state_lock(&paths).unwrap(),
        )));
        let release = std::rc::Rc::clone(&held);
        crate::client::OBSERVE_BLOCKED.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                release.borrow_mut().take();
            }));
        });
        let observed = newest_started(
            &paths,
            std::time::Instant::now() - std::time::Duration::from_secs(11),
        );
        crate::client::OBSERVE_BLOCKED.with(|hook| hook.borrow_mut().take());
        let observed = observed.unwrap().unwrap();
        assert!(held.borrow().is_none());
        assert_eq!(observed.epoch, expected.epoch);
        assert!(*observed.key == *expected.key);

        let owner = crate::client::test_hold_state_lock(&paths).unwrap();
        for refused in [
            newest_started(
                &paths,
                std::time::Instant::now() - std::time::Duration::from_secs(61),
            ),
            newest_until(&paths, std::time::Instant::now()),
        ] {
            assert_eq!(refused.unwrap_err().kind(), crate::ErrorKind::VaultBusy);
        }
        drop(owner);
        let fresh = newest(&paths).unwrap().unwrap();
        assert_eq!(fresh.epoch, expected.epoch);
        assert!(*fresh.key == *expected.key);
    }
}
