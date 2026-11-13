// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable account-description commands for a local durable outbox. These
//! values retain a destination and retry identity, never writer authority.

use crate::{
    Error, Result, crypto,
    database::{Change, Command, decode_hex},
};
use astream_wire::sync::{Actor, Digest};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::Value;
use serde::{Deserialize, Serialize};

pub(super) const MAX_PAYLOAD: usize = 64 * 1024;
const MAX_PENDING: usize = 2 * MAX_PAYLOAD + 2048;

/// Exact historical destination, including its writer incarnation. Decoding
/// this metadata does not admit a current network, table or signing key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    network: String,
    authority: String,
    genesis: String,
    table: String,
    generation: u64,
    actor: String,
}

impl Destination {
    pub fn new(
        network: [u8; 16],
        authority: [u8; 32],
        genesis: Digest,
        table: [u8; 16],
        generation: u64,
        actor: Actor,
    ) -> Result<Self> {
        let destination = Self {
            network: hex::encode_lower(&network),
            authority: hex::encode_lower(&authority),
            genesis: hex::encode_lower(&genesis),
            table: hex::encode_lower(&table),
            generation,
            actor: hex::encode_lower(&actor),
        };
        destination.validate()?;
        Ok(destination)
    }

    #[cfg(unix)]
    pub(super) fn observed(scope: crate::table::Scope, actor: Actor) -> Result<Self> {
        let trust = scope.trust();
        Self::new(
            trust.network,
            trust.authority,
            trust.genesis,
            *scope.table(),
            scope.generation(),
            actor,
        )
    }

