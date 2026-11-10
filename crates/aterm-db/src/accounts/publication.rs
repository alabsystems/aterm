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
mod tests {
    use super::*;
    use aterm_json::json;

    fn draft(expected: Option<Digest>) -> Draft {
        Draft::new(
            Destination::new([1; 16], [2; 32], [3; 32], [4; 16], 1, [5; 48]).unwrap(),
            "accounts".into(),
            "one".into(),
            expected,
            json!({"provider":"claude", "method":"cli", "email":"one@example.test"}),
        )
        .unwrap()
    }

    #[test]
    fn retained_identity_binds_condition_destination_and_command() {
        let pending = draft(None).allocate_with([6; 16]).unwrap();
        assert_eq!(
            Pending::decode(&pending.encode().unwrap()).unwrap(),
            pending
        );
        for field in ["condition", "destination", "command", "nonce", "unknown"] {
            let value = aterm_json::to_value(&pending).unwrap();
            let mut fields = value.as_object().unwrap().clone();
            match field {
                "condition" => {
                    fields.insert(
                        "condition".into(),
                        json!({"kind":"existing", "operation":hex::encode_lower(&[7; 32])}),
                    );
                }
                "destination" => {
                    fields
                        .get_mut("destination")
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .insert("actor".into(), Value::String(hex::encode_lower(&[7; 48])));
                }
                "command" => {
                    let mut command = pending.validate().unwrap();
                    let Command::Write { changes, .. } = &mut command else {
                        unreachable!()
                    };
                    changes[0]
                        .value
                        .as_mut()
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .insert("email".into(), Value::String("two@example.test".into()));
                    fields.insert(
                        "command".into(),
                        Value::String(String::from_utf8(command.encode().unwrap()).unwrap()),
                    );
                }
                "nonce" => {
                    fields.insert("nonce".into(), Value::String(hex::encode_lower(&[7; 16])));
                }
                _ => {
                    fields.insert("unrecognized".into(), Value::Bool(true));
                }
            }
            assert!(
                Pending::decode(&aterm_json::to_vec(&fields).unwrap()).is_err(),
                "{field}"
            );
        }
        let again = draft(None).allocate_with([7; 16]).unwrap();
        let changed_condition = draft(Some([7; 32])).allocate_with([6; 16]).unwrap();
        assert_ne!(
            pending.validate().unwrap().request(),
            again.validate().unwrap().request()
        );
        assert_ne!(
            pending.validate().unwrap().request(),
            changed_condition.validate().unwrap().request()
        );
    }

    #[test]
    fn original_v1_bindings_survive_decode_and_reencode() {
        // Frozen request IDs from the v1 canonical binding, independently hashed.
        for (expected, request) in [
            (None, "6f26e91421ae7d6c724c07cfe8881f05"),
            (Some([7; 32]), "710dd53850bbfcb85b59c219ba859a3b"),
        ] {
            let draft = draft(expected);
            let request: [u8; 16] = decode_hex(request).unwrap();
            let wire = LegacyWire {
                schema: 1,
                destination: draft.destination,
                expected: expected.map(|id| hex::encode_lower(&id)),
                nonce: hex::encode_lower(&[6; 16]),
                command: bounded_command(request, &[draft.change]).unwrap(),
            };
            let bytes = aterm_json::to_vec(&wire).unwrap();
            let pending = Pending::decode(&bytes).unwrap();
            assert_eq!(pending.request().unwrap(), request);
            assert_eq!(pending.encode().unwrap(), bytes);
            let next = Draft::after(&pending, json!({"provider":"claude", "method":"cli"}))
                .unwrap()
                .allocate_with([8; 16])
                .unwrap();
            assert_eq!(next.schema, 2);
            assert!(matches!(next.condition, Condition::After { .. }));
            assert_eq!(Pending::decode(&next.encode().unwrap()).unwrap(), next);
        }
    }

    #[test]
    fn predecessor_binding_and_version_shapes_cannot_be_changed() {
        let predecessor = draft(None).allocate_with([6; 16]).unwrap();
        let pending = Draft::after(&predecessor, json!({"provider":"claude", "method":"cli"}))
            .unwrap()
            .allocate_with([8; 16])
            .unwrap();
        for field in ["request", "command_sha256", "schema", "legacy", "unknown"] {
            let mut wire = aterm_json::to_value(&pending).unwrap();
            let fields = wire.as_object_mut().unwrap();
            match field {
                "request" | "command_sha256" => {
                    fields
                        .get_mut("condition")
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .insert(
                            field.into(),
                            json!("09".repeat(if field == "request" { 16 } else { 32 })),
                        );
                }
                "schema" => {
                    fields.insert("schema".into(), json!(1));
                }
                "legacy" => {
                    fields.insert("expected".into(), Value::Null);
                }
                _ => {
                    fields
                        .get_mut("condition")
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .insert("extra".into(), Value::Null);
                }
            }
            assert!(
                Pending::decode(&aterm_json::to_vec(&wire).unwrap()).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn only_bounded_classified_metadata_can_be_queued() {
        let destination = draft(None).destination;
        for (collection, key, value) in [
            (
                "budgets",
                "one",
                json!({"provider":"claude", "method":"cli"}),
            ),
            (
                "accounts",
                "../one",
                json!({"provider":"claude", "method":"cli"}),
            ),
            (
                "accounts",
                "one",
                json!({"provider":"claude", "method":"cli", "unclassified":"value"}),
            ),
            ("phones", "one", json!({"number":"not-a-number"})),
            ("phones", "one", Value::Null),
        ] {
            assert!(
                Draft::new(
                    destination.clone(),
                    collection.into(),
                    key.into(),
                    None,
                    value
                )
                .is_err()
            );
        }
        let mut value = json!({"provider":"claude", "method":"cli", "details":{}});
        for n in 0..40 {
            value
                .as_object_mut()
                .unwrap()
                .get_mut("details")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(
                    format!("note{n}"),
                    json!({"kind":"note", "value":"x".repeat(2048)}),
                );
        }
        assert!(Draft::new(destination, "accounts".into(), "one".into(), None, value).is_err());
    }

    #[test]
    fn destination_rejects_zero_custody_components() {
        for field in 0..6 {
            assert!(
                Destination::new(
                    if field == 0 { [0; 16] } else { [1; 16] },
                    if field == 1 { [0; 32] } else { [2; 32] },
                    if field == 2 { [0; 32] } else { [3; 32] },
                    if field == 3 { [0; 16] } else { [4; 16] },
                    if field == 4 { 0 } else { 1 },
                    if field == 5 { [0; 48] } else { [5; 48] },
                )
                .is_err(),
                "component {field}"
            );
        }
    }
}
