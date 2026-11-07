// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Prepared targets are durable correlation, not a process freeze or a grant.
//! A usable proof exists only inside the claimed book and current owner callback.

use super::*;

const MAX_PREPARED: usize = 64;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    operation: String,
    pool_revision: String,
    expected_assignment: u64,
    account: Option<String>,
}

impl Body {
    fn from_request(request: &protocol::Request) -> Result<Self, String> {
        let protocol::Command::Prepare {
            pool_revision,
            expected_assignment,
            selection,
        } = &request.command
        else {
            return Err("assignment request is not preparation".into());
        };
        let body = Self {
            operation: request.operation.clone(),
            pool_revision: pool_revision.clone(),
            expected_assignment: *expected_assignment,
            account: match selection {
                protocol::Selection::Auto => None,
                protocol::Selection::Account(id) => Some(id.clone()),
            },
        };
        if !body.valid() {
            return Err("assignment preparation is invalid".into());
        }
        Ok(body)
    }
    fn valid(&self) -> bool {
        hex(&self.operation, 32)
            && hex(&self.pool_revision, 64)
            && self
                .account
                .as_deref()
                .is_none_or(|account| hex(account, 64))
    }
    fn selection(&self) -> protocol::Selection {
        self.account
            .clone()
            .map_or(protocol::Selection::Auto, protocol::Selection::Account)
    }
}

/// Native endpoint provenance from an authenticated producer document. No JWT
/// claim or legacy refresh default can construct this evidence.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuth {
    issuer: String,
    token_endpoint: String,
    client: String,
}

impl OAuth {
    fn from_document(document: &[u8]) -> Result<Self, String> {
        let value: aterm_json::Value =
            aterm_json::from_slice(document).map_err(|_| "assignment target document invalid")?;
        let provenance = value
            .get("tokens")
            .and_then(|v| v.get("oauth_provenance"))
            .ok_or("assignment target has no native OAuth provenance")?;
        let issuer = provenance
            .get("issuer")
            .and_then(aterm_json::Value::as_str)
            .ok_or("assignment target issuer unavailable")?;
        let client = provenance
            .get("client_id")
            .and_then(aterm_json::Value::as_str)
            .ok_or("assignment target client unavailable")?;
        let result = Self {
            issuer: issuer.into(),
            token_endpoint: format!("{}/oauth/token", issuer.trim_end_matches('/')),
            client: client.into(),
        };
        if !result.valid() {
            return Err("assignment target OAuth provenance invalid".into());
        }
        Ok(result)
    }
    fn valid(&self) -> bool {
        if self.issuer.len() > 2048
            || self.client.is_empty()
            || self.client.len() > 256
            || self.client.trim() != self.client
            || self.client.chars().any(char::is_control)
            || self
                .issuer
                .bytes()
                .any(|b| b <= 0x20 || b >= 0x7f || matches!(b, b'\\' | b'?' | b'#'))
            || self.token_endpoint != format!("{}/oauth/token", self.issuer.trim_end_matches('/'))
        {
            return false;
        }
        let Some(issuer) = aterm_http::Uri::parse(&self.issuer) else {
            return false;
        };
        let Some(token) = aterm_http::Uri::parse(&self.token_endpoint) else {
            return false;
        };
        issuer.scheme() == aterm_http::Scheme::Https
            && token.scheme() == issuer.scheme()
            && token.host() == issuer.host()
            && token.port() == issuer.port()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    binding: owner::GrantBinding,
    identity: super::super::codex_renewal::Identity,
    oauth: OAuth,
    credential_floor: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    body: Body,
    source: owner::GrantBinding,
    source_authority: SourceAuthority,
    // None is a durable accepted intent, never a prepared-target proof.
    target: Option<Target>,
}

impl Record {
    fn valid(&self) -> bool {
        self.body.valid()
            && self.source_authority.valid(self.body.expected_assignment)
            && self.source.generation > 0
            && self.source.request.valid()
            && self.source.reference.valid()
            && self.target.as_ref().is_none_or(|target| {
                target.binding.generation > 0
                    && target.binding.request.valid()
                    && target.binding.reference.valid()
                    && hex(&target.identity.account, 64)
                    && hex(&target.identity.user, 64)
                    && target.credential_floor > 0
                    && target.oauth.valid()
                    && self.body.account.as_ref().map_or_else(
                        || self.source.request.label != target.binding.request.label,
                        |id| {
                            *id == super::super::codex_enrollment::account_id(
                                &target.binding.request.label,
                            )
                        },
                    )
            })
    }
}

pub(super) fn records_valid(records: &[Record]) -> bool {
    records.len() <= MAX_PREPARED
        && records.iter().enumerate().all(|(index, record)| {
            record.valid()
                && records[..index]
                    .iter()
                    .all(|old| old.body.operation != record.body.operation)
        })
}

fn admitted<'a>(records: &'a [Record], body: &Body) -> Result<Option<&'a Record>, String> {
    let prior = records
        .iter()
        .find(|record| record.body.operation == body.operation);
    if prior.is_some_and(|record| record.body != *body) {
        return Err("assignment operation body changed".into());
    }
    if prior.is_none() && records.len() >= MAX_PREPARED {
        return Err("assignment preparation capacity unavailable".into());
    }
    Ok(prior)
}

