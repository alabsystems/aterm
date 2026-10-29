// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CREDENTIAL REQUESTS ON THE LAB'S TABLE (docs/DESIGN-credential-requests-2026-10-27.md §5,
//! §6.2, §7, §9): `credentials/requests/1` carries one Mac's request for a credential it lacks
//! and every answer to it, and never a secret. Each record is one whole value signed by its
//! writer; the projection reads every eligible record, never the cells, because a cell's causal
//! replacement would let one writer's record hide another's. A record whose writer or evidence
//! does not check is inert, and the projection says why.
//!
//! Every member runs [`pass`]: it flushes this Mac's need ledger into the table, takes in what
//! the lab decided about its own requests, and sweeps the others' (an `Offer` where its hold
//! probe finds the want, and one `Sweep` naming what it checked). A Mac that can sign with the
//! vault's current key also runs [`custodian_pass`]: it copies a held entry into the
//! requester's own host section, matched by the recipient the requester's Commons binding
//! names, and attests each decision it writes.

use crate::{
    Error, Result,
    catalog::Descriptor,
    database::{Change, Command},
    documents::log::Log,
    table::{
        Scope, Session,
        store::{Receipt, Store},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::{Map, Value};
use aterm_secrets::{
    client::{self, Paths, custodian::Copied},
    commons::bootstrap::Bootstrap,
    need::{
        self, Absorb, Decision, Need, Policy, Purpose, Reason, Retirement, State, Want, custody,
    },
};
use std::collections::{BTreeMap, BTreeSet};

pub mod cli;

/// A record body's canonical bytes are at most this long (§5).
pub const MAX_RECORD: usize = 4096;
/// The most request ids one `Sweep` names.
pub const MAX_COVERS: usize = 64;
/// The longest `Retired` note (§5).
pub const MAX_NOTE: usize = 160;
/// The longest `Retired` evidence.
pub const MAX_EVIDENCE: usize = 96;
const COMMAND_DOMAIN: &[u8] = b"aterm/credentials/command/1\0";
const REQUEST_KEY_DOMAIN: &[u8] = b"aterm/credentials/request-key/1\0";
const MS: u64 = 1000;

/// The record kinds this build reads. A custodian's `Policy` record is step 5's: this build
/// holds one inert, so the compiled policy stays in force until a build that reads it.
pub const KINDS: [&str; 8] = [
    "request",
    "offer",
    "sweep",
    "still",
    "published",
    "needs-person",
    "fulfilled",
    "retired",
];

fn descriptor(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "credentials", "requests", 1)
}

fn ids(text: &str) -> Result<[u8; 16]> {
    crate::database::decode_hex::<16>(text)
}

/// The table key of `requester`'s request whose need-ledger id is `nonce`: the request id every
/// other record names. It is derived from the requester, so no other writer can claim it: a
/// request whose key is not its signer's derivation is inert, whatever order the records
/// arrived in.
#[must_use]
pub fn request_key(requester: &Actor, nonce: &[u8; 16]) -> [u8; 16] {
    let mut input = REQUEST_KEY_DOMAIN.to_vec();
    input.extend_from_slice(requester);
    input.extend_from_slice(nonce);
    let digest = Sha256::digest(&input);
    let mut key = [0; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

/// One record as written: its kind, its key (a request id, or a sweep's pass id) and its body.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// One of [`KINDS`].
    pub kind: String,
    /// 32 lowercase hex characters.
    pub key: String,
    /// The closed body.
    pub body: Map,
}

impl Record {
    fn new(kind: &str, key: &[u8; 16], body: Map) -> Self {
        Self {
            kind: kind.to_owned(),
            key: hex::encode_lower(key),
            body,
        }
    }

    /// The command that carries it. Its retry identity is derived from the record, so a
    /// second write of the same record returns the first one's receipt rather than a twin.
    ///
    /// # Errors
    /// The body is over [`MAX_RECORD`] bytes or cannot be encoded.
    pub fn command(&self) -> Result<Command> {
        let bytes = canonical(&self.body)?;
        let mut input = COMMAND_DOMAIN.to_vec();
        input.extend_from_slice(self.kind.as_bytes());
        input.push(0);
        input.extend_from_slice(self.key.as_bytes());
        input.push(0);
        input.extend_from_slice(&bytes);
        let digest = Sha256::digest(&input);
        let mut request = [0; 16];
        request.copy_from_slice(&digest[..16]);
        Ok(Command::Write {
            request,
            changes: vec![Change {
                collection: self.kind.clone(),
                key: self.key.clone(),
                value: Some(Value::Object(self.body.clone())),
            }],
        })
    }

    /// The canonical body a custodian attests, without its `att` field.
    fn attested_bytes(&self) -> Result<Vec<u8>> {
        let mut body = self.body.clone();
        body.remove("att");
        canonical(&body)
    }

    /// Attach a custodian's attestation (§5), which covers the record's kind and key with its
    /// body, so it never counts for another request.
    fn attest(
        mut self,
        sign: &dyn Fn(custody::Attested<'_>) -> aterm_secrets::Result<[u8; 64]>,
    ) -> Result<Self> {
        self.body.remove("att");
        let key = ids(&self.key)?;
        let body = self.attested_bytes()?;
        let signature = sign(custody::Attested {
            kind: &self.kind,
            key: &key,
            body: &body,
        })
        .map_err(crate::table::failure)?;
        self.body
            .insert("att".into(), Value::from(hex::encode_lower(&signature)));
        Ok(self)
    }

    /// Whether its attestation verifies against `signer` on `network`, for this record's own
    /// kind and key.
    fn attested(&self, signer: Option<&[u8; 32]>, network: &[u8; 16]) -> bool {
        let (Some(signer), Some(att)) = (signer, self.body.get("att").and_then(Value::as_str))
        else {
            return false;
        };
        let (Ok(signature), Ok(key)) = (crate::database::decode_hex::<64>(att), ids(&self.key))
        else {
            return false;
        };
        self.attested_bytes().is_ok_and(|body| {
            let record = custody::Attested {
                kind: &self.kind,
                key: &key,
                body: &body,
            };
            custody::attested(signer, network, record, &signature)
        })
    }
}

fn canonical(body: &Map) -> Result<Vec<u8>> {
    let bytes = aterm_json::to_vec(body)
        .map_err(|_| Error::new("credential record could not be encoded"))?;
    if bytes.len() > MAX_RECORD {
        return Err(Error::new("credential record body exceeds 4 KiB"));
    }
    Ok(bytes)
}

fn text<'a>(body: &'a Map, field: &str, cap: usize) -> Result<&'a str> {
    body.get(field)
        .and_then(Value::as_str)
        .filter(|text| text.chars().count() <= cap && !text.chars().any(char::is_control))
        .ok_or_else(|| Error::new("credential record field is missing or malformed"))
}

fn number(body: &Map, field: &str) -> Result<u64> {
    body.get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::new("credential record number is missing"))
}

