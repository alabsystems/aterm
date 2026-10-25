// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authority-certified device bindings. A certificate is historical evidence,
//! not current membership: a current validated roster must still include it.

use super::binding::{BINDING_LEN, Binding};
use crate::{Error, Result};
use ring::signature::{ED25519, UnparsedPublicKey};

const DOMAIN: &[u8] = b"aterm/commons/member-certificate/1\0";
/// Exact bounded certificate wire size, including its authority signature.
pub const CERTIFICATE_LEN: usize = 4 + BINDING_LEN + 32 + 64;

/// Public eligibility metadata captured from a verified vault on open. Never
/// consult the caller's mutable edit document while issuing certificates.
#[cfg(unix)]
pub(crate) struct Eligibility {
    recipients: std::collections::BTreeMap<String, crate::age::Recipient>,
}
#[cfg(unix)]
impl Eligibility {
    pub(crate) fn from_document(document: &crate::text::Document) -> Self {
        let mut recipients = std::collections::BTreeMap::new();
        if let Some(crate::text::Audience::Hosts(hosts)) = document.commons_audience() {
            for machine in document.machines() {
                if hosts.binary_search(&machine.name).is_ok()
                    && let Ok(recipient) = crate::age::Recipient::parse(&machine.recipient)
                {
                    recipients.insert(machine.name, recipient);
                }
            }
        }
        Self { recipients }
    }
    pub(crate) fn includes(&self, host: &str, recipient: crate::age::Recipient) -> bool {
        self.recipients.get(host) == Some(&recipient)
    }
}

/// A root-signed statement that the named binding was approved at an exact
/// control head. This grants no administrator role or terminal-drive capability.
#[derive(Clone, Debug)]
pub struct Certificate {
    binding: Binding,
    issued_on: [u8; 32],
    wire: Vec<u8>,
}

impl Certificate {
    /// Authenticate historical evidence against independent network/root pins.
    /// Consumers must also validate current roster inclusion and revocations.
    pub fn verify(wire: &[u8], network: [u8; 16], authority: [u8; 32]) -> Result<Self> {
        if wire.len() != CERTIFICATE_LEN || &wire[..4] != b"AMC1" {
            return Err(Error::new("invalid Commons member certificate"));
        }
        let binding = Binding::decode(&wire[4..4 + BINDING_LEN])?;
        let issued_on = wire[4 + BINDING_LEN..CERTIFICATE_LEN - 64]
            .try_into()
            .unwrap();
        if *binding.network() != network
            || *binding.authority() != authority
            || issued_on == [0; 32]
        {
            return Err(Error::new("Commons certificate has another trust context"));
        }
        let mut signed = DOMAIN.to_vec();
        signed.extend_from_slice(&wire[..CERTIFICATE_LEN - 64]);
        UnparsedPublicKey::new(&ED25519, authority)
            .verify(&signed, &wire[CERTIFICATE_LEN - 64..])
            .map_err(|_| Error::new("Commons certificate signature failed"))?;
        Ok(Self {
            binding,
            issued_on,
            wire: wire.to_vec(),
        })
    }

    /// The exact jointly proven device identity certified by the authority.
    pub fn binding(&self) -> &Binding {
        &self.binding
    }

    /// The complete control head against which approval was checked.
    pub const fn issued_on(&self) -> &[u8; 32] {
        &self.issued_on
    }

    /// Immutable public evidence for a signed roster or peer authentication.
    pub fn bytes(&self) -> &[u8] {
        &self.wire
    }

    /// Content identity, independent of clocks and friendly machine names.
    pub fn id(&self) -> [u8; 32] {
        aterm_digest::Sha256::digest(&self.wire)
    }
}

/// Explicit authority approval. Automatic enrollment must first establish its
/// own durable eligibility evidence; a vault machine-list entry alone is not it.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbMemberCertificate",
        action = "Valid",
        project = "aterm_secrets::commons::membership::tests::approval_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbMemberCertificate",
        action = "ForeignAuthority",
        project = "aterm_secrets::commons::membership::tests::approval_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbMemberCertificate",
        action = "ForeignRecipient",
        project = "aterm_secrets::commons::membership::tests::approval_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbMemberCertificate",
        action = "RetiredWriter",
        project = "aterm_secrets::commons::membership::tests::approval_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbMemberCertificate",
        action = "IncompleteControl",
        project = "aterm_secrets::commons::membership::tests::approval_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbMemberCertificate",
        action = "IneligibleRecipient",
        project = "aterm_secrets::commons::membership::tests::approval_transition"
    )
)]
pub(crate) fn approve(
    seed: &[u8; 32],
    journal: &super::journal::Journal,
    eligibility: &Eligibility,
    host: &str,
    possession: super::binding::Possession,
) -> Result<Certificate> {
    use ring::signature::Ed25519KeyPair;
    let binding = possession.binding();
    check_candidate(seed, journal, eligibility, host, binding)?;
    let signer = Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| Error::new("invalid Commons authority seed"))?;
    let mut wire = b"AMC1".to_vec();
    wire.extend_from_slice(&binding.bytes());
    wire.extend_from_slice(&journal.constraints()?.head());
    let mut signed = DOMAIN.to_vec();
    signed.extend_from_slice(&wire);
    wire.extend_from_slice(signer.sign(&signed).as_ref());
    Certificate::verify(&wire, journal.trust().network, journal.trust().authority)
}

/// Read-only policy check. Only `approve` consumes joint possession and signs a
/// certificate; success here grants no membership or reusable permission.
#[cfg(unix)]
pub(crate) fn check_candidate(
    seed: &[u8; 32],
    journal: &super::journal::Journal,
    eligibility: &Eligibility,
    host: &str,
    binding: &Binding,
) -> Result<()> {
    use ring::signature::{Ed25519KeyPair, KeyPair};
    let trust = journal.trust();
    let constraints = journal.constraints()?;
    if constraints.head() == [0; 32] {
        return Err(Error::new("Commons certificate has another trust context"));
    }
    let signer = Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| Error::new("invalid Commons authority seed"))?;
    let authority = signer.public_key().as_ref() == trust.authority
        && *binding.authority() == trust.authority
        && *binding.network() == trust.network;
    let writer = constraints.check_device_candidate(binding).is_ok();
    let eligible = eligibility.includes(host, binding.recipient());
    if !aterm_db_core::member_approval(authority, eligible, writer) {
        return Err(Error::new(
            "Commons approval requires its pinned authority, eligible recipient and active writer",
        ));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
