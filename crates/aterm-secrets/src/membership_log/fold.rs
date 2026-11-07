// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.2 M6, §11 B11): the Commons network key
//! derived from an `[everyone]` epoch, so `[everyone]` is the one keyed audience a removal
//! rotates.
//!
//! A table operation sealed under the fold names the `[everyone]` epoch its key derives from in
//! the slot an older operation names its Commons epoch in (the roster basis), so one lookup of
//! that id in the log says which derivation opens it, and a reader opens any operation whose
//! epoch it holds, whichever head it came from. The key is
//! `HKDF-SHA-256(ikm = [everyone] key, info = "aterm/commons/network-key/2\0" ‖ network ‖ epoch)`
//! with an empty salt: one way from the `[everyone]` key, so a history handoff of a derived key
//! (to a member admitted after its epoch) opens that epoch's table operations and none of the
//! vault entries the `[everyone]` epoch seals.
//!
//! Which epoch a writer seals under (M6): the newest it holds that is not due a rotation (P1's
//! one rule, [`Log::head_due`]: its gate is open and it wraps no removed recipient); among
//! concurrent newest epochs that qualify, the lowest id, as the vault does for twins. When every
//! newest epoch it holds is due, the lowest one is its view with [`CommonsEpochView::due`] set,
//! and the writer waits (M4, `aterm_mesh_core::capacity::writer_waits`) until the rotation
//! arrives. Concurrent
//! heads withhold nothing: both are member-signed log records.

use super::{Body, EpochAudience, Log, RecordId, entries::CommonsEpochView};
use crate::commons::binding::Binding;
use crate::{Result, age::Identity};
use zeroize::Zeroizing;

/// The HKDF info label of the fold's network key, before the network and epoch ids.
pub const NETWORK_KEY_INFO: &[u8] = b"aterm/commons/network-key/2\0";

/// The Commons network key derived from the `[everyone]` epoch `epoch` of the log `network`,
/// whose key is `everyone_key`.
#[must_use]
pub fn fold_network_key(
    network: &RecordId,
    epoch: &RecordId,
    everyone_key: &[u8; 32],
) -> Zeroizing<[u8; 32]> {
    let prk = Zeroizing::new(aterm_digest::hkdf_extract(&[], everyone_key));
    let mut info = [0u8; NETWORK_KEY_INFO.len() + 64];
    info[..NETWORK_KEY_INFO.len()].copy_from_slice(NETWORK_KEY_INFO);
    info[NETWORK_KEY_INFO.len()..NETWORK_KEY_INFO.len() + 32].copy_from_slice(network);
    info[NETWORK_KEY_INFO.len() + 32..].copy_from_slice(epoch);
    Zeroizing::new(aterm_digest::hkdf_expand_32(&prk, &info))
}

impl Log {
    /// The generation and commitment of the counting `[everyone]` epoch `epoch`, or `None` where
    /// it is no such epoch in this log.
    fn fold_epoch(&self, epoch: &RecordId) -> Option<(usize, u64, [u8; 32])> {
        let &at = self.held.get(epoch)?;
        match self.order[at].body() {
            Body::Epoch {
                audience: EpochAudience::Everyone,
                generation,
                commitment,
            } if self.effective().counts(at) => Some((at, *generation, *commitment)),
            _ => None,
        }
    }

    /// Whether `identity` is a current seat the `[everyone]` audience keeps: the condition under
    /// which it opens fold keys at all, as [`Log::commons_epoch_view`] asks of Commons.
    fn fold_reader(&self, identity: &Identity) -> bool {
        let recipient = identity.recipient();
        let membership = self.membership_ref();
        !self.recipient_is_removed(&recipient)
            && !self.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
            && membership.members().iter().any(|(actor, m)| {
                m.binding.recipient() == recipient
                    && !membership.member_node_removed(m)
                    && !self.audience_left_out(&EpochAudience::Everyone, actor)
            })
    }