fn optional<T>(body: &Map, field: &str, read: impl Fn(&Value) -> Option<T>) -> Result<Option<T>> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => read(value)
            .map(Some)
            .ok_or_else(|| Error::new("credential record field is malformed")),
    }
}

fn hex_field<const N: usize>(value: &Value) -> Option<[u8; N]> {
    value
        .as_str()
        .and_then(|text| crate::database::decode_hex::<N>(text).ok())
}

/// Check a record's closed schema: the fields its kind allows, each well formed.
fn check(record: &Record) -> Result<()> {
    let allowed: &[&str] = match record.kind.as_str() {
        "request" => &["nonce", "want", "purpose", "refused", "supersedes"],
        "offer" => &[],
        "sweep" => &["covers"],
        "still" => &["at"],
        "published" => &["vault", "seq", "recipient", "contributor", "att"],
        "needs-person" => &["template", "att"],
        "fulfilled" => &["seq"],
        "retired" => &["reason", "evidence", "note", "att"],
        _ => return Err(Error::new("unsupported credential record kind")),
    };
    if record
        .body
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err(Error::new(
            "unknown credential record field; values belong in the vault",
        ));
    }
    ids(&record.key)?;
    if record.key != record.key.to_ascii_lowercase() {
        return Err(Error::new("credential record key is not lowercase hex"));
    }
    let body = &record.body;
    let att = || optional(body, "att", hex_field::<64>);
    match record.kind.as_str() {
        "request" => {
            body.get("nonce")
                .and_then(hex_field::<16>)
                .ok_or_else(|| Error::new("a request names its ledger nonce"))?;
            Want::parse(text(body, "want", 160)?).map_err(|_| Error::new("unknown want"))?;
            Purpose::parse(text(body, "purpose", 64)?)
                .ok_or_else(|| Error::new("unknown purpose"))?;
            optional(body, "refused", Value::as_u64)?;
            optional(body, "supersedes", hex_field::<16>)?;
        }
        "sweep" => {
            let covers = body
                .get("covers")
                .and_then(Value::as_array)
                .filter(|covers| !covers.is_empty() && covers.len() <= MAX_COVERS)
                .ok_or_else(|| Error::new("a sweep names 1 to 64 requests"))?;
            let mut seen = BTreeSet::new();
            for id in covers {
                let id = hex_field::<16>(id).ok_or_else(|| Error::new("sweep id malformed"))?;
                if !seen.insert(id) {
                    return Err(Error::new("a sweep names a request twice"));
                }
            }
        }
        "still" => {
            number(body, "at")?;
        }
        "published" => {
            text(body, "vault", 64)?;
            number(body, "seq")?;
            body.get("recipient")
                .and_then(hex_field::<32>)
                .ok_or_else(|| Error::new("published names no recipient digest"))?;
            optional(body, "contributor", hex_field::<48>)?;
            att()?.ok_or_else(|| Error::new("published carries no attestation"))?;
        }
        "needs-person" => {
            if !matches!(text(body, "template", 32)?, "vault-lacks" | "holder-away") {
                return Err(Error::new("unknown needs-person template"));
            }
            att()?.ok_or_else(|| Error::new("needs-person carries no attestation"))?;
        }
        "fulfilled" => {
            number(body, "seq")?;
        }
        "retired" => {
            Reason::parse(text(body, "reason", 32)?)
                .ok_or_else(|| Error::new("unknown retirement reason"))?;
            text(body, "evidence", MAX_EVIDENCE)?;
            text(body, "note", MAX_NOTE)?;
            att()?;
        }
        _ => {}
    }
    canonical(body)?;
    Ok(())
}

