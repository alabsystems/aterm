// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Historical evidence of an actual signed URL response admitted to local files.

use super::*;

/// The role of the URL that supplied a network observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VaultNetworkSource {
    /// The configured URL supplied the observation.
    ConfiguredUrl,
    /// A probed URL supplied the observation; no serving machine identity is implied.
    PeerUrl,
}

impl VaultNetworkSource {
    const fn word(self) -> &'static str {
        match self {
            Self::ConfiguredUrl => "configured-url",
            Self::PeerUrl => "peer-url",
        }
    }
}

/// A historical network response and local materialization, with no endpoint or payload.
/// This evidence establishes neither remote currency nor provider sign-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VaultNetworkMaterialization {
    source: VaultNetworkSource,
    sequence: u64,
    sampled_at_ms: u64,
}

impl VaultNetworkMaterialization {
    /// The role of the URL that supplied the body.
    pub const fn source(self) -> VaultNetworkSource {
        self.source
    }

    /// The signed envelope sequence locally admitted during that observation.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// The original wall-clock sample taken before the request, never a read time.
    pub const fn sampled_at_ms(self) -> u64 {
        self.sampled_at_ms
    }
}

/// Request context captured before any network response or local substitution.
#[derive(Clone, Copy)]
pub(super) struct Attempt {
    source: VaultNetworkSource,
    configuration: [u8; 32],
    recipient: [u8; 32],
    sampled_at_ms: u64,
    // Preserve the original process-local sample through admission, without ordering commits
    // by start time: an older request can legitimately be the last successful commit.
    _sampled_at: Instant,
}

impl Attempt {
    pub(super) fn start(
        paths: &Paths,
        config: &Config,
        source: VaultNetworkSource,
    ) -> Option<Self> {
        if !matches!(config.source, Source::Url(_)) || config.signer.is_none() {
            return None;
        }
        let recipient = read_identity(paths).ok()??.recipient().0;
        let sampled_at_ms =
            aterm_time::unix_millis().filter(|at| *at > 0 && *at <= i64::MAX as u64)?;
        Some(Self {
            source,
            configuration: configuration_binding(config),
            recipient,
            sampled_at_ms,
            _sampled_at: Instant::now(),
        })
    }

    pub(super) fn received(self, fetched: &Fetched) -> Option<Body> {
        let Fetched::Body(bytes) = fetched else {
            return None;
        };
        Some(Body {
            attempt: self,
            digest: sha256(bytes),
        })
    }
}

/// Evidence is bound to the exact bytes returned by a real URL request.
#[derive(Clone, Copy)]
pub(super) struct Body {
    attempt: Attempt,
    digest: [u8; 32],
}

/// Raw caller bytes and local cache/path reads carry no network provenance.
pub(super) struct Received {
    pub(super) fetched: Fetched,
    pub(super) body: Option<Body>,
}

impl From<Fetched> for Received {
    fn from(fetched: Fetched) -> Self {
        Self {
            fetched,
            body: None,
        }
    }
}

/// Private persisted context; only the closed three-field observation leaves this module.
#[derive(Clone, Debug)]
pub(super) struct Receipt {
    configuration: [u8; 32],
    source_identity: [u8; 32],
    recipient: [u8; 32],
    sequence: u64,
    digest: [u8; 32],
    records_digest: Option<[u8; 32]>,
    observation: VaultNetworkMaterialization,
}

pub(super) fn configuration_binding(config: &Config) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm vault network configuration/1\0");
    digest.update(source_identity(config));
    digest.update((config.followed.len() as u64).to_be_bytes());
    for signer in &config.followed {
        digest.update(signer);
    }
    digest.update((config.host.len() as u64).to_be_bytes());
    digest.update(config.host.as_bytes());
    digest.finalize()
}