/// Borrowed from a currently claimed native book and authenticated target.
/// This grants no freeze, provider dispatch, materialization or assignment.
pub(in crate::harness) struct PreparedAssignment<'a, 'grant> {
    record: &'a Record,
    source: &'a super::super::codex_source_registry::ReadySourceProof,
    target: &'a owner::AdmittedGrant<'grant>,
}

/// Owned continuation minted only inside the claimed prepared-target callback.
/// It is metadata authority to re-admit this target, never credential bytes or
/// a process-freeze proof. It cannot be cloned or deserialized.
pub(in crate::harness) struct PreparedTarget {
    record: Record,
    caller: Caller,
}

impl PreparedTarget {
    pub(in crate::harness) fn operation(&self) -> &str {
        &self.record.body.operation
    }
    pub(in crate::harness) fn caller(&self) -> &Caller {
        &self.caller
    }
    pub(in crate::harness) fn source(&self) -> &owner::GrantBinding {
        &self.record.source
    }
    pub(in crate::harness) fn matches(&self, other: &Self) -> bool {
        self.caller == other.caller && self.record == other.record
    }
}

impl std::fmt::Debug for PreparedTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedTarget(redacted)")
    }
}

#[cfg(all(test, unix))]
pub(in crate::harness) fn reservation_metadata_fixture(
    caller: Caller,
    source: owner::GrantBinding,
    target: owner::GrantBinding,
) -> PreparedTarget {
    // Explicit synthetic metadata injection for registry lifecycle tests only.
    // It does not pass the prepared callback, authenticate an owner or qualify
    // a live source/process and must never be counted as an admission test.
    PreparedTarget {
        caller,
        record: Record {
            body: Body {
                operation: "1".repeat(32),
                pool_revision: "2".repeat(64),
                expected_assignment: 0,
                account: None,
            },
            source,
            source_authority: SourceAuthority::Startup {
                nonce: "3".repeat(32),
            },
            target: Some(Target {
                binding: target,
                identity: super::super::codex_renewal::Identity::from_native("fixture", "fixture"),
                oauth: OAuth {
                    issuer: "https://auth.example.test".into(),
                    token_endpoint: "https://auth.example.test/oauth/token".into(),
                    client: "fixture".into(),
                },
                credential_floor: 1,
            }),
        },
    }
}

impl<'grant> PreparedAssignment<'_, 'grant> {
    pub(in crate::harness) fn retain_target(&self) -> PreparedTarget {
        PreparedTarget {
            record: self.record.clone(),
            caller: self.source.caller().clone(),
        }
    }
    pub(super) fn operation(&self) -> &str {
        &self.record.body.operation
    }
    pub(super) fn target(&self) -> &owner::AdmittedGrant<'grant> {
        self.target
    }
    pub(super) fn source(&self) -> &super::super::codex_source_registry::ReadySourceProof {
        self.source
    }
}

impl std::fmt::Debug for PreparedAssignment<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedAssignment(redacted)")
    }
}