fn record_of(command: &Command) -> Result<Record> {
    let Command::Write { changes, .. } = command else {
        return Err(Error::new("credential records are whole-value writes"));
    };
    let [change] = changes.as_slice() else {
        return Err(Error::new(
            "a credential command carries exactly one record",
        ));
    };
    let Some(Value::Object(body)) = &change.value else {
        return Err(Error::new("a credential record is an object"));
    };
    let record = Record {
        kind: change.collection.clone(),
        key: change.key.clone(),
        body: body.clone(),
    };
    check(&record)?;
    if record.command()? != *command {
        return Err(Error::new(
            "credential command's retry identity is not its record's",
        ));
    }
    Ok(record)
}

fn encode(command: &Command) -> Result<Vec<u8>> {
    let payload = command.encode()?;
    if decode(&payload)? != *command {
        return Err(Error::new("credential encoding would change its meaning"));
    }
    Ok(payload)
}

fn decode(payload: &[u8]) -> Result<Command> {
    if payload.len() > MAX_RECORD * 2 {
        return Err(Error::new("credential record exceeds payload limit"));
    }
    let command = Command::decode(payload)?;
    if command.encode()? != payload {
        return Err(Error::new("credential record encoding is not canonical"));
    }
    record_of(&command)?;
    Ok(command)
}

/// One eligible record and who wrote it, as the projection reads it.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The signing writer incarnation (never a body field).
    pub actor: Actor,
    /// That writer's sequence number.
    pub sequence: u64,
    /// The operation digest.
    pub id: Digest,
    /// When this replica first held it (Unix ms).
    pub received_ms: u64,
    /// The record.
    pub record: Record,
}

/// What every member reads the records against.
#[derive(Debug, Clone)]
pub struct Context {
    /// The policy in force (§7.2).
    pub policy: Policy,
    /// The vault signing key this Mac authenticated its applied version by.
    pub signer: Option<[u8; 32]>,
    /// The Commons network id, bound into every attestation.
    pub network: [u8; 16],
    /// Every current member's actor.
    pub members: BTreeSet<Actor>,
    /// Unix ms now.
    pub now_ms: u64,
}

/// One request and every checked answer to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// The request id.
    pub id: [u8; 16],
    /// What is wanted.
    pub want: Want,
    /// Whose need it is.
    pub purpose: Purpose,
    /// The vault seq whose value the consumer refused.
    pub refused: Option<u64>,
    /// The request it replaces.
    pub supersedes: Option<[u8; 16]>,
    /// The requester: the request record's signer.
    pub requester: Actor,
    /// The request's operation digest.
    pub operation: Digest,
    /// The requester's sequence number for it.
    pub sequence: u64,
    /// When the request reached this replica (the want window's start at a custodian).
    pub arrived_ms: u64,
    /// When this replica last held a record from the requester on it.
    pub heard_ms: u64,
    /// Holders that offered it, in record-id order, at most one each.
    pub offers: Vec<Actor>,
    /// Members whose sweep covered it.
    pub swept: BTreeSet<Actor>,
    /// The custodian's publication: vault name and seq.
    pub published: Option<(String, u64)>,
    /// The custodian handed it to a person.
    pub needs_person: bool,
    /// The requester applied it at this seq.
    pub fulfilled: Option<u64>,
    /// How it was retired, and whether a custodian attested it.
    pub retired: Option<(Retirement, bool)>,
}

impl Thread {
    /// Fulfilled or retired.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.fulfilled.is_some() || self.retired.is_some()
    }

    /// Every current member but the requester has swept it (the window's early end, §6.2).
    #[must_use]
    pub fn swept_by_all(&self, members: &BTreeSet<Actor>) -> bool {
        members
            .iter()
            .filter(|member| **member != self.requester)
            .all(|member| self.swept.contains(member))
    }

    fn window_ended(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.arrived_ms) >= need::WANT_WINDOW_SECS * MS
    }
}

/// The projection: every request whose record checked, and every record that did not.
#[derive(Debug, Clone, Default)]
pub struct Book {
    /// Requests by id.
    pub threads: BTreeMap<[u8; 16], Thread>,
    /// Inert records and why.
    pub inert: Vec<(Digest, String)>,
}