    /// The `[everyone]` key of `epoch` from `identity`'s own counting `Wrap`, checked against the
    /// epoch's commitment, or `None` where no such `Wrap` opens.
    fn open_own_wrap(
        &self,
        identity: &Identity,
        epoch: &RecordId,
        generation: u64,
        commitment: &[u8; 32],
    ) -> Result<Option<Zeroizing<[u8; 32]>>> {
        let recipient = identity.recipient();
        let effective = self.effective();
        for (i, record) in self.order.iter().enumerate().skip(1) {
            if let Body::Wrap {
                epoch: wrapped,
                recipient: to,
                capsule,
            } = record.body()
                && effective.counts(i)
                && wrapped == epoch
                && *to == recipient
                && let Some(key) = super::entries::open_wrap(
                    identity,
                    self.network(),
                    epoch,
                    &EpochAudience::Everyone,
                    generation,
                    commitment,
                    capsule,
                )?
            {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    /// The fold's view for `identity`: the `[everyone]` epoch a table writer seals under (the
    /// module's rule), its generation and its derived network key. `None` where `identity` is no
    /// current `[everyone]` seat or holds no newest `[everyone]` epoch's key.
    ///
    /// # Errors
    /// A `Wrap` addressed to `identity` fails authentication or its commitment.
    pub fn fold_epoch_view(&self, identity: &Identity) -> Result<Option<CommonsEpochView>> {
        if !self.fold_reader(identity) {
            return Ok(None);
        }
        let mut newest = self.newest_epochs(&EpochAudience::Everyone);
        newest.sort_unstable();
        let mut held = Vec::new();
        for epoch in newest {
            let Some((_, generation, commitment)) = self.fold_epoch(&epoch) else {
                continue;
            };
            if let Some(key) = self.open_own_wrap(identity, &epoch, generation, &commitment)? {
                held.push((epoch, generation, key, self.table_basis_due(&epoch)));
            }
        }
        let chosen = match held.iter().position(|(_, _, _, due)| !due) {
            Some(at) => held.swap_remove(at),
            None if !held.is_empty() => held.swap_remove(0),
            None => return Ok(None),
        };
        let (basis, generation, key, due) = chosen;
        Ok(Some(CommonsEpochView {
            basis,
            generation,
            key: fold_network_key(self.network(), &basis, &key),
            due,
        }))
    }

    /// Writer bindings eligible to authenticate table operations sealed under the `[everyone]`
    /// epoch `epoch` (the same historical rule as a Commons epoch's), with its generation, or
    /// `None` where `epoch` is no counting `[everyone]` epoch.
    #[must_use]
    pub fn historical_fold_writers(&self, epoch: &RecordId) -> Option<(u64, Vec<Binding>)> {
        let (at, generation, _) = self.fold_epoch(epoch)?;
        let writers =
            super::eval::historical_bindings_for_epoch(self.history(), self.weighing(), at)?;
        Some((generation, writers))
    }

    /// The fold network key of the `[everyone]` epoch `epoch` through `identity`'s own `Wrap` of
    /// it, or `None` where `identity` holds none (a Mac admitted after it reads it through a
    /// history handoff).
    ///
    /// # Errors
    /// The `Wrap` fails authentication or its commitment.
    pub fn unwrap_fold_epoch(
        &self,
        epoch: &RecordId,
        identity: &Identity,
    ) -> Result<Option<Zeroizing<[u8; 32]>>> {
        if !self.fold_reader(identity) {
            return Ok(None);
        }
        let Some((_, generation, commitment)) = self.fold_epoch(epoch) else {
            return Ok(None);
        };
        Ok(self
            .open_own_wrap(identity, epoch, generation, &commitment)?
            .map(|key| fold_network_key(self.network(), epoch, &key)))
    }

    /// Every counting `[everyone]` epoch, in record order: with [`Log::commons_epoch_ids`], the
    /// sources a history handoff can carry a table key of.
    #[must_use]
    pub fn fold_epoch_ids(&self) -> Vec<RecordId> {
        let effective = self.effective();
        self.order
            .iter()
            .enumerate()
            .skip(1)
            .filter(|&(i, record)| {
                effective.counts(i)
                    && matches!(
                        record.body(),
                        Body::Epoch {
                            audience: EpochAudience::Everyone,
                            ..
                        }
                    )
            })
            .map(|(_, record)| *record.id())
            .collect()
    }

    /// Whether `epoch` is a counting `[everyone]` epoch: a table operation naming it was sealed
    /// under the fold.
    #[must_use]
    pub fn is_fold_epoch(&self, epoch: &RecordId) -> bool {
        self.fold_epoch(epoch).is_some()
    }
}
