// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[cfg(unix)]
use aterm_secrets::client;

pub(super) fn record() -> Record {
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
pub(super) fn book(record: &Record) -> Book {
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
        acknowledged: Vec::new(),
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
    let (store, _) =
        super::super::super::pending_native_store::Store::open_owned(&directory, &home, MAX_BOOK)
            .unwrap();
    let mut pending = record();
    pending.target = None;
    let mut book = book(&pending);
    book.schema = 3;
    book.prepared.push(pending.clone());
    let mut claimed = Claimed {
        store,
        book,
        pending_install: None,
    };
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
    let (_store, bytes) = super::super::super::pending_native_store::Store::open_owned_existing(
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
        acknowledged: Vec::new(),
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