    fn validate(&self) -> Result<()> {
        fn canonical<const N: usize>(word: &str) -> Result<[u8; N]> {
            let bytes = decode_hex(word)?;
            if hex::encode_lower(&bytes) != word {
                return Err(Error::new("publication destination is not canonical"));
            }
            Ok(bytes)
        }
        let network = canonical::<16>(&self.network)?;
        let authority = canonical::<32>(&self.authority)?;
        let genesis = canonical::<32>(&self.genesis)?;
        let table = canonical::<16>(&self.table)?;
        let actor = canonical::<48>(&self.actor)?;
        if network == [0; 16]
            || authority == [0; 32]
            || genesis == [0; 32]
            || table == [0; 16]
            || actor == [0; 48]
            || self.generation == 0
        {
            return Err(Error::new(
                "publication destination needs nonzero custody and a table generation",
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn matches(&self, scope: crate::table::Scope, actor: Actor) -> Result<bool> {
        Ok(*self == Self::observed(scope, actor)?)
    }
}

/// One conditional whole-value description change, before a retry identity is
/// allocated. `None` expects a key that has never been occupied.
#[derive(Clone, Debug)]
pub struct Draft {
    destination: Destination,
    change: Change,
    condition: Condition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum Condition {
    NeverUsed,
    Existing {
        operation: String,
    },
    After {
        request: String,
        command_sha256: String,
    },
}

impl Condition {
    fn existing(expected: Option<Digest>) -> Self {
        expected.map_or(Self::NeverUsed, |id| Self::Existing {
            operation: hex::encode_lower(&id),
        })
    }

    fn validate(&self) -> Result<()> {
        fn canonical<const N: usize>(word: &str) -> Result<()> {
            let bytes: [u8; N] = decode_hex(word)?;
            if hex::encode_lower(&bytes) != word {
                return Err(Error::new("publication condition is not canonical"));
            }
            Ok(())
        }
        match self {
            Self::NeverUsed => Ok(()),
            Self::Existing { operation } => canonical::<32>(operation),
            Self::After {
                request,
                command_sha256,
            } => {
                canonical::<16>(request)?;
                canonical::<32>(command_sha256)
            }
        }
    }

    fn legacy_expected(&self) -> Result<Option<String>> {
        match self {
            Self::NeverUsed => Ok(None),
            Self::Existing { operation } => Ok(Some(operation.clone())),
            Self::After { .. } => Err(Error::new("legacy publication cannot name a predecessor")),
        }
    }
}

impl Draft {
    pub fn new(
        destination: Destination,
        collection: String,
        key: String,
        expected: Option<Digest>,
        value: Value,
    ) -> Result<Self> {
        destination.validate()?;
        let change = Change {
            collection,
            key,
            value: Some(value),
        };
        validate_change(&change)?;
        bounded_command([0; 16], std::slice::from_ref(&change))?;
        Ok(Self {
            destination,
            change,
            condition: Condition::existing(expected),
        })
    }

    /// Follow one exact pending put, preserving its destination and key. The
    /// predecessor may still be offline, or already acknowledged locally.
    /// Delivery requires its signed operation to remain the sole current value.
    pub fn after(predecessor: &Pending, value: Value) -> Result<Self> {
        let Command::Write { request, changes } = predecessor.validate()? else {
            return Err(Error::new("publication predecessor needs a put"));
        };
        let [change] = changes.as_slice() else {
            return Err(Error::new("publication predecessor needs one description"));
        };
        let mut draft = Self::new(
            predecessor.destination.clone(),
            change.collection.clone(),
            change.key.clone(),
            None,
            value,
        )?;
        draft.condition = Condition::After {
            request: hex::encode_lower(&request),
            command_sha256: hex::encode_lower(&Sha256::digest(predecessor.command.as_bytes())),
        };
        Ok(draft)
    }

    #[cfg(all(test, unix))]
    pub(super) fn with_reference_for_test(mut self, request: [u8; 16], digest: Digest) -> Self {
        self.condition = Condition::After {
            request: hex::encode_lower(&request),
            command_sha256: hex::encode_lower(&digest),
        };
        self
    }

    /// Allocate once while planning a local transaction, before publishing its
    /// intent. Repeated delivery uses the returned value without reallocating.
    pub fn allocate(self) -> Result<Pending> {
        self.allocate_with(crypto::random()?)
    }

    fn allocate_with(self, nonce: [u8; 16]) -> Result<Pending> {
        let nonce = hex::encode_lower(&nonce);
        let changes = std::slice::from_ref(&self.change);
        let request = request_for(2, &self.destination, &self.condition, &nonce, changes)?;
        let command = bounded_command(request, changes)?;
        let pending = Pending {
            schema: 2,
            destination: self.destination,
            condition: self.condition,
            nonce,
            command,
        };
        pending.validate()?;
        Ok(pending)
    }
}

/// Exact immutable command and condition to retry after an unknown append
/// outcome. The fresh nonce binds both the condition and destination into the
/// request identity; equal A -> B -> A payloads are separate intents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    schema: u8,
    destination: Destination,
    condition: Condition,
    nonce: String,
    command: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyWire {
    schema: u8,
    destination: Destination,
    expected: Option<String>,
    nonce: String,
    command: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentWire {
    schema: u8,
    destination: Destination,
    condition: Condition,
    nonce: String,
    command: String,
}

impl Serialize for Pending {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self.schema {
            1 => LegacyWire {
                schema: 1,
                destination: self.destination.clone(),
                expected: self
                    .condition
                    .legacy_expected()
                    .map_err(serde::ser::Error::custom)?,
                nonce: self.nonce.clone(),
                command: self.command.clone(),
            }
            .serialize(serializer),
            2 => CurrentWire {
                schema: 2,
                destination: self.destination.clone(),
                condition: self.condition.clone(),
                nonce: self.nonce.clone(),
                command: self.command.clone(),
            }
            .serialize(serializer),
            _ => Err(serde::ser::Error::custom("unsupported publication schema")),
        }
    }
}

impl<'de> Deserialize<'de> for Pending {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Legacy(LegacyWire),
            Current(CurrentWire),
        }
        let pending = match Wire::deserialize(deserializer)? {
            Wire::Legacy(wire) if wire.schema == 1 => Self {
                schema: 1,
                destination: wire.destination,
                condition: wire.expected.map_or(Condition::NeverUsed, |operation| {
                    Condition::Existing { operation }
                }),
                nonce: wire.nonce,
                command: wire.command,
            },
            Wire::Current(wire) if wire.schema == 2 => Self {
                schema: 2,
                destination: wire.destination,
                condition: wire.condition,
                nonce: wire.nonce,
                command: wire.command,
            },
            _ => return Err(serde::de::Error::custom("unsupported publication schema")),
        };
        pending.validate().map_err(serde::de::Error::custom)?;
        Ok(pending)
    }
}

impl Pending {
    pub fn destination(&self) -> &Destination {
        &self.destination
    }

    pub fn request(&self) -> Result<[u8; 16]> {
        Ok(self.validate()?.request())
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let encoded =
            aterm_json::to_vec(self).map_err(|_| Error::new("publication encoding failed"))?;
        if encoded.len() > MAX_PENDING {
            return Err(Error::new("publication exceeds its encoded limit"));
        }
        Ok(encoded)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_PENDING {
            return Err(Error::new("publication exceeds its encoded limit"));
        }
        aterm_json::from_slice(bytes).map_err(|_| Error::new("invalid retained publication"))
    }

    pub(super) fn validate(&self) -> Result<Command> {
        if !matches!(self.schema, 1 | 2) {
            return Err(Error::new("unsupported publication schema"));
        }
        self.destination.validate()?;
        let nonce: [u8; 16] = decode_hex(&self.nonce)?;
        if hex::encode_lower(&nonce) != self.nonce {
            return Err(Error::new("publication nonce is not canonical"));
        }
        self.condition.validate()?;
        if self.command.len() > MAX_PAYLOAD {
            return Err(Error::new("publication command exceeds its limit"));
        }
        let command = Command::decode(self.command.as_bytes())?;
        let Command::Write { request, changes } = &command else {
            return Err(Error::new("publication must be a description write"));
        };
        if changes.len() != 1 {
            return Err(Error::new("publication must change one description"));
        }
        validate_change(&changes[0])?;
        if bounded_command(*request, changes)? != self.command
            || *request
                != request_for(
                    self.schema,
                    &self.destination,
                    &self.condition,
                    &self.nonce,
                    changes,
                )?
        {
            return Err(Error::new("publication command or binding changed"));
        }
        Ok(command)
    }

    #[cfg(unix)]
    pub(super) fn condition(&self) -> &Condition {
        &self.condition
    }
}

/// A local application receipt, bound to the exact retained publication. It is
/// neither a peer acknowledgment nor a credential or resource grant.
pub struct Completion {
    pending: Pending,
    operation: Digest,
}

impl Completion {
    #[cfg(unix)]
    pub(super) fn admitted(pending: &Pending, operation: Digest) -> Self {
        Self {
            pending: pending.clone(),
            operation,
        }
    }

    pub fn matches(&self, pending: &Pending) -> bool {
        self.pending == *pending
    }
    pub const fn operation(&self) -> &Digest {
        &self.operation
    }
}

/// Construct only acknowledgment-correlation input for dependent crates' local
/// protocol models. This performs no application append or authority check.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn test_completion(pending: &Pending, operation: Digest) -> Completion {
    Completion {
        pending: pending.clone(),
        operation,
    }
}

fn validate_change(change: &Change) -> Result<()> {
    if !crypto::name(&change.key) {
        return Err(Error::new("invalid publication key"));
    }
    let value = change
        .value
        .as_ref()
        .ok_or_else(|| Error::new("publication needs a description value"))?;
    match change.collection.as_str() {
        "accounts" => super::validate(value),
        "mailboxes" | "email-addresses" | "phones" => {
            crate::contacts::validate(&change.collection, value)
        }
        "machine-proposals" => crate::machines::validate(&change.key, value),
        _ => Err(Error::new("unsupported publication collection")),
    }
}

fn bounded_command(request: [u8; 16], changes: &[Change]) -> Result<String> {
    let encoded = Command::encode_write(request, changes)?;
    if encoded.len() > MAX_PAYLOAD {
        return Err(Error::new("publication command exceeds its limit"));
    }
    String::from_utf8(encoded).map_err(|_| Error::new("publication command is not UTF-8"))
}

fn request_for(
    schema: u8,
    destination: &Destination,
    condition: &Condition,
    nonce: &str,
    changes: &[Change],
) -> Result<[u8; 16]> {
    #[derive(Serialize)]
    struct LegacyBinding<'a> {
        destination: &'a Destination,
        expected: &'a Option<String>,
        nonce: &'a str,
        command: String,
    }
    #[derive(Serialize)]
    struct Binding<'a> {
        destination: &'a Destination,
        condition: &'a Condition,
        nonce: &'a str,
        command: String,
    }
    let command = bounded_command([0; 16], changes)?;
    let bytes = match schema {
        1 => aterm_json::to_vec(&LegacyBinding {
            destination,
            expected: &condition.legacy_expected()?,
            nonce,
            command,
        }),
        2 => aterm_json::to_vec(&Binding {
            destination,
            condition,
            nonce,
            command,
        }),
        _ => return Err(Error::new("unsupported publication schema")),
    }
    .map_err(|_| Error::new("publication binding encoding failed"))?;
    let mut hash = Sha256::new();
    hash.update(if schema == 1 {
        b"aterm/account-publication-request/1\0"
    } else {
        b"aterm/account-publication-request/2\0"
    });
    hash.update(bytes);
    let digest = hash.finalize();
    let mut request = [0; 16];
    request.copy_from_slice(&digest[..16]);
    Ok(request)
}

#[cfg(test)]
#[path = "publication_tests.rs"]
mod tests;
