// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Value-free claims about an entry's past log selection.

use super::provenance::Source;
use super::*;
use crate::client::version::{AuthenticatedVersion, LogVersion, VersionClaim, VersionRelation};
use crate::membership_log::{MAX_FOUNDERS, RecordId};

const MAGIC: &[u8; 4] = b"AEC1";
/// The complete encoded claim's bound, including its causal frontier.
pub const MAX_ENTRY_CLAIM_BYTES: usize = 64 * 1024;

/// Public metadata describing a past log selection, not an authenticated
/// observation or an authorization to reactivate an account. A decoder checks
/// its representation; a current verified graph must check its assertions.
/// No secret value, value digest, endpoint or private key is retained here.
#[derive(Clone, PartialEq, Eq)]
pub struct EntryClaim {
    version: VersionClaim,
    recipient: Recipient,
    name: String,
    host: String,
    audience: Audience,
    operation: RecordId,
    wrapper: RecordId,
}

/// What a current entry observation proves about one retained claim. None of
/// these variants authorizes a use outside the observation's admission guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryClaimRelation {
    /// The selected semantic write is unchanged, even if re-encrypted.
    Same,
    /// The selected semantic write strictly follows the validated old selection.
    Advanced,
    /// Missing, conflicting, foreign or mismatched evidence proves no relation.
    Unproven,
}

#[cfg(test)]
std::thread_local! {
    static MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static PREFIX_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl std::fmt::Debug for EntryClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryClaim").finish_non_exhaustive()
    }
}

fn invalid() -> Error {
    Error::new("invalid entry selection claim")
}

impl EntryClaim {
    fn validate(&self) -> crate::Result<()> {
        if self.version.log().is_none()
            || !text::valid_entry_name(&self.name)
            || !text::valid_machine_name(&self.host)
            || !crate::x25519::canonical_u(&self.recipient.0)
            || Recipient::parse(&self.recipient.to_bech32()).is_err()
            || self.operation == [0; 32]
            || self.wrapper == [0; 32]
        {
            return Err(invalid());
        }
        if let Audience::Hosts(hosts) = &self.audience
            && (hosts.is_empty()
                || hosts.len() > MAX_FOUNDERS
                || hosts.iter().any(|host| !text::valid_machine_name(host))
                || hosts.windows(2).any(|pair| pair[0] >= pair[1]))
        {
            return Err(invalid());
        }
        if self.encode().len() > MAX_ENTRY_CLAIM_BYTES {
            return Err(invalid());
        }
        Ok(())
    }

