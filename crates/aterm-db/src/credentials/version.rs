// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded public version witnesses. A manifest authenticates every part through its
//! hashes; parts also belong to the manifest writer, request and use. Reassembly creates
//! only a claim, never an authenticated current source or evidence of advancement.

use super::*;
use aterm_secrets::client::version::{
    AuthenticatedVersion, MAX_VERSION_BYTES, RefusalClaim, VersionClaim, VersionRelation,
};

pub(super) const PART_KIND: &str = "version-part";
pub(super) const MAX_PARTS: usize = 16;
const PART_BYTES: usize = 33 * 1024;
const MAX_PART_BODY: usize = 48 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Use {
    Refused,
    Published,
}

impl Use {
    const fn word(self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::Published => "published",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Manifest {
    id: Digest,
    bytes: usize,
    parts: Vec<Digest>,
}

impl Manifest {
    pub(super) fn read(value: &Value) -> Result<Self> {
        let body = value
            .as_object()
            .ok_or_else(|| Error::new("credential version manifest is not an object"))?;
        if body.len() != 4
            || body
                .keys()
                .any(|key| !["v", "id", "bytes", "parts"].contains(&key.as_str()))
            || number(body, "v")? != 1
        {
            return Err(Error::new("unsupported credential version manifest"));
        }
        let id = body
            .get("id")
            .and_then(hex_field::<32>)
            .ok_or_else(|| Error::new("credential version manifest has no identity"))?;
        let bytes = usize::try_from(number(body, "bytes")?)
            .map_err(|_| Error::new("credential version length exceeds bounds"))?;
        let parts = body
            .get("parts")
            .and_then(Value::as_array)
            .filter(|parts| !parts.is_empty() && parts.len() <= MAX_PARTS)
            .ok_or_else(|| Error::new("credential version part count exceeds bounds"))?;
        if bytes == 0 || bytes > MAX_VERSION_BYTES || parts.len() != bytes.div_ceil(PART_BYTES) {
            return Err(Error::new("credential version length and parts disagree"));
        }
        let parts = parts
            .iter()
            .map(|part| {
                part.as_str()
                    .ok_or_else(|| Error::new("credential version part hash is malformed"))
                    .and_then(crate::database::decode_hex::<32>)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { id, bytes, parts })
    }

    fn value(&self) -> Value {
        let mut body = Map::new();
        body.insert("v".into(), Value::from(1_u64));
        body.insert("id".into(), Value::from(hex::encode_lower(&self.id)));
        body.insert("bytes".into(), Value::from(self.bytes as u64));
        body.insert(
            "parts".into(),
            Value::Array(
                self.parts
                    .iter()
                    .map(|part| Value::from(hex::encode_lower(part)))
                    .collect(),
            ),
        );
        Value::Object(body)
    }
}

pub(super) struct Prepared {
    pub(super) manifest: Value,
    pub(super) parts: Vec<Record>,
}

pub(super) fn prepare(request: &[u8; 16], usage: Use, claim: &VersionClaim) -> Result<Prepared> {
    let encoded = claim.encode();
    if encoded.is_empty() || encoded.len() > MAX_VERSION_BYTES {
        return Err(Error::new("credential version encoding exceeds bounds"));
    }
    let chunks: Vec<_> = encoded.chunks(PART_BYTES).collect();
    if chunks.len() > MAX_PARTS {
        return Err(Error::new("credential version needs too many parts"));
    }
    let manifest = Manifest {
        id: claim.id(),
        bytes: encoded.len(),
        parts: chunks.iter().map(Sha256::digest).collect(),
    };
    let parts = chunks
        .into_iter()
        .enumerate()
        .map(|(index, bytes)| {
            let mut body = Map::new();
            body.insert("use".into(), Value::from(usage.word()));
            body.insert(
                "version".into(),
                Value::from(hex::encode_lower(&manifest.id)),
            );
            body.insert("index".into(), Value::from(index as u64));
            body.insert(
                "data".into(),
                Value::from(aterm_secrets::b64::encode_padded(bytes)),
            );
            let record = Record::new(PART_KIND, request, body);
            check_part(&record.body)?;
            if record.command()?.encode()?.len() > crate::crypto::MAX_BODY {
                return Err(Error::new(
                    "credential version part exceeds operation bounds",
                ));
            }
            Ok(record)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Prepared {
        manifest: manifest.value(),
        parts,
    })
}

pub(super) fn canonical_part(body: &Map) -> Result<Vec<u8>> {
    let bytes = aterm_json::to_vec(body)
        .map_err(|_| Error::new("credential version part could not be encoded"))?;
    if bytes.len() > MAX_PART_BODY {
        return Err(Error::new("credential version part exceeds bounds"));
    }
    Ok(bytes)
}

pub(super) fn check_part(body: &Map) -> Result<Vec<u8>> {
    Ok(part(body)?.encoded)
}

struct Part {
    encoded: Vec<u8>,
    usage: Use,
    version: Digest,
    index: usize,
    bytes: Vec<u8>,
}

fn part(body: &Map) -> Result<Part> {
    let encoded = canonical_part(body)?;
    if body.len() != 4
        || body
            .keys()
            .any(|key| !["use", "version", "index", "data"].contains(&key.as_str()))
        || !matches!(text(body, "use", 16)?, "refused" | "published")
        || body.get("version").and_then(hex_field::<32>).is_none()
        || number(body, "index")? >= MAX_PARTS as u64
    {
        return Err(Error::new("credential version part schema is malformed"));
    }
    let data = text(body, "data", 4 * PART_BYTES.div_ceil(3))?;
    let bytes = aterm_secrets::b64::decode_padded(data)
        .filter(|bytes| !bytes.is_empty() && bytes.len() <= PART_BYTES)
        .ok_or_else(|| Error::new("credential version part data is malformed"))?;
    Ok(Part {
        encoded,
        usage: if text(body, "use", 16)? == "refused" {
            Use::Refused
        } else {
            Use::Published
        },
        version: body
            .get("version")
            .and_then(hex_field::<32>)
            .ok_or_else(|| Error::new("missing part version"))?,
        index: number(body, "index")? as usize,
        bytes,
    })
}

type PartKey = (Actor, String, Use, Digest, usize, Digest);

/// Decode each eligible part once per projection. The manifest still selects the exact
/// bytes; unrelated, duplicated and malformed parts grant no authority.
pub(super) struct Parts(BTreeMap<PartKey, Vec<u8>>);

impl Parts {
    pub(super) fn read(entries: &[Entry]) -> Self {
        let mut parts = BTreeMap::new();
        for entry in entries
            .iter()
            .filter(|entry| entry.record.kind == PART_KIND)
        {
            if ids(&entry.record.key).is_err() {
                continue;
            }
            let Ok(part) = part(&entry.record.body) else {
                continue;
            };
            parts
                .entry((
                    entry.actor,
                    entry.record.key.clone(),
                    part.usage,
                    part.version,
                    part.index,
                    Sha256::digest(&part.bytes),
                ))
                .or_insert(part.bytes);
        }
        Self(parts)
    }

    /// Missing or foreign parts remain pending. An extra wrong part cannot hide a
    /// later exact part selected by the manifest's hash.
    pub(super) fn resolve(
        &self,
        actor: &Actor,
        request: &str,
        usage: Use,
        manifest: &Manifest,
    ) -> Result<Option<VersionClaim>> {
        let mut encoded = Vec::with_capacity(manifest.bytes);
        for (index, expected) in manifest.parts.iter().enumerate() {
            let Some(part) = self.0.get(&(
                *actor,
                request.to_owned(),
                usage,
                manifest.id,
                index,
                *expected,
            )) else {
                return Ok(None);
            };
            if part.len() != (manifest.bytes - encoded.len()).min(PART_BYTES) {
                return Err(Error::new(
                    "credential version part length disagrees with manifest",
                ));
            }
            encoded.extend_from_slice(part);
        }
        let claim = VersionClaim::decode(&encoded).map_err(crate::table::failure)?;
        if claim.id() != manifest.id || claim.encode() != encoded {
            return Err(Error::new(
                "credential version witness differs from its manifest",
            ));
        }
        Ok(Some(claim))
    }
}

pub(super) fn exceeds(current: &AuthenticatedVersion, floor: &RefusalClaim) -> bool {
    match floor {
        RefusalClaim::LegacySequence(seq) => current
            .envelope_sequence()
            .is_some_and(|current| current > *seq),
        RefusalClaim::Authenticated(claim) => current.relation(claim) == VersionRelation::Advanced,
    }
}

pub(super) fn accepts(
    current: &AuthenticatedVersion,
    candidate: &VersionClaim,
    floor: Option<&RefusalClaim>,
) -> bool {
    // The caller verified the custodian's attestation over this exact manifest.
    // A log-only recipient need not also cache its envelope, but must prove the
    // claimed causal prefix using its actual verified graph.
    if current.relation_between(candidate, candidate) != VersionRelation::Same {
        return false;
    }
    match floor {
        None => true,
        Some(RefusalClaim::LegacySequence(seq)) => {
            candidate.envelope().is_some_and(|v| v.seq > *seq)
        }
        Some(RefusalClaim::Authenticated(claim)) => {
            current.relation_between(candidate, claim) == VersionRelation::Advanced
        }
    }
}