/// Called only after the ordinary signed-envelope verifier and successful apply/unchanged
/// branch. The extra readiness check excludes that branch's deferred-repair shortcut.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "ConfiguredBody",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "PeerBody",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "FreshSameVersion",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "EarlierStartLaterCommit",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "RawBody",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "NoNewer",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "Missing",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "Path",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "CachedSubstitute",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "ChangedConfiguration",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "ChangedRecipient",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "UnboundSource",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "WrongSequence",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "WrongEnvelopeDigest",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "FailedApply",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "PendingApply",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "NoOwnership",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "Revoked",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "CacheMissing",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "FileMissing",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "FileChanged",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "MissingIdentity",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "PlainSource",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "UnknownMembership",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationAdmission",
        action = "ChangedRequestedContext",
        project = "aterm_secrets::client::network_observation::conformance_tests::admission_transition"
    )
)]
pub(super) fn record(
    paths: &Paths,
    lock: &StateLock,
    requested: &Config,
    config: &Config,
    state: &mut State,
    envelope: &Envelope,
    body: Option<Body>,
) -> bool {
    let Some(body) = body else {
        return false;
    };
    if body.attempt.configuration != configuration_binding(requested)
        || body.digest != envelope.digest()
        || requested.source != config.source
        || requested.name != config.name
        || requested.host != config.host
        || !matches!(config.source, Source::Url(_))
        || super::config(paths).ok().flatten().as_ref() != Some(config)
        || !pin_admitted(config, None)
        || state.membership != Stored::In
        || state.failed
        || !state.pending.is_empty()
        || state.own.is_none()
        || !source_bound(config, state)
        || state.seq != envelope.seq
        || state.digest != Some(body.digest)
        || state.seq == 0
        || state.seq > i64::MAX as u64
        || read_identity(paths)
            .ok()
            .flatten()
            .map(|key| key.recipient().0)
            != Some(body.attempt.recipient)
        || steward::local_recipient_denied_in_roster_held(paths, lock)
        || !files_intact(paths, state)
    {
        return false;
    }
    state.network_materialization = Some(Receipt {
        configuration: configuration_binding(config),
        source_identity: source_identity(config),
        recipient: body.attempt.recipient,
        sequence: envelope.seq,
        digest: body.digest,
        records_digest: state.records_digest,
        observation: VaultNetworkMaterialization {
            source: body.attempt.source,
            sequence: envelope.seq,
            sampled_at_ms: body.attempt.sampled_at_ms,
        },
    });
    true
}

/// Called inside the existing successful readiness observation, under its shared guard.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "Matching",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "NoReceipt",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedSource",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedName",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedPin",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedFollowed",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedHost",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedRecipient",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedSequence",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedEnvelope",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedRecords",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ChangedSourceIdentity",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "StateReload",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "ReadAgain",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "MissingIdentity",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "MalformedReceipt",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkMaterializationProjection",
        action = "LegacyState",
        project = "aterm_secrets::client::network_observation::conformance_tests::projection_transition"
    )
)]
pub(super) fn project(
    paths: &Paths,
    config: &Config,
    state: &State,
) -> Option<VaultNetworkMaterialization> {
    let receipt = state.network_materialization.as_ref()?;
    (matches!(config.source, Source::Url(_))
        && receipt.configuration == configuration_binding(config)
        && receipt.source_identity == source_identity(config)
        && state.source_identity == Some(receipt.source_identity)
        && receipt.sequence == state.seq
        && state.digest == Some(receipt.digest)
        && receipt.records_digest == state.records_digest
        && read_identity(paths)
            .ok()
            .flatten()
            .map(|key| key.recipient().0)
            == Some(receipt.recipient))
    .then_some(receipt.observation)
}

impl Receipt {
    pub(super) fn parse(table: &aterm_toml::Table) -> Option<Self> {
        let table = table.get("network_materialization")?.as_table()?;
        let int = |key| {
            table
                .get(key)?
                .as_integer()
                .and_then(|n| u64::try_from(n).ok())
                .filter(|n| *n > 0)
        };
        let digest = |key| {
            let text = table.get(key)?.as_str()?;
            if text.len() != 64 {
                return None;
            }
            let bytes = aterm_codec::hex::decode_vec(text.as_bytes())?;
            <[u8; 32]>::try_from(bytes.as_slice()).ok()
        };
        if int("version")? != 1 {
            return None;
        }
        let source = match table.get("source")?.as_str()? {
            "configured-url" => VaultNetworkSource::ConfiguredUrl,
            "peer-url" => VaultNetworkSource::PeerUrl,
            _ => return None,
        };
        let sequence = int("sequence")?;
        Some(Self {
            configuration: digest("configuration")?,
            source_identity: digest("source_identity")?,
            recipient: digest("recipient")?,
            sequence,
            digest: digest("digest")?,
            records_digest: if table.get("records_digest").is_some() {
                Some(digest("records_digest")?)
            } else {
                None
            },
            observation: VaultNetworkMaterialization {
                source,
                sequence,
                sampled_at_ms: int("sampled_at_ms")?,
            },
        })
    }

    pub(super) fn write(&self, text: &mut String) {
        text.push_str("\n[network_materialization]\nversion = 1\n");
        text.push_str(&format!(
            "source = {}\nsequence = {}\nsampled_at_ms = {}\n",
            toml_str(self.observation.source.word()),
            self.sequence,
            self.observation.sampled_at_ms
        ));
        for (key, value) in [
            ("configuration", self.configuration),
            ("source_identity", self.source_identity),
            ("recipient", self.recipient),
            ("digest", self.digest),
        ] {
            text.push_str(&format!("{key} = {}\n", toml_str(&hex(&value))));
        }
        if let Some(digest) = self.records_digest {
            text.push_str(&format!("records_digest = {}\n", toml_str(&hex(&digest))));
        }
    }
}

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod tests;