    /// Canonical bounded metadata. The bytes are a claim to verify, never a
    /// serialized live observation or a secret-value commitment.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let version = self.version.encode();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(version.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&version);
        bytes.extend_from_slice(&self.recipient.0);
        bytes.extend_from_slice(&(self.name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(self.name.as_bytes());
        bytes.push(self.host.len() as u8);
        bytes.extend_from_slice(self.host.as_bytes());
        match &self.audience {
            Audience::Everyone => bytes.push(0),
            Audience::Hosts(hosts) => {
                bytes.push(1);
                bytes.push(hosts.len() as u8);
                for host in hosts {
                    bytes.push(host.len() as u8);
                    bytes.extend_from_slice(host.as_bytes());
                }
            }
        }
        bytes.extend_from_slice(&self.operation);
        bytes.extend_from_slice(&self.wrapper);
        bytes
    }

    /// Decode one complete canonical claim without granting source authority.
    /// # Errors
    /// The frame is oversized, truncated, noncanonical or names invalid metadata.
    pub fn decode(bytes: &[u8]) -> crate::Result<Self> {
        if bytes.len() > MAX_ENTRY_CLAIM_BYTES {
            return Err(invalid());
        }
        fn take<'a>(left: &mut &'a [u8], n: usize) -> crate::Result<&'a [u8]> {
            let (part, rest) = left.split_at_checked(n).ok_or_else(invalid)?;
            *left = rest;
            Ok(part)
        }
        fn string(left: &mut &[u8], n: usize) -> crate::Result<String> {
            std::str::from_utf8(take(left, n)?)
                .map(str::to_owned)
                .map_err(|_| invalid())
        }
        let mut left = bytes;
        if take(&mut left, 4)? != MAGIC {
            return Err(invalid());
        }
        let n = u32::from_be_bytes(take(&mut left, 4)?.try_into().unwrap()) as usize;
        let version = VersionClaim::decode(take(&mut left, n)?)?;
        let recipient = Recipient(take(&mut left, 32)?.try_into().unwrap());
        let n = u16::from_be_bytes(take(&mut left, 2)?.try_into().unwrap()) as usize;
        let name = string(&mut left, n)?;
        let n = take(&mut left, 1)?[0] as usize;
        let host = string(&mut left, n)?;
        let audience = match take(&mut left, 1)?[0] {
            0 => Audience::Everyone,
            1 => {
                let n = take(&mut left, 1)?[0];
                let mut hosts = Vec::new();
                for _ in 0..n {
                    let n = take(&mut left, 1)?[0] as usize;
                    hosts.push(string(&mut left, n)?);
                }
                Audience::Hosts(hosts)
            }
            _ => return Err(invalid()),
        };
        let operation = take(&mut left, 32)?.try_into().unwrap();
        let wrapper = take(&mut left, 32)?.try_into().unwrap();
        if !left.is_empty() {
            return Err(invalid());
        }
        let claim = Self {
            version,
            recipient,
            name,
            host,
            audience,
            operation,
            wrapper,
        };
        claim.validate()?;
        Ok(claim)
    }

    /// The claimed complete public source frontier.
    #[must_use]
    pub const fn version(&self) -> &VersionClaim {
        &self.version
    }
}

impl EntrySnapshot {
    /// Retain bounded public metadata for this actual log-selected text entry.
    /// Envelope fallback, synthetic entries and unresolved or unreadable
    /// audiences yield no claim. This contains neither a value commitment nor
    /// the configured endpoint. The application must separately retain its
    /// local source policy; decoding a claim cannot restore that policy.
    /// # Errors
    /// The complete claim cannot fit its bound or contains invalid metadata.
    pub fn entry_claim(&self) -> crate::Result<Option<EntryClaim>> {
        let Source::Log {
            history,
            register: Some(register),
        } = &self.source
        else {
            return Ok(None);
        };
        let Some(signer) = self.config.signer else {
            return Ok(None);
        };
        if register.ambiguous || !register.complete {
            return Ok(None);
        }
        let version = VersionClaim::new(
            self.config.name.clone(),
            signer,
            self.envelope_context.clone(),
            Some(LogVersion {
                network: *history.network(),
                heads: history.heads(),
            }),
        )?;
        let claim = EntryClaim {
            version,
            recipient: self.recipient,
            name: self.name.clone(),
            host: self.config.host.clone(),
            audience: register.audience.clone(),
            operation: register.operation,
            wrapper: register.wrapper,
        };
        claim.validate()?;
        Ok(Some(claim))
    }