fn current(
    env: &Env,
    book: &Book,
    source: &super::super::codex_source_registry::ReadySourceProof,
    body: &Body,
) -> bool {
    let (_, _, handle, _) = source.gate_source();
    env.home
        .as_deref()
        .is_some_and(|home| source.gate_source().3.matches_home(home))
        && source.current()
        && book.valid(source.caller())
        && book.current.generation == body.expected_assignment
        && book.current.binding == *handle.binding()
        && handle.assignment_generation() == body.expected_assignment
        && book.current.authority == *handle.source_authority()
        && matches!(book.current.phase, Phase::Startup | Phase::Applied)
}

fn publish(claimed: &mut Claimed, identity: &age::Identity) -> Result<(), String> {
    if !claimed.book.valid(&claimed.book.caller) {
        return Err("assignment preparation book is invalid".into());
    }
    let mac = signature(identity, &claimed.book)?;
    // Serialize by reference: a failed save leaves the exact admitted intent in
    // memory, and every replay retries this same write before minting a proof.
    #[derive(Serialize)]
    struct BorrowedEnvelope<'a> {
        book: &'a Book,
        mac: String,
    }
    let bytes = SecretBytes::new(
        aterm_json::to_vec(&BorrowedEnvelope {
            book: &claimed.book,
            mac,
        })
        .map_err(|_| "assignment preparation encoding unavailable")?,
    );
    claimed.store.save(bytes.as_slice())?;
    claimed.store.unchanged().map_err(str::to_owned)
}

/// Prepare a target for an already authenticated registered source. This does
/// not advertise transport support or grant permission to freeze that source.
/// # Errors
/// Stale source/pool policy, unavailable target custody, altered operation body,
/// unsupported provenance, capacity, or durable publication failure.
pub fn prepare(
    env: &Env,
    source: &super::super::codex_source_registry::ReadySourceProof,
    request: &protocol::Request,
) -> Result<protocol::Status, String> {
    with_prepared(env, source, request, |prepared| {
        if !prepared.source().current() {
            return Err("assignment source changed".into());
        }
        Ok(protocol::Status {
            operation: prepared.operation().to_owned(),
            phase: protocol::Phase::Prepared,
            boundary: None,
            assignment_generation: None,
            credential_generation: None,
            target_ref: Some(super::super::codex_enrollment::account_id(
                &prepared.target().binding().request.label,
            )),
            selection_mode: Some(if prepared.record.body.account.is_some() {
                protocol::SelectionMode::Manual
            } else {
                protocol::SelectionMode::Auto
            }),
            reason: None,
        })
    })
}