impl Book {
    /// Project `entries` under `context` (§6.2's checks).
    #[must_use]
    pub fn project(mut entries: Vec<Entry>, context: &Context) -> Self {
        entries.sort_by_key(|entry| entry.id);
        let mut book = Self::default();
        let mut inert = |id: Digest, why: &str| book.inert.push((id, why.to_owned()));
        let mut threads: BTreeMap<[u8; 16], Thread> = BTreeMap::new();
        for entry in entries
            .iter()
            .filter(|entry| entry.record.kind == "request")
        {
            let body = &entry.record.body;
            let (Ok(id), Ok(want), Some(purpose)) = (
                ids(&entry.record.key),
                Want::parse(body.get("want").and_then(Value::as_str).unwrap_or_default()),
                Purpose::parse(
                    body.get("purpose")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
            ) else {
                inert(entry.id, "request does not parse");
                continue;
            };
            if !want.posted() {
                inert(entry.id, "the catalog never posts this want");
                continue;
            }
            let derived = body
                .get("nonce")
                .and_then(hex_field::<16>)
                .map(|nonce| request_key(&entry.actor, &nonce));
            if derived != Some(id) {
                inert(entry.id, "a request id is not its signer's own derivation");
                continue;
            }
            if threads.contains_key(&id) {
                inert(entry.id, "a request id is its first requester's own");
                continue;
            }
            threads.insert(
                id,
                Thread {
                    id,
                    want,
                    purpose,
                    refused: body.get("refused").and_then(Value::as_u64),
                    supersedes: body.get("supersedes").and_then(hex_field::<16>),
                    requester: entry.actor,
                    operation: entry.id,
                    sequence: entry.sequence,
                    arrived_ms: entry.received_ms,
                    heard_ms: entry.received_ms,
                    offers: Vec::new(),
                    swept: BTreeSet::new(),
                    published: None,
                    needs_person: false,
                    fulfilled: None,
                    retired: None,
                },
            );
        }
        // What members say first, so a decision is checked against every sweep and Still.
        for entry in entries.iter() {
            let record = &entry.record;
            match record.kind.as_str() {
                "sweep" => {
                    for id in record
                        .body
                        .get("covers")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(hex_field::<16>)
                    {
                        if let Some(thread) = threads.get_mut(&id)
                            && thread.requester != entry.actor
                        {
                            thread.swept.insert(entry.actor);
                        }
                    }
                }
                "offer" | "still" | "fulfilled" => {
                    let Some(thread) = ids(&record.key).ok().and_then(|id| threads.get_mut(&id))
                    else {
                        inert(entry.id, "names no request");
                        continue;
                    };
                    let by_requester = thread.requester == entry.actor;
                    match record.kind.as_str() {
                        "offer" if by_requester => {
                            inert(entry.id, "a requester offers nothing to itself")
                        }
                        "offer" if thread.offers.contains(&entry.actor) => {
                            inert(entry.id, "a second offer from one holder");
                        }
                        "offer" => thread.offers.push(entry.actor),
                        _ if !by_requester => inert(entry.id, "only the requester writes this"),
                        "fulfilled" if thread.fulfilled.is_some() => {
                            inert(entry.id, "already fulfilled")
                        }
                        "fulfilled" => {
                            thread.fulfilled = record.body.get("seq").and_then(Value::as_u64);
                        }
                        _ => {}
                    }
                    if by_requester {
                        thread.heard_ms = thread.heard_ms.max(entry.received_ms);
                    }
                }
                _ => {}
            }
        }
        for entry in entries.iter() {
            let record = &entry.record;
            if !matches!(
                record.kind.as_str(),
                "published" | "needs-person" | "retired"
            ) {
                continue;
            }
            let Some(thread) = ids(&record.key).ok().and_then(|id| threads.get_mut(&id)) else {
                inert(entry.id, "names no request");
                continue;
            };
            let attested = record.attested(context.signer.as_ref(), &context.network);
            let by_requester = thread.requester == entry.actor;
            if thread.ended() {
                inert(entry.id, "the request has ended");
                continue;
            }
            match record.kind.as_str() {
                "published" | "needs-person" if !attested => {
                    inert(
                        entry.id,
                        "a custodian's record whose attestation does not verify",
                    );
                }
                "published" if thread.published.is_none() => {
                    let vault = record.body.get("vault").and_then(Value::as_str);
                    let seq = record.body.get("seq").and_then(Value::as_u64);
                    thread.published = vault.zip(seq).map(|(vault, seq)| (vault.to_owned(), seq));
                }
                "needs-person" => thread.needs_person = true,
                "retired" => {
                    let body = &record.body;
                    let Some(reason) = body
                        .get("reason")
                        .and_then(Value::as_str)
                        .and_then(Reason::parse)
                    else {
                        continue;
                    };
                    let evidence = body
                        .get("evidence")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let refused = match reason {
                        Reason::NotNeeded | Reason::Superseded => {
                            (!by_requester).then_some("only the requester retires its own need so")
                        }
                        Reason::Declined => (!by_requester && !attested)
                            .then_some("a decline from a Mac that is neither the requester nor a custodian"),
                        Reason::NeverStored | Reason::NeverCopied => {
                            thread.want.catalog_retirement().is_none().then_some(
                                "the catalog gives this reason to no posted want",
                            )
                        }
                        Reason::NotAuthorized => {
                            let cited = format!("policy {}", context.policy.generation);
                            (context.policy.allows(&thread.want) || evidence != cited)
                                .then_some("the cited policy gives this want to the requester")
                        }
                        Reason::NotHeld => (!attested
                            || !(thread.swept_by_all(&context.members)
                                || thread.window_ended(context.now_ms)))
                        .then_some("not-held needs a custodian and every member's sweep or the window's end"),
                        Reason::Lapsed => (!attested
                            || context.now_ms.saturating_sub(thread.heard_ms)
                                < need::LAPSE_SECS * MS)
                            .then_some("lapsed needs a custodian and 30 days without a word from the requester"),
                        Reason::RequesterGone => (!attested)
                            .then_some("requester-gone from the vault needs a custodian"),
                    };
                    if let Some(why) = refused {
                        inert(entry.id, why);
                        continue;
                    }
                    let note = body.get("note").and_then(Value::as_str).unwrap_or_default();
                    thread.retired = Some((
                        Retirement {
                            reason,
                            evidence: evidence.to_owned(),
                            note: note.to_owned(),
                            generation: (reason == Reason::NotAuthorized)
                                .then_some(context.policy.generation),
                        },
                        attested,
                    ));
                }
                _ => inert(entry.id, "a second publication"),
            }
        }
        // At most one live request per requester, want and purpose (the need ledger's own
        // identity, so two consumers of one want each hear their answer), and 16 live per
        // requester.
        let mut order: Vec<&Thread> = threads.values().collect();
        order.sort_by_key(|thread| (thread.requester, thread.sequence));
        let mut live: BTreeMap<Actor, Vec<(String, Purpose)>> = BTreeMap::new();
        let mut dropped = Vec::new();
        for thread in order {
            let mine = live.entry(thread.requester).or_default();
            if thread.ended() {
                continue;
            }
            let identity = (thread.want.key(), thread.purpose);
            if mine.contains(&identity) {
                dropped.push((
                    thread.id,
                    thread.operation,
                    "a second live request for one want and purpose",
                ));
            } else if mine.len() >= need::MAX_OPEN {
                dropped.push((
                    thread.id,
                    thread.operation,
                    "past 16 live requests from one Mac",
                ));
            } else {
                mine.push(identity);
            }
        }
        for (id, operation, why) in dropped {
            threads.remove(&id);
            book.inert.push((operation, why.to_owned()));
        }
        book.threads = threads;
        book
    }

    fn read(log: &Log, context: &Context) -> Self {
        let entries = log
            .eligible()
            .into_iter()
            .filter_map(|(operation, command)| {
                record_of(&command).ok().map(|record| Entry {
                    actor: operation.header.actor,
                    sequence: operation.header.sequence,
                    id: operation.header.id,
                    received_ms: operation.receipt.received_ms,
                    record,
                })
            })
            .collect();
        Self::project(entries, context)
    }
}

fn now_ms() -> u64 {
    aterm_time::unix_secs().unwrap_or(0).saturating_mul(MS)
}

/// This Mac's facts, read before the table's session opens, since they take the vault's
/// own locks.
fn facts(paths: &Paths, now_ms: u64) -> Result<need::Facts> {
    need::facts(paths, now_ms / MS).map_err(crate::table::failure)
}

fn context(facts: &need::Facts, session: &Session<'_>, scope: Scope, now_ms: u64) -> Context {
    Context {
        policy: facts.policy,
        signer: facts.sight.as_ref().map(|sight| sight.signer),
        network: scope.trust().network,
        members: session
            .members()
            .iter()
            .map(aterm_secrets::commons::binding::Binding::actor)
            .collect(),
        now_ms,
    }
}

fn with_store<T>(
    paths: &Paths,
    action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
) -> Result<T> {
    crate::documents::application::with_store(paths, true, descriptor, action)
}

fn commit(session: &Session<'_>, store: &mut Store, log: &Log, record: &Record) -> Result<Receipt> {
    let command = record.command()?;
    if let Some(id) = log.retry(session.actor(), &command)? {
        return store
            .receipt(session, &id)?
            .ok_or_else(|| Error::new("credential record receipt is unavailable"));
    }
    store.commit_catalog(session, &encode(&command)?)
}

fn request_record(me: &Actor, need: &Need) -> Record {
    let mut body = Map::new();
    body.insert("nonce".into(), Value::from(hex::encode_lower(&need.id)));
    body.insert("want".into(), Value::from(need.want.key()));
    body.insert("purpose".into(), Value::from(need.purpose.id()));
    if let Some(seq) = need.refused {
        body.insert("refused".into(), Value::from(seq));
    }
    Record::new("request", &request_key(me, &need.id), body)
}

fn retired_record(id: &[u8; 16], reason: Reason, evidence: &str, note: &str) -> Record {
    let mut body = Map::new();
    body.insert("reason".into(), Value::from(reason.word()));
    body.insert(
        "evidence".into(),
        Value::from(evidence.chars().take(MAX_EVIDENCE).collect::<String>()),
    );
    body.insert(
        "note".into(),
        Value::from(note.chars().take(MAX_NOTE).collect::<String>()),
    );
    Record::new("retired", id, body)
}

/// What a member's pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pass {
    /// This Mac is a current Commons Writer.
    pub writer: bool,
    /// Requests this pass posted.
    pub posted: usize,
    /// Records this pass wrote about this Mac's own requests (Still, Fulfilled, Retired).
    pub answered: usize,
    /// Decisions this pass took into the ledger.
    pub decided: usize,
    /// Offers this pass wrote.
    pub offered: usize,
    /// Requests this pass's sweep covered.
    pub swept: usize,
    /// Every request the table holds, this Mac's and the others'.
    pub threads: Vec<Thread>,
    /// Inert records and why.
    pub inert: Vec<(Digest, String)>,
}