    /// Check a retained claim against this observation's authenticated graph.
    /// Reconstruct and resolve the claimed complete prefix with this recipient;
    /// finding the old operation somewhere in the graph is insufficient. The
    /// host label must still match, and the ordinary membership and roster
    /// denial rules must allow this reader at the claimed historical prefix.
    ///
    /// Call this with the fresh entry inside [`Self::admit`] when deciding a
    /// durable account transition, after checking the application's independent
    /// source identity and automatic-use policy. This does not reopen the entry
    /// or establish current currency. No private key is retained in a snapshot
    /// or claim: the local identity is read for this comparison and then dropped.
    /// # Errors
    /// The local identity cannot be read. A claimed prefix that cannot be
    /// reconstructed or opened is unproven, not a failure of the current view.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbEntryClaim",
            action = "Case",
            reason = "The environment chooses the retained claim and current authenticated observation."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryClaim",
            action = "Read",
            project = "aterm_secrets::client::entry::claim::tests::transition"
        )
    )]
    pub fn compare_claim(
        &self,
        paths: &Paths,
        claim: &EntryClaim,
    ) -> crate::Result<EntryClaimRelation> {
        use EntryClaimRelation::{Advanced, Same, Unproven};
        let Source::Log {
            history,
            register: Some(current),
        } = &self.source
        else {
            return Ok(Unproven);
        };
        let Some(signer) = self.config.signer else {
            return Ok(Unproven);
        };
        if self.name != claim.name
            || self.config.host != claim.host
            || self.recipient != claim.recipient
            || current.audience != claim.audience
            || current.ambiguous
            || !current.complete
        {
            return Ok(Unproven);
        }
        let version = AuthenticatedVersion::from_source(
            &self.config.name,
            signer,
            self.config.followed.iter().copied(),
            self.envelope_context.clone(),
            Some(history),
        )?;
        if !matches!(
            version.relation(&claim.version),
            VersionRelation::Same | VersionRelation::Advanced
        ) {
            return Ok(Unproven);
        }
        let Some(identity) = read_identity(paths)? else {
            return Ok(Unproven);
        };
        if identity.recipient() != self.recipient {
            return Ok(Unproven);
        }
        let check_old = || -> crate::Result<bool> {
            #[cfg(test)]
            if PREFIX_ERROR.get() {
                return Err(invalid());
            }
            let Some(prefix) = claim.version.log() else {
                return Ok(false);
            };
            // relation above requires every head to be held before project_heads,
            // whose ancestor closure alone would silently omit unknown heads.
            let old = history.project_heads(&prefix.heads)?;
            if old.heads() != prefix.heads {
                return Ok(false);
            }
            let Some(selected) = old.open_selection(
                &identity,
                Some(&[claim.name.as_str(), crate::SWARM_ROSTER_ENTRY]),
            )?
            else {
                return Ok(false);
            };
            let authorized = log_entry_reader_allowed(&old, &identity)
                && selected_entry_reader_allowed(
                    &selected.opened.entries,
                    Some(&old),
                    &self.config,
                    &self.name,
                    self.recipient,
                );
            #[cfg(test)]
            let authorized = authorized || MUTANT.get() == 4;
            if !authorized {
                return Ok(false);
            }
            let key = (claim.audience.clone(), claim.name.clone());
            Ok(selected
                .opened
                .entries
                .iter()
                .any(|entry| entry.name == claim.name && entry.audience == claim.audience)
                && selected.operations.get(&key) == Some(&claim.operation)
                && selected.puts.get(&key) == Some(&claim.wrapper)
                && !selected.opened.conflicts.contains(&key)
                && selected.complete.contains(&claim.audience))
        };
        let old_result = check_old();
        #[cfg(test)]
        let old_result = if MUTANT.get() == 5 {
            Ok(old_result?)
        } else {
            old_result
        };
        let old_selected = old_result.unwrap_or(false);
        #[cfg(test)]
        let old_selected = old_selected || MUTANT.get() == 1;
        if !old_selected {
            return Ok(Unproven);
        }
        let (operation, previous) = (current.operation, claim.operation);
        #[cfg(test)]
        let (operation, previous) = if MUTANT.get() == 2 {
            (current.wrapper, claim.wrapper)
        } else {
            (operation, previous)
        };
        if operation == previous {
            Ok(Same)
        } else if history.past_of(&[operation]).holds(&previous) {
            #[cfg(test)]
            if MUTANT.get() == 3 {
                return Ok(Unproven);
            }
            Ok(Advanced)
        } else {
            Ok(Unproven)
        }
    }
}

#[cfg(test)]
mod tests;