pub(in crate::harness) fn with_prepared<R>(
    env: &Env,
    source: &super::super::codex_source_registry::ReadySourceProof,
    request: &protocol::Request,
    use_prepared: impl FnOnce(PreparedAssignment<'_, '_>) -> Result<R, String>,
) -> Result<R, String> {
    platform::require_available()?;
    let body = Body::from_request(request)?;
    let home = env.home.as_deref().ok_or("assignment home unavailable")?;
    if !source.gate_source().3.matches_home(home) {
        return Err("assignment source namespace changed".into());
    }
    let lease = open(env, source.caller())?;
    let identity = platform::machine_identity(home)
        .map_err(|_| "assignment identity unavailable")?
        .ok_or("assignment identity unavailable")?;
    let prior = {
        let mut claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
        if !current(env, &claimed.book, source, &body) {
            return Err("assignment source changed".into());
        }
        claimed.store.unchanged().map_err(str::to_owned)?;
        let prior = admitted(&claimed.book.prepared, &body)?.cloned();
        let retained = match prior {
            Some(prior) => prior,
            None => {
                let intent = Record {
                    body: body.clone(),
                    source: claimed.book.current.binding.clone(),
                    source_authority: claimed.book.current.authority.clone(),
                    target: None,
                };
                claimed.book.prepared.push(intent.clone());
                claimed.book.schema = 3;
                intent
            }
        };
        if retained.source != claimed.book.current.binding
            || retained.source_authority != claimed.book.current.authority
        {
            return Err("assignment intent source changed".into());
        }
        // Admission is durable before credential contention. An unknown/lost
        // reply therefore cannot turn this ID into a different request later.
        publish(&mut claimed, &identity)?;
        retained
    };
    let selection = body.selection();
    let source_binding = prior.source.clone();
    owner::with_selected_target(
        env,
        source.caller(),
        &body.pool_revision,
        &selection,
        &source_binding,
        prior.target.as_ref().map(|target| &target.binding),
        |grant| {
            // Account/owner custody precedes the book, matching native source
            // use. The initial book observation never remains locked while
            // acquiring credential custody.
            let mut claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
            if !current(env, &claimed.book, source, &body) {
                return Err("assignment source changed".into());
            }
            claimed.store.unchanged().map_err(str::to_owned)?;
            let current_prior = admitted(&claimed.book.prepared, &body)?
                .ok_or("assignment intent disappeared")?
                .clone();
            if current_prior.source != prior.source
                || current_prior.source_authority != prior.source_authority
                || prior
                    .target
                    .as_ref()
                    .is_some_and(|old| current_prior.target.as_ref() != Some(old))
            {
                return Err("assignment preparation changed".into());
            }
            let target = Target {
                binding: grant.binding().clone(),
                identity: grant.identity().clone(),
                oauth: OAuth::from_document(grant.document())?,
                credential_floor: grant.revision().generation,
            };
            if let Some(old) = &current_prior.target {
                if old.binding != target.binding
                    || old.identity != target.identity
                    || old.oauth != target.oauth
                    || target.credential_floor < old.credential_floor
                {
                    return Err("prepared assignment target changed".into());
                }
            } else {
                let retained = claimed
                    .book
                    .prepared
                    .iter_mut()
                    .find(|record| record.body.operation == body.operation)
                    .ok_or("assignment intent disappeared")?;
                retained.target = Some(target);
            }
            publish(&mut claimed, &identity)?;
            let retained = claimed
                .book
                .prepared
                .iter()
                .find(|record| record.body.operation == body.operation)
                .ok_or("prepared assignment disappeared")?;
            let result = use_prepared(PreparedAssignment {
                record: retained,
                source,
                target: &grant,
            })?;
            if !current(env, &claimed.book, source, &body) {
                return Err("assignment source changed".into());
            }
            claimed.store.unchanged().map_err(str::to_owned)?;
            Ok(result)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use aterm_secrets::client;

    fn record() -> Record {
        // Serialized data validation only; this never mints a native source or
        // PreparedAssignment, and contains no real credential or caller.
        let binding = |label: &str| {
            aterm_json::json!({
                "request":{"id":"a".repeat(32),"caller":"b".repeat(64),"label":label,"revision":"c".repeat(64)},
                "reference":{"operation":"a".repeat(32),"capability":"d".repeat(64),"issuance_digest":"e".repeat(64),"producer_mac":"f".repeat(64)},
                "generation":1
            })
        };
        aterm_json::from_value(aterm_json::json!({
            "body":{"operation":"1".repeat(32),"pool_revision":"2".repeat(64),"expected_assignment":0,"account":null},
            "source":binding("source"),"source_authority":{"startup":{"nonce":"3".repeat(32)}},
            "target":{"binding":binding("target"),"identity":{"account":"4".repeat(64),"user":"5".repeat(64)},
            "oauth":{"issuer":"https://auth.example.test/realm","token_endpoint":"https://auth.example.test/realm/oauth/token","client":"native"},
            "credential_floor":1}
        })).unwrap()
    }

    #[cfg(unix)]
    fn book(record: &Record) -> Book {
        Book {
            schema: 2,
            caller: Caller {
                sid: "s-fixture".into(),
                pid: 42,
                birth: 7,
            },
            consumer: Consumer {
                pid: 42,
                birth: 7,
                parent: 1,
                group: 42,
                session: 42,
                image: b"/synthetic".to_vec(),
                device: 1,
                inode: 2,
            },
            clock: 0,
            current: Current {
                binding: record.source.clone(),
                generation: 0,
                previous_generation: 0,
                authority: record.source_authority.clone(),
                boundary: None,
                phase: Phase::Startup,
            },
            prepared: Vec::new(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn durable_pending_intent_reopens_with_same_mac_and_rejects_retargeting() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
        let paths = client::Paths::locate_for_home(&home);
        client::machine_identity_as(
            &paths,
            aterm_secrets::Making {
                test_binary: true,
                ..aterm_secrets::Making::default()
            },
        )
        .unwrap();
        let identity = client::read_machine_identity(&paths).unwrap().unwrap();
        let directory = root.path().join("book");
        let (store, _) = super::super::super::pending_native_store::Store::open_owned(
            &directory, &home, MAX_BOOK,
        )
        .unwrap();
        let mut pending = record();
        pending.target = None;
        let mut book = book(&pending);
        book.schema = 3;
        book.prepared.push(pending.clone());
        let mut claimed = Claimed { store, book };
        publish(&mut claimed, &identity).unwrap();
        // Exact replay can settle an uncertain write acknowledgement without
        // creating a second target, provider request or operation identity.
        publish(&mut claimed, &identity).unwrap();
        let original = claimed.book.prepared[0].body.clone();
        let ciphertext = std::fs::read(directory.join("candidate.age")).unwrap();
        claimed.book.prepared[0].body.operation.clear();
        assert!(publish(&mut claimed, &identity).is_err());
        // Validation must fail before changing the actual encrypted payload.
        assert_eq!(
            ciphertext,
            std::fs::read(directory.join("candidate.age")).unwrap()
        );
        claimed.store.unchanged().unwrap();
        claimed.book.prepared[0].body = original;
        drop(claimed);
        let (_store, bytes) =
            super::super::super::pending_native_store::Store::open_owned_existing(
                &directory, &home, MAX_BOOK,
            )
            .unwrap()
            .unwrap();
        let envelope: Envelope = aterm_json::from_slice(bytes.unwrap().as_slice()).unwrap();
        assert_eq!(signature(&identity, &envelope.book).unwrap(), envelope.mac);
        assert!(envelope.book.valid(&envelope.book.caller));
        let mut changed = pending.body;
        changed.account = Some("9".repeat(64));
        assert!(admitted(&envelope.book.prepared, &changed).is_err());
        let mut tampered = envelope.book;
        tampered.prepared[0].body.account = changed.account;
        assert_ne!(signature(&identity, &tampered).unwrap(), envelope.mac);
    }

    #[test]
    fn bounded_intents_replay_exact_body_without_eviction_or_retarget() {
        let original = record();
        assert!(original.valid());
        let mut records = vec![original.clone()];
        assert!(admitted(&records, &original.body).unwrap().is_some());
        let mut changed = original.body.clone();
        changed.pool_revision = "6".repeat(64);
        assert!(admitted(&records, &changed).is_err());
        changed = original.body.clone();
        changed.account = Some("7".repeat(64));
        assert!(admitted(&records, &changed).is_err());
        for index in 1..MAX_PREPARED {
            let mut next = original.clone();
            next.body.operation = format!("{index:032x}");
            records.push(next);
        }
        assert!(records_valid(&records));
        assert!(admitted(&records, &original.body).unwrap().is_some());
        let mut new_body = original.body.clone();
        new_body.operation = "9".repeat(32);
        assert!(admitted(&records, &new_body).is_err());
        records.push(original);
        assert!(!records_valid(&records));
    }

    #[test]
    fn unresolved_intent_keeps_immutable_body_before_any_target_exists() {
        let mut pending = record();
        pending.target = None;
        assert!(pending.valid());
        let records = vec![pending.clone()];
        let encoded = aterm_json::to_vec(&records).unwrap();
        let restored: Vec<Record> = aterm_json::from_slice(&encoded).unwrap();
        assert!(records_valid(&restored));
        assert!(
            admitted(&restored, &pending.body)
                .unwrap()
                .unwrap()
                .target
                .is_none()
        );
        let mut changed = pending.body;
        changed.account = Some("8".repeat(64));
        assert!(admitted(&restored, &changed).is_err());
    }

    #[test]
    fn persisted_target_cannot_substitute_scope_identity_or_manual_selection() {
        let mut value = record();
        value.body.account = Some(super::super::super::codex_enrollment::account_id("target"));
        assert!(value.valid());
        value.body.account = Some("8".repeat(64));
        assert!(!value.valid());
        value = record();
        value.source_authority = SourceAuthority::Assignment {
            operation: "9".repeat(32),
        };
        assert!(!value.valid());
        value = record();
        value.target.as_mut().unwrap().identity.user.clear();
        assert!(!value.valid());
        value = record();
        value.target.as_mut().unwrap().credential_floor = 0;
        assert!(!value.valid());
    }

    #[test]
    fn legacy_book_encoding_stays_exact_and_new_records_require_schema_three() {
        let record = record();
        let caller = Caller {
            sid: "s-fixture".into(),
            pid: 42,
            birth: 7,
        };
        let mut book = Book {
            schema: 2,
            caller: caller.clone(),
            consumer: Consumer {
                pid: 42,
                birth: 7,
                parent: 1,
                group: 42,
                session: 42,
                image: b"/synthetic".to_vec(),
                device: 1,
                inode: 2,
            },
            clock: 0,
            current: Current {
                binding: record.source.clone(),
                generation: 0,
                previous_generation: 0,
                authority: record.source_authority.clone(),
                boundary: None,
                phase: Phase::Startup,
            },
            prepared: Vec::new(),
        };
        assert!(book.valid(&caller));
        let legacy = aterm_json::to_vec(&book).unwrap();
        let object: aterm_json::Value = aterm_json::from_slice(&legacy).unwrap();
        assert!(object.get("prepared").is_none());
        let decoded: Book = aterm_json::from_slice(&legacy).unwrap();
        assert_eq!(legacy, aterm_json::to_vec(&decoded).unwrap());
        book.prepared.push(record);
        assert!(!book.valid(&caller));
        book.schema = 3;
        assert!(book.valid(&caller));
        let encoded = aterm_json::to_vec(&book).unwrap();
        let mut decoded: Book = aterm_json::from_slice(&encoded).unwrap();
        assert!(decoded.valid(&caller));
        decoded.prepared.push(decoded.prepared[0].clone());
        assert!(!decoded.valid(&caller));
        decoded.prepared.clear();
        assert!(!decoded.valid(&caller));
    }

    #[test]
    fn provenance_requires_explicit_native_record_and_exact_https_realm() {
        let document = br#"{"tokens":{"oauth_provenance":{"issuer":"https://auth.example.test/realm","client_id":"native-client"}}}"#;
        let proof = OAuth::from_document(document).unwrap();
        assert_eq!(
            proof.token_endpoint,
            "https://auth.example.test/realm/oauth/token"
        );
        for absent in [
            br#"{"tokens":{}}"#.as_slice(),
            br#"{"tokens":{"access_token":"opaque"},"iss":"https://auth.example.test"}"#,
        ] {
            assert!(OAuth::from_document(absent).is_err());
        }
        for issuer in [
            "http://auth.example.test",
            "https://auth.example.test?q=other",
            "https://auth.example.test/#realm",
        ] {
            let doc = aterm_json::to_vec(&aterm_json::json!({"tokens":{"oauth_provenance":{"issuer":issuer,"client_id":"native"}}})).unwrap();
            assert!(OAuth::from_document(&doc).is_err());
        }
        let mut changed = proof;
        changed.token_endpoint = "https://other.example.test/oauth/token".into();
        assert!(!changed.valid());
    }

    #[test]
    fn request_body_keeps_auto_and_manual_and_retargeting_distinct() {
        let prefix = format!(
            "assignment-prepare schema=1 operation={} pool_revision={} expected_assignment=0 selection=",
            "1".repeat(32),
            "2".repeat(64)
        );
        let auto = protocol::Request::parse(&(prefix.clone() + "auto")).unwrap();
        let body = Body::from_request(&auto).unwrap();
        assert!(body.account.is_none());
        let mut manual = body.clone();
        manual.account = Some("3".repeat(64));
        assert!(manual.valid());
        assert!(manual != body);
        manual.operation = "4".repeat(31);
        assert!(!manual.valid());
    }
}