fn decision_for(thread: &Thread, need: &Need) -> Option<Decision> {
    if let Some((retirement, _)) = &thread.retired {
        return Some(Decision::Retired(retirement.clone()));
    }
    if let Some((_, seq)) = &thread.published {
        return Some(Decision::Published { seq: *seq });
    }
    if thread.needs_person && need.state != State::AwaitingPerson {
        return Some(Decision::NeedsPerson);
    }
    (!thread.offers.is_empty() && need.state == State::Open).then_some(Decision::Offered)
}

/// The holder's probe (§7.3): the one compiled path the want resolves to under this Mac's
/// home holds a file. It stats the path and never reads it.
fn holds(paths: &Paths, want: &Want) -> bool {
    want.hold_path(&paths.home).is_some_and(|path| {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
    })
}

/// One member's pass over `credentials/requests/1` (§9.1, §9.2): post this Mac's needs, take
/// in what the lab decided about them, and sweep everyone else's.
///
/// # Errors
/// The ledger or the vault state cannot be read, or this Mac has no Commons to open.
pub fn pass(paths: &Paths) -> Result<Pass> {
    pass_at(paths, now_ms())
}

/// [`pass`] at `now_ms`.
///
/// # Errors
/// As [`pass`].
pub fn pass_at(paths: &Paths, now_ms: u64) -> Result<Pass> {
    let needs = need::list(paths).map_err(crate::table::failure)?;
    let facts = facts(paths, now_ms)?;
    let mut moves = Vec::new();
    let mut report = with_store(paths, |session, store, scope| {
        let context = context(&facts, session, scope, now_ms);
        let log = Log::read_with(session, store, scope, decode)?;
        let writer = session.writer().is_ok() && log.writable(session.actor()).is_ok();
        let book = Book::read(&log, &context);
        let me = session.actor();
        let mut report = Pass {
            writer,
            ..Pass::default()
        };
        // A request past the 16 live per Mac would be inert and wait for good: it stays in the
        // ledger until one of this Mac's live requests ends.
        let mut open = book
            .threads
            .values()
            .filter(|thread| thread.requester == me && !thread.ended())
            .count();
        for need in &needs {
            let key = request_key(&me, &need.id);
            let thread = book
                .threads
                .get(&key)
                .filter(|thread| thread.requester == me);
            let allowed = context.policy.allows(&need.want);
            let room = open < need::MAX_OPEN;
            match (need.state, thread) {
                (State::Local, None) if writer && allowed && room => {
                    let receipt = commit(session, store, &log, &request_record(&me, need))?;
                    moves.push((need.id, Absorb::Flushed(receipt.id.to_vec())));
                    report.posted += 1;
                    open += 1;
                }
                (State::AwaitingPerson, None) if writer && allowed && room && !need.posted => {
                    let receipt = commit(session, store, &log, &request_record(&me, need))?;
                    moves.push((need.id, Absorb::Posted(receipt.id.to_vec())));
                    report.posted += 1;
                    open += 1;
                }
                (State::Local, Some(thread)) => {
                    moves.push((need.id, Absorb::Flushed(thread.operation.to_vec())));
                }
                (State::AwaitingPerson, Some(thread)) if !need.posted => {
                    moves.push((need.id, Absorb::Posted(thread.operation.to_vec())));
                }
                (State::Fulfilled, Some(thread)) if writer && !thread.ended() => {
                    let mut body = Map::new();
                    body.insert("seq".into(), Value::from(need.seq.unwrap_or(0)));
                    commit(session, store, &log, &Record::new("fulfilled", &key, body))?;
                    report.answered += 1;
                }
                (State::Retired, Some(thread)) if writer && !thread.ended() => {
                    if let Some(retired) = need.retired.as_ref().filter(|retired| {
                        matches!(
                            retired.reason,
                            Reason::NotNeeded | Reason::Declined | Reason::Superseded
                        )
                    }) {
                        let record =
                            retired_record(&key, retired.reason, &retired.evidence, &retired.note);
                        commit(session, store, &log, &record)?;
                        report.answered += 1;
                    }
                }
                (
                    State::Open | State::Offered | State::Published | State::AwaitingPerson,
                    Some(thread),
                ) => {
                    if let Some(decision) = decision_for(thread, need) {
                        moves.push((need.id, Absorb::Decided(decision)));
                    } else if writer
                        && now_ms.saturating_sub(thread.heard_ms) >= need::STILL_SECS * MS
                    {
                        let mut body = Map::new();
                        body.insert("at".into(), Value::from(now_ms / MS));
                        commit(session, store, &log, &Record::new("still", &key, body))?;
                        report.answered += 1;
                    }
                }
                _ => {}
            }
        }
        // A live request of this Mac's that no need in the ledger stands for (its need ended
        // and was raised again under a new id before a pass wrote the end) would hold its want
        // and purpose against its successor for good: the requester supersedes it.
        if writer {
            let standing: BTreeSet<[u8; 16]> = needs
                .iter()
                .map(|need| request_key(&me, &need.id))
                .collect();
            for thread in book.threads.values().filter(|thread| {
                thread.requester == me && !thread.ended() && !standing.contains(&thread.id)
            }) {
                let record = retired_record(
                    &thread.id,
                    Reason::Superseded,
                    "no need in the requester's ledger stands for it",
                    "",
                );
                commit(session, store, &log, &record)?;
                report.answered += 1;
            }
        }
        if writer {
            let mut covers = Vec::new();
            for thread in book.threads.values() {
                let unanswered = thread.requester != me
                    && !thread.ended()
                    && thread.published.is_none()
                    && !thread.swept.contains(&me)
                    && !thread.offers.contains(&me);
                if !unanswered || !context.policy.allows(&thread.want) {
                    continue;
                }
                if holds(paths, &thread.want) {
                    commit(
                        session,
                        store,
                        &log,
                        &Record::new("offer", &thread.id, Map::new()),
                    )?;
                    report.offered += 1;
                }
                covers.push(Value::from(hex::encode_lower(&thread.id)));
                if covers.len() == MAX_COVERS {
                    break;
                }
            }
            if !covers.is_empty() {
                report.swept = covers.len();
                let pass_id = crate::crypto::random::<16>()?;
                let mut body = Map::new();
                body.insert("covers".into(), Value::Array(covers));
                commit(session, store, &log, &Record::new("sweep", &pass_id, body))?;
            }
        }
        report.threads = book.threads.into_values().collect();
        report.inert = book.inert;
        Ok(report)
    })?;
    report.decided = need::absorb(paths, &moves)
        .map_err(crate::table::failure)?
        .iter()
        .filter(|need| {
            moves
                .iter()
                .any(|(id, step)| *id == need.id && matches!(step, Absorb::Decided(_)))
        })
        .count();
    Ok(report)
}

/// What a custodian's pass decided (§9.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Custodian {
    /// Requests published, with the vault seq that carries each.
    pub published: Vec<([u8; 16], u64)>,
    /// Requests handed to a person.
    pub asked: Vec<[u8; 16]>,
    /// Requests retired, and why.
    pub retired: Vec<([u8; 16], Reason)>,
    /// Requests still waiting, and on whom.
    pub waiting: Vec<([u8; 16], String)>,
}

enum Plan {
    Copy {
        id: [u8; 16],
        recipient: aterm_secrets::age::Recipient,
        want: Want,
        offered: bool,
        decided: bool,
        asked: bool,
    },
    Retire([u8; 16], Reason, String),
}

/// The custodian's pass (§8.1, §9.3), on a Mac that can sign with the vault's current key:
/// `None` elsewhere. For each live request the policy decides first, before anything reads
/// whether the vault holds the entry. A held entry is copied into the requester's own host
/// section (matched by the recipient its Commons binding names) and published; one the vault
/// lacks waits for the sweeps or the want window, then goes to a person when one can make it
/// and is retired `not-held` when none can. Every decision is attested.
///
/// # Errors
/// The vault cannot be published, or the table cannot be read or written.
pub fn custodian_pass(paths: &Paths) -> Result<Option<Custodian>> {
    custodian_pass_at(paths, now_ms())
}

/// [`custodian_pass`] at `now_ms`.
///
/// # Errors
/// As [`custodian_pass`].
pub fn custodian_pass_at(paths: &Paths, now_ms: u64) -> Result<Option<Custodian>> {
    let Ok(mut admin) = client::admin_cached(paths) else {
        return Ok(None);
    };
    let facts = facts(paths, now_ms)?;
    let mut report = Custodian::default();
    let (plans, network) = with_store(paths, |session, store, scope| {
        let mut context = context(&facts, session, scope, now_ms);
        context.signer = Some(admin.custodian_signer().map_err(crate::table::failure)?);
        let log = Log::read_with(session, store, scope, decode)?;
        let book = Book::read(&log, &context);
        let mut plans = Vec::new();
        for thread in book.threads.values() {
            if thread.ended() || thread.published.is_some() {
                continue;
            }
            if !context.policy.allows(&thread.want) {
                plans.push(Plan::Retire(
                    thread.id,
                    Reason::NotAuthorized,
                    format!("policy {}", context.policy.generation),
                ));
                continue;
            }
            if now_ms.saturating_sub(thread.heard_ms) >= need::LAPSE_SECS * MS {
                plans.push(Plan::Retire(
                    thread.id,
                    Reason::Lapsed,
                    "30 days unheard".into(),
                ));
                continue;
            }
            let Ok(binding) = session.peer(thread.requester) else {
                report
                    .waiting
                    .push((thread.id, "the requester is not a current member".into()));
                continue;
            };
            let decided = thread.swept_by_all(&context.members) || thread.window_ended(now_ms);
            plans.push(Plan::Copy {
                id: thread.id,
                recipient: binding.recipient(),
                want: thread.want.clone(),
                offered: !thread.offers.is_empty(),
                decided,
                asked: thread.needs_person,
            });
        }
        Ok((plans, scope.trust().network))
    })?;
    let mut records = Vec::new();
    let mut copied = Vec::new();
    let mut changed = false;
    for plan in plans {
        match plan {
            Plan::Retire(id, reason, evidence) => {
                records.push(retired_record(&id, reason, &evidence, ""));
                report.retired.push((id, reason));
            }
            Plan::Copy {
                id,
                recipient,
                want,
                offered,
                decided,
                asked,
            } => {
                // The first entry answers the want; the others (a mailbox's TOTP) go with it
                // where the vault holds them, and never without it.
                let mut first = None;
                for entry in want.entries() {
                    let (outcome, _) = admin
                        .copy_to_requester(&recipient, &entry, want.if_absent())
                        .map_err(crate::table::failure)?;
                    changed |= outcome == Copied::Copied;
                    if first.is_none() {
                        first = Some(outcome);
                        if !matches!(outcome, Copied::Copied | Copied::AlreadyThere) {
                            break;
                        }
                    }
                }
                match first.unwrap_or(Copied::NotHeld) {
                    Copied::Copied | Copied::AlreadyThere => copied.push((id, recipient)),
                    Copied::NoHost => report.waiting.push((
                        id,
                        "the vault names no machine with this Mac's recipient: a pending join, or an admin's aterm secrets approve, adds it".into(),
                    )),
                    Copied::AudienceLimit => report.waiting.push((
                        id,
                        "the vault holds 62 host sections already: an admin merges two".into(),
                    )),
                    Copied::NotHeld if offered => report.waiting.push((
                        id,
                        "a holder offered it: the handoff from a holder is the next step's".into(),
                    )),
                    Copied::NotHeld if asked => report.waiting.push((
                        id,
                        "awaits a person: aterm secrets set on this Mac, or on the requester, gives it".into(),
                    )),
                    Copied::NotHeld if !decided => report
                        .waiting
                        .push((id, "waits on the members' sweeps or the want window".into())),
                    Copied::NotHeld if want.person_sourced() => {
                        let mut body = Map::new();
                        body.insert("template".into(), Value::from("vault-lacks"));
                        records.push(Record::new("needs-person", &id, body));
                        report.asked.push(id);
                    }
                    Copied::NotHeld => {
                        records.push(retired_record(
                            &id,
                            Reason::NotHeld,
                            "every member swept; the vault lacks it",
                            "",
                        ));
                        report.retired.push((id, Reason::NotHeld));
                    }
                }
            }
        }
    }
    if !copied.is_empty() {
        let seq = if !changed {
            admin.seq()
        } else {
            let doc = admin.doc.clone();
            client::admin_publish(paths, &admin, &doc)
                .map_err(crate::table::failure)?
                .seq
        };
        for (id, recipient) in copied {
            let mut body = Map::new();
            body.insert("vault".into(), Value::from(admin.config.name.clone()));
            body.insert("seq".into(), Value::from(seq));
            body.insert(
                "recipient".into(),
                Value::from(hex::encode_lower(&Sha256::digest(recipient.0))),
            );
            records.push(Record::new("published", &id, body));
            report.published.push((id, seq));
        }
    }
    if records.is_empty() {
        return Ok(Some(report));
    }
    let sign = |record: custody::Attested<'_>| admin.attest_credential_record(&network, record);
    let records = records
        .into_iter()
        .map(|record| record.attest(&sign))
        .collect::<Result<Vec<_>>>()?;
    with_store(paths, |session, store, scope| {
        let log = Log::read_with(session, store, scope, decode)?;
        session.writer()?;
        for record in &records {
            commit(session, store, &log, record)?;
        }
        Ok(())
    })?;
    Ok(Some(report))
}

#[cfg(test)]
mod tests;
