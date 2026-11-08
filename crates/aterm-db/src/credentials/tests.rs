// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::{Lab, repair_commons};
use aterm_secrets::{
    commons::{bootstrap::writer_here, journal::Journal, roster::Role},
    need::{Purpose, Reason, State, Want},
    text::Audience,
};

const SEED: [u8; 32] = [5; 32];
const NETWORK: [u8; 16] = [9; 16];
const A: Actor = [1; 48];
const B: Actor = [2; 48];
const C: Actor = [3; 48];
const DAY_MS: u64 = 86_400_000;

#[test]
fn mailbox_component_contributions_accept_only_the_requested_field() {
    use aterm_secrets::{
        SecretBytes,
        mailbox_component::{Component, Part},
        need::transfer::Values,
    };
    let component = Component::for_mailbox(
        "https://mail.example",
        "owner@example",
        Part::SecondPassword,
    )
    .unwrap();
    let want = Want::MailboxComponent(component);
    let valid = Values(vec![(
        component.entry(),
        SecretBytes::from(&b"fixture-second"[..]),
    )]);
    assert!(value::validate(&want, &valid).is_ok());
    let wrong =
        Component::for_mailbox("https://mail.example", "owner@example", Part::Password).unwrap();
    let wrong = Values(vec![(
        wrong.entry(),
        SecretBytes::from(&b"fixture-login"[..]),
    )]);
    assert!(value::validate(&want, &wrong).is_err());
    let extra = Values(vec![
        (component.entry(), SecretBytes::from(&b"fixture-second"[..])),
        ("other".into(), SecretBytes::from(&b"fixture-extra"[..])),
    ]);
    assert!(value::validate(&want, &extra).is_err());
}

#[test]
fn peeking_without_local_state_does_not_create_a_ledger_or_table() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("unconfigured");
    let paths = Paths::for_home(&home);
    for words in [vec!["peek", "--json"], vec!["peek", "--all", "--json"]] {
        let words: Vec<_> = words.into_iter().map(str::to_owned).collect();
        let result = cli::run(&paths, &words).unwrap();
        let result: Value = aterm_json::from_str(&result).unwrap();
        assert!(result["needs"].as_array().unwrap().is_empty());
        if result.get("table").is_some() {
            assert_eq!(result["table"]["available"].as_bool(), Some(false));
        }
        assert!(!home.exists(), "inspection must not initialize this Mac");
    }
}

#[test]
fn peeking_reads_remote_requests_beside_writers_without_posting_or_sweeping() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let requester = &lab.paths[1];
    let raised = need::raise(
        requester,
        Want::Mailbox("m19-2".into()),
        Purpose::HarnessSignIn,
    )
    .unwrap();
    assert_eq!(raised.state, State::Local);
    let ledger = requester.dir.join(need::LEDGER);
    let before = std::fs::read(&ledger).unwrap();
    let words = ["peek", "--all", "--json"].map(str::to_owned);
    let read =
        |paths| -> Value { aterm_json::from_str(&cli::run(paths, &words).unwrap()).unwrap() };
    assert_eq!(read(requester)["table"]["available"].as_bool(), Some(false));
    assert_eq!(std::fs::read(&ledger).unwrap(), before);
    assert_eq!(need::peek(requester).unwrap()[0].state, State::Local);

    // Setup deliberately posts and repairs. The inspection that follows must do neither.
    pass(requester).unwrap();
    repair_commons(&lab, 3);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
    let catalog_scope = directory.catalog_scope();
    let catalog = Store::open(
        &session,
        &directory.path(catalog_scope).unwrap(),
        catalog_scope,
    )
    .unwrap();
    let scope = descriptor(&bootstrap).unwrap().scope;
    let table_path = directory.path(scope).unwrap();
    let table = Store::open(&session, &table_path, scope).unwrap();
    let before_table = std::fs::read(table_path.join("table.astream")).unwrap();
    let before_ledger = std::fs::read(&ledger).unwrap();
    let result = read(&lab.paths[0]);
    assert_eq!(
        result["table"]["observer"],
        Value::String(hex::encode_lower(&session.actor()))
    );
    let requests = result["table"]["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["want"].as_str(), Some("mailbox:m19-2"));
    assert_eq!(requests[0]["own"].as_bool(), Some(false));
    assert_eq!(
        requests[0]["requester"],
        Value::String(hex::encode_lower(&observe(requester).unwrap().observer))
    );
    assert_eq!(requests[0]["delivery_origin"], Value::Null);
    assert_eq!(requests[0]["offers"].as_u64(), Some(0));
    assert_eq!(requests[0]["swept_current"].as_u64(), Some(0));
    assert_eq!(requests[0]["required_sweeps"].as_u64(), Some(1));
    assert_eq!(requests[0]["sweeps_complete"].as_bool(), Some(false));
    assert_eq!(requests[0]["needs_person"].as_bool(), Some(false));
    assert_eq!(requests[0]["published_seq"], Value::Null);
    assert_eq!(
        std::fs::read(table_path.join("table.astream")).unwrap(),
        before_table
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), before_ledger);
    assert_eq!(catalog.scope(), catalog_scope);
    assert_eq!(table.scope(), scope);
    drop((table, catalog, session));

    // Signed but unreadable records must remain visible in the coverage count.
    with_store(&lab.paths[0], |session, store, _| {
        let invalid = Command::Write {
            request: [8; 16],
            changes: vec![Change {
                collection: "unknown".into(),
                key: "unknown".into(),
                value: Some(Value::Null),
            }],
        };
        store.commit_catalog(session, &invalid.encode()?)
    })
    .unwrap();
    let result = read(&lab.paths[0]);
    assert_eq!(result["table"]["retained_operations"].as_u64(), Some(2));
    assert_eq!(result["table"]["interpreted_operations"].as_u64(), Some(1));
    assert_eq!(result["table"]["requests"].as_array().unwrap().len(), 1);
}

fn signer() -> [u8; 32] {
    aterm_secrets::envelope::signer_public(&SEED).unwrap()
}

fn context(now_ms: u64) -> Context {
    Context {
        policy: need::Policy::compiled(true),
        signer: Some(signer()),
        version: None,
        network: NETWORK,
        members: [A, B, C].into_iter().collect(),
        scope: None,
        writers: BTreeMap::new(),
        now_ms,
    }
}

struct Feed {
    entries: Vec<Entry>,
    next: u8,
}

impl Feed {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            next: 0,
        }
    }

    fn put(&mut self, actor: Actor, record: Record) -> Digest {
        self.next += 1;
        let id = [self.next; 32];
        self.put_as(actor, record, id)
    }

    /// Put `record` under the operation digest `id`, which orders the projection's reads.
    fn put_as(&mut self, actor: Actor, record: Record, id: Digest) -> Digest {
        self.next += 1;
        self.entries.push(Entry {
            actor,
            sequence: u64::from(self.next),
            id,
            received_ms: 0,
            record,
        });
        id
    }

    fn request(&mut self, actor: Actor, nonce: u8, want: &str) -> [u8; 16] {
        self.request_for(actor, nonce, want, "by-hand")
    }

    /// `actor`'s request for `want` by `purpose`, from its ledger id `[nonce; 16]`: its key.
    fn request_for(&mut self, actor: Actor, nonce: u8, want: &str, purpose: &str) -> [u8; 16] {
        let (key, record) = request_body(&actor, nonce, want, purpose);
        self.put(actor, record);
        key
    }

    fn book(&self, now_ms: u64) -> Book {
        Book::project(self.entries.clone(), &context(now_ms))
    }
}

fn request_body(actor: &Actor, nonce: u8, want: &str, purpose: &str) -> ([u8; 16], Record) {
    let nonce = [nonce; 16];
    let mut body = Map::new();
    body.insert("nonce".into(), Value::from(hex::encode_lower(&nonce)));
    body.insert("want".into(), Value::from(want));
    body.insert("purpose".into(), Value::from(purpose));
    if want == "notary" {
        body.insert("entry".into(), Value::from("release-notary"));
    }
    let key = request_key(actor, &nonce);
    (key, Record::new("request", &key, body))
}

#[test]
fn a_notary_request_binds_the_release_entry_across_mixed_version_peers() {
    let (_, record) = request_body(&A, 1, "notary", "release.notary");
    check(&record).unwrap();
    let mut legacy = record.clone();
    legacy.body.remove("entry");
    check(&legacy).unwrap();
    let mut wrong = record.clone();
    wrong.body.insert("entry".into(), Value::from("notary"));
    check(&wrong).unwrap();

    let mut feed = Feed::new();
    feed.put(A, legacy);
    feed.put(A, wrong);
    let book = feed.book(0);
    assert!(book.threads.is_empty());
    assert_eq!(book.inert.len(), 2);
}

#[test]
fn a_posted_legacy_notary_request_is_reissued_with_its_entry_binding() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let paths = &lab.paths[1];
    need::test_alab_policy(paths);
    let raised = need::raise(paths, Want::Notary, Purpose::ReleaseNotary).unwrap();
    let receipt = with_store(paths, |session, store, _scope| {
        let mut legacy = request_record(&session.actor(), &raised)?.record;
        legacy.body.remove("entry");
        let payload = legacy.command()?.encode()?;
        store.commit_catalog(session, &payload)
    })
    .unwrap();
    need::absorb(paths, &[(raised.id, Absorb::Flushed(receipt.id.to_vec()))]).unwrap();
    assert_eq!(need::peek(paths).unwrap()[0].state, State::Open);
    let old_need = need::peek(paths).unwrap()[0].clone();
    let posted = pass_at(paths, 1_000_000_000).unwrap();
    assert_eq!(posted.posted, 1);
    let next = pass_at(paths, 1_000_000_000).unwrap();
    assert_eq!(next.posted, 0);
    assert_eq!(next.threads.len(), 1);
    assert_eq!(next.threads[0].want, Want::Notary);
    let replacement = need::peek(paths).unwrap()[0].clone();
    assert_ne!(replacement.id, raised.id);

    let facts = facts(paths, 1_000_000_000).unwrap();
    with_store(paths, |session, store, scope| {
        let log = Log::read_with(session, store, scope, decode)?;
        let ctx = super::context(&facts, session, scope, 1_000_000_000)?;
        let current = Book::read(&log, &ctx);
        let old_key = request_key(&session.actor(), &raised.id);
        assert!(!current.threads.contains_key(&old_key));
        let mut old_view = Vec::new();
        let mut terminal_seq = None;
        let mut replacement_seq = None;
        for (operation, command) in log.eligible() {
            let mut record = record_of(command)?;
            if record.kind == "request" && record.body.contains_key("entry") {
                replacement_seq = Some(operation.header.sequence);
                continue; // 0.146's closed schema rejects this request.
            }
            if record.kind == "retired" && record.key == hex::encode_lower(&old_key) {
                terminal_seq = Some(operation.header.sequence);
            }
            if record.kind == "request" {
                // Use an unchanged copyable want to exercise the old reader's terminal
                // reduction without teaching the current reader the legacy notary meaning.
                record
                    .body
                    .insert("want".into(), Value::from("site-deployer-key"));
            }
            old_view.push(Entry {
                actor: operation.header.actor,
                sequence: operation.header.sequence,
                id: operation.header.id,
                received_ms: operation.receipt.received_ms,
                record,
            });
        }
        assert!(terminal_seq.unwrap() < replacement_seq.unwrap());
        let old = Book::project(old_view, &ctx);
        assert_eq!(reason(&old, &old_key), Some(Reason::Superseded));
        assert!(old.threads.values().all(Thread::ended));
        Ok(())
    })
    .unwrap();

    // A crash before ledger adoption retries the same successor and creates no third request,
    // without retiring the recovered successor before the ledger adopts its id.
    need::with_ledger(paths, |rows, _| {
        rows[0] = old_need;
        Ok(())
    })
    .unwrap();
    let recovered = pass_at(paths, 1_000_000_000).unwrap();
    assert_eq!(recovered.posted, 0);
    assert_eq!(recovered.answered, 0);
    assert_eq!(need::peek(paths).unwrap()[0].id, replacement.id);
    let after_recovery = pass_at(paths, 1_000_000_000).unwrap();
    assert_eq!(after_recovery.threads.len(), 1);
    assert!(!after_recovery.threads[0].ended());
    assert_eq!(need::peek(paths).unwrap()[0].state, State::Open);
}

fn attested(record: Record) -> Record {
    record
        .attest(&|record| custody::attest(&SEED, &NETWORK, record))
        .unwrap()
}

fn retired(id: &[u8; 16], reason: Reason, evidence: &str) -> Record {
    retired_record(id, reason, evidence, "")
}

fn reason(book: &Book, id: &[u8; 16]) -> Option<Reason> {
    book.threads[id].retired.as_ref().map(|(r, _)| r.reason)
}

#[test]
fn a_decline_counts_only_from_the_requester_or_a_custodian() {
    let mut feed = Feed::new();
    let id = feed.request(A, 1, "notary");
    feed.put(B, retired(&id, Reason::Declined, "the person"));
    assert_eq!(
        reason(&feed.book(0), &id),
        None,
        "another member's decline is inert"
    );
    let mut by_custodian = Feed::new();
    let id2 = by_custodian.request(A, 1, "notary");
    by_custodian.put(C, attested(retired(&id2, Reason::Declined, "the person")));
    assert_eq!(reason(&by_custodian.book(0), &id2), Some(Reason::Declined));
    feed.put(A, retired(&id, Reason::Declined, "the person"));
    assert_eq!(reason(&feed.book(0), &id), Some(Reason::Declined));
}

#[test]
fn not_held_needs_an_attestation_and_every_sweep_or_the_windows_end() {
    let mut feed = Feed::new();
    let id = feed.request(A, 1, "notary");
    let decision = attested(retired(&id, Reason::NotHeld, "every member swept"));
    feed.put(C, decision.clone());
    assert_eq!(
        reason(&feed.book(0), &id),
        None,
        "unswept and inside the window"
    );
    let mut sweep = Map::new();
    sweep.insert(
        "covers".into(),
        Value::Array(vec![Value::from(hex::encode_lower(&id))]),
    );
    feed.put(B, Record::new("sweep", &[8; 16], sweep.clone()));
    assert_eq!(reason(&feed.book(0), &id), None, "C has not swept");
    feed.put(C, Record::new("sweep", &[9; 16], sweep));
    assert_eq!(reason(&feed.book(0), &id), Some(Reason::NotHeld));
    let mut late = Feed::new();
    let id = late.request(A, 1, "notary");
    late.put(C, decision);
    assert_eq!(
        reason(&late.book(need::WANT_WINDOW_SECS * 1000), &id),
        Some(Reason::NotHeld),
        "the window's end decides for the silent members"
    );
    let mut forged = Feed::new();
    let id = forged.request(A, 1, "notary");
    forged.put(C, retired(&id, Reason::NotHeld, "every member swept"));
    assert_eq!(
        reason(&forged.book(need::WANT_WINDOW_SECS * 1000), &id),
        None,
        "an unattested not-held is inert"
    );
}

#[test]
fn a_publication_needs_the_vault_signers_attestation_on_this_network() {
    let publication = |id: &[u8; 16]| {
        let mut body = Map::new();
        body.insert("vault".into(), Value::from("alab"));
        body.insert("seq".into(), Value::from(7_u64));
        body.insert("recipient".into(), Value::from(hex::encode_lower(&[4; 32])));
        Record::new("published", id, body)
    };
    let mut feed = Feed::new();
    let id = feed.request(A, 1, "notary");
    feed.put(
        B,
        publication(&id)
            .attest(&|record| custody::attest(&[6; 32], &NETWORK, record))
            .unwrap(),
    );
    assert_eq!(
        feed.book(0).threads[&id].published,
        None,
        "another key's attestation"
    );
    feed.put(
        B,
        publication(&id)
            .attest(&|record| custody::attest(&SEED, &[1; 16], record))
            .unwrap(),
    );
    assert_eq!(
        feed.book(0).threads[&id].published,
        None,
        "another network's attestation"
    );
    let mut tampered = attested(publication(&id));
    tampered.body.insert("seq".into(), Value::from(8_u64));
    feed.put(B, tampered);
    assert_eq!(
        feed.book(0).threads[&id].published,
        None,
        "a body changed after signing"
    );
    feed.put(B, attested(publication(&id)));
    assert_eq!(
        feed.book(0).threads[&id].published,
        Some(("alab".into(), RefusalClaim::LegacySequence(7)))
    );

    let mut refused = Feed::new();
    let (id, mut request) = request_body(&A, 2, "notary", "release.notary");
    request.body.insert("refused".into(), Value::from(7_u64));
    refused.put(A, request);
    for seq in [6_u64, 7] {
        let mut stale = publication(&id);
        stale.body.insert("seq".into(), Value::from(seq));
        refused.put(B, attested(stale));
        assert!(refused.book(0).threads[&id].published.is_none());
    }
    let mut newer = publication(&id);
    newer.body.insert("seq".into(), Value::from(8_u64));
    refused.put(B, attested(newer));
    assert_eq!(
        refused.book(0).threads[&id].published,
        Some(("alab".into(), RefusalClaim::LegacySequence(8))),
        "stale signed records cannot block a later valid publication"
    );

    // A typed request cannot shed its floor while parts are arriving, and an
    // incomplete or stale signed decision cannot occupy the publication slot.
    let lab = Lab::with_foundation(&[Role::Writer]);
    let paths = &lab.paths[0];
    let admin = client::admin_open(paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Hosts(vec!["m1".into()]),
        "release-notary",
        b"old-version",
    )
    .unwrap();
    client::admin_publish(paths, &admin, &doc).unwrap();
    client::seed_vault_genesis(paths, &lab._home.path().join("genesis.v2")).unwrap();
    let admin = client::admin_cached(paths).unwrap();
    client::admin_publish(paths, &admin, &admin.doc).unwrap();
    let before = facts(paths, 0).unwrap().sight.unwrap().version.unwrap();
    let legacy_floor = before.envelope_sequence().unwrap();
    let floor = before.claim().clone();
    assert_eq!(floor.envelope().unwrap().seq, legacy_floor);
    assert!(floor.log().is_some());
    let admin = client::admin_cached(paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Hosts(vec!["m1".into()]),
        "release-notary",
        b"new-version",
    )
    .unwrap();
    let produced = client::admin_publish(paths, &admin, &doc).unwrap();
    let admin = client::admin_cached(paths).unwrap();
    let actual = facts(paths, 0).unwrap().sight.unwrap().version.unwrap();
    assert_eq!(
        actual.relation(&floor),
        client::version::VersionRelation::Advanced
    );
    let candidate = produced.version().clone();
    assert!(candidate.envelope().is_some() && candidate.log().is_some());
    let mut context = context(0);
    context.signer = Some(admin.custodian_signer().unwrap());
    context.version = Some(actual.clone());
    let sign = |record: custody::Attested<'_>| admin.attest_credential_record(&NETWORK, record);
    let mut feed = Feed::new();
    let (id, mut request) = request_body(&A, 3, "notary", "release.notary");
    let refused = version::prepare(&id, version::Use::Refused, &floor).unwrap();
    request.body.insert("v".into(), Value::from(2_u64));
    request
        .body
        .insert("refused_version".into(), refused.manifest);
    feed.put(A, request);
    let project = |feed: &Feed| Book::project(feed.entries.clone(), &context);
    assert!(project(&feed).threads[&id].version_pending);
    for part in &refused.parts {
        feed.put(B, part.clone());
    }
    assert!(project(&feed).threads[&id].version_pending);
    for part in refused.parts {
        feed.put(A, part);
    }
    assert_eq!(
        project(&feed).threads[&id].refused,
        Some(RefusalClaim::Authenticated(floor.clone()))
    );
    assert!(!project(&feed).threads[&id].version_pending);
    let recipient = aterm_secrets::age::Identity::from_secret_bytes([4; 32]).recipient();
    let (stale, _) = super::publication(&id, &floor, None, &recipient, None, true).unwrap();
    for part in stale.parts {
        feed.put(B, part);
    }
    feed.put(B, stale.record.attest(&sign).unwrap());
    assert!(project(&feed).threads[&id].published.is_none());
    let (fresh, _) = super::publication(&id, &candidate, None, &recipient, None, true).unwrap();
    feed.put(B, fresh.record.attest(&sign).unwrap());
    assert!(project(&feed).threads[&id].published.is_none());
    for part in &fresh.parts {
        feed.put(C, part.clone());
    }
    assert!(project(&feed).threads[&id].published.is_none());
    for part in &fresh.parts {
        feed.put(B, part.clone());
    }
    assert_eq!(
        project(&feed).threads[&id].published,
        Some((
            candidate.vault().to_owned(),
            RefusalClaim::Authenticated(candidate.clone())
        ))
    );
    for part in fresh.parts {
        feed.put(B, part);
    }
    assert!(project(&feed).threads[&id].published.is_some());

    // An untrusted protocol floor may name only the accepted log prefix. Adding
    // its envelope to an attested candidate does not advance that prefix, as
    // judged by the current authenticated view of both envelope and log.
    let same_floor = client::version::VersionClaim::new(
        actual.claim().vault().to_owned(),
        *actual.claim().signer(),
        None,
        actual.claim().log().cloned(),
    )
    .unwrap();
    assert!(same_floor.envelope().is_none() && same_floor.log().is_some());
    assert_eq!(
        actual.relation_between(&candidate, &same_floor),
        client::version::VersionRelation::Same
    );
    let mut same = Feed::new();
    let (id, mut request) = request_body(&A, 4, "notary", "release.notary");
    let floor_parts = version::prepare(&id, version::Use::Refused, &same_floor).unwrap();
    request.body.insert("v".into(), Value::from(2_u64));
    request
        .body
        .insert("refused_version".into(), floor_parts.manifest);
    for part in floor_parts.parts {
        same.put(A, part);
    }
    same.put(A, request);
    let (publication, _) =
        super::publication(&id, &candidate, None, &recipient, None, true).unwrap();
    for part in publication.parts {
        same.put(B, part);
    }
    same.put(B, publication.record.attest(&sign).unwrap());
    assert!(project(&same).threads[&id].published.is_none());

    // The same accepted view reports its genuine envelope sequence for an
    // older scalar request, without changing typed H ordering.
    let sequence = actual.envelope_sequence().unwrap();
    assert!(sequence > legacy_floor);
    assert!(version::exceeds(
        &actual,
        &RefusalClaim::LegacySequence(legacy_floor)
    ));
    assert!(!version::exceeds(
        &actual,
        &RefusalClaim::LegacySequence(sequence)
    ));
    let mut legacy = Feed::new();
    let (id, mut request) = request_body(&A, 5, "notary", "release.notary");
    request
        .body
        .insert("refused".into(), Value::from(legacy_floor));
    legacy.put(A, request);
    let (publication, reported) =
        super::publication(&id, actual.claim(), Some(sequence), &recipient, None, false).unwrap();
    assert!(publication.parts.is_empty());
    assert_eq!(reported, RefusalClaim::LegacySequence(sequence));
    legacy.put(B, publication.record.attest(&sign).unwrap());
    assert_eq!(
        project(&legacy).threads[&id].published,
        Some((
            actual.claim().vault().to_owned(),
            RefusalClaim::LegacySequence(sequence)
        ))
    );
}

#[test]
fn one_live_request_per_want_and_the_requesters_own_records_only() {
    let mut feed = Feed::new();
    let first = feed.request(A, 1, "notary");
    let second = feed.request(A, 2, "mailbox:m29");
    feed.request(A, 3, "paper-master");
    // B claims A's request id, read first (the lowest digest), with its own nonce and with
    // A's: neither derives that id from B, so neither is B's.
    let (_, mut claim) = request_body(&B, 7, "notary", "by-hand");
    claim.key = hex::encode_lower(&second);
    feed.put_as(B, claim, [0; 32]);
    let (_, mut copied) = request_body(&A, 2, "mailbox:m29", "by-hand");
    copied.key = hex::encode_lower(&second);
    feed.put_as(B, copied, [0; 32]);
    let book = feed.book(0);
    assert!(book.threads.contains_key(&first));
    assert_eq!(
        book.threads[&second].requester, A,
        "an id is the requester's own, whatever order the records arrived in"
    );
    assert!(
        !book.threads.values().any(|t| t.requester == B),
        "B's claims on A's id are inert"
    );
    let mut dup = Feed::new();
    dup.request(A, 1, "notary");
    dup.request(A, 2, "notary");
    assert_eq!(
        dup.book(0).threads.len(),
        1,
        "a second live request for one want and purpose is inert"
    );
    dup.request_for(A, 3, "notary", "release.notary");
    assert_eq!(
        dup.book(0).threads.len(),
        2,
        "another consumer of the same want has its own request"
    );
    assert!(
        !feed
            .book(0)
            .threads
            .values()
            .any(|t| t.want == Want::PaperMaster),
        "the catalog never posts a paper master"
    );
    let mut own = Feed::new();
    let id = own.request(A, 1, "notary");
    own.put(A, Record::new("offer", &id, Map::new()));
    own.put(B, retired(&id, Reason::NotNeeded, "its check"));
    let mut seq = Map::new();
    seq.insert("seq".into(), Value::from(3_u64));
    own.put(B, Record::new("fulfilled", &id, seq));
    let book = own.book(0);
    assert!(book.threads[&id].offers.is_empty() && book.threads[&id].fulfilled.is_none());
    assert_eq!(reason(&book, &id), None);
}

#[test]
fn not_authorized_must_cite_a_policy_that_refuses_and_lapse_needs_thirty_quiet_days() {
    let mut feed = Feed::new();
    let id = feed.request(A, 1, "notary");
    feed.put(B, retired(&id, Reason::NotAuthorized, "policy 0"));
    assert_eq!(
        reason(&feed.book(0), &id),
        None,
        "the compiled ALab policy gives it"
    );
    let mut refusing = context(0);
    refusing.policy = need::Policy::compiled(false);
    assert_eq!(
        Book::project(feed.entries.clone(), &refusing).threads[&id]
            .retired
            .as_ref()
            .map(|(r, _)| r.reason),
        Some(Reason::NotAuthorized)
    );
    let mut quiet = Feed::new();
    let id = quiet.request(A, 1, "notary");
    quiet.put(C, attested(retired(&id, Reason::Lapsed, "30 days unheard")));
    assert_eq!(reason(&quiet.book(29 * DAY_MS), &id), None);
    assert_eq!(reason(&quiet.book(30 * DAY_MS), &id), Some(Reason::Lapsed));
}

#[test]
fn the_schema_is_closed_and_a_retry_identity_is_its_records() {
    let (key, record) = request_body(&A, 1, "notary", "by-hand");
    let body = record.body.clone();
    let command = record.command().unwrap();
    assert_eq!(decode(&command.encode().unwrap()).unwrap(), command);
    let mut extra = body.clone();
    extra.insert("value".into(), Value::from("a secret"));
    let smuggled = Record::new("request", &key, extra).command().unwrap();
    assert!(
        decode(&smuggled.encode().unwrap()).is_err(),
        "no field carries a value"
    );
    let Command::Write { changes, .. } = command else {
        unreachable!()
    };
    let renamed = Command::Write {
        request: [0; 16],
        changes,
    };
    assert!(decode(&renamed.encode().unwrap()).is_err());
    assert!(
        decode(
            &Record::new("policy", &[1; 16], Map::new())
                .command()
                .unwrap()
                .encode()
                .unwrap()
        )
        .is_err(),
        "a policy record is step 5's"
    );
    // A window decision names machines, each once, at most 16, and nothing else.
    let named = |hosts: Vec<String>| {
        let mut body = Map::new();
        body.insert("template".into(), Value::from("vault-lacks"));
        body.insert(
            "unanswered".into(),
            Value::Array(hosts.into_iter().map(Value::from).collect()),
        );
        body.insert("others".into(), Value::from(2_u64));
        let record = attested(Record::new("needs-person", &key, body));
        decode(&record.command().unwrap().encode().unwrap()).is_ok()
    };
    let hosts = |n: usize| (0..n).map(|n| format!("m{n}")).collect::<Vec<_>>();
    assert!(named(hosts(need::MAX_UNANSWERED)));
    assert!(!named(hosts(need::MAX_UNANSWERED + 1)));
    assert!(!named(Vec::new()));
    assert!(!named(vec!["m3".into(), "m3".into()]));
    assert!(!named(vec!["m3; rm".into()]), "a host is a machine name");

    use aterm_secrets::client::version::{EnvelopeVersion, LogVersion, VersionClaim};
    let head = |n: usize| {
        let mut id = [0; 32];
        id[24..].copy_from_slice(&(n as u64 + 1).to_be_bytes());
        id
    };
    let claim = VersionClaim::new(
        "v".repeat(63),
        signer(),
        Some(EnvelopeVersion {
            seq: 7,
            digest: [8; 32],
        }),
        Some(LogVersion {
            network: [9; 32],
            heads: (0..aterm_secrets::membership_log::MAX_HELD_RECORDS)
                .map(head)
                .collect(),
        }),
    )
    .unwrap();
    let prepared = version::prepare(&key, version::Use::Refused, &claim).unwrap();
    assert_eq!(prepared.parts.len(), version::MAX_PARTS);
    let manifest = version::Manifest::read(&prepared.manifest).unwrap();
    assert!(aterm_json::to_vec(&prepared.manifest).unwrap().len() < MAX_RECORD);
    let dataset =
        crate::crypto::Manifest::deterministic(&SEED, [3; 32], [4; 16], "credentials").unwrap();
    let writer_seed = [6; 32];
    let mut actor = [1; 48];
    actor[..32].copy_from_slice(&crate::crypto::public(&writer_seed).unwrap());
    let capability = crate::crypto::Capability::issue(&dataset, &SEED, actor).unwrap();
    let parents: Vec<_> = (0..astream_wire::sync::MAX_PARENTS).map(head).collect();
    for part in &prepared.parts {
        let payload = part.command().unwrap().encode().unwrap();
        assert_eq!(decode(&payload).unwrap(), part.command().unwrap());
        let wire = crate::crypto::seal_at(
            &dataset,
            &[7; 32],
            &writer_seed,
            &capability,
            crate::crypto::Causal {
                sequence: 1,
                previous: Some(parents[0]),
                parents: &parents,
            },
            &payload,
            1,
        )
        .unwrap();
        assert!(wire.len() <= crate::crypto::MAX_OPERATION);
        assert_eq!(
            crate::crypto::open(&dataset, &[7; 32], &wire)
                .unwrap()
                .plaintext,
            payload
        );
    }
    let mut feed = Feed::new();
    for part in &prepared.parts[1..] {
        feed.put(A, part.clone());
    }
    let resolve = |feed: &Feed, actor: &Actor, usage| {
        version::Parts::read(&feed.entries)
            .resolve(actor, &hex::encode_lower(&key), usage, &manifest)
            .unwrap()
    };
    assert!(resolve(&feed, &A, version::Use::Refused).is_none());
    feed.put(B, prepared.parts[0].clone());
    assert!(
        resolve(&feed, &A, version::Use::Refused).is_none(),
        "a foreign writer supplies no part"
    );
    let mut wrong = prepared.parts[0].clone();
    wrong.body.insert(
        "data".into(),
        Value::from(aterm_secrets::b64::encode_padded(b"wrong public witness")),
    );
    feed.put(A, wrong);
    assert!(resolve(&feed, &A, version::Use::Refused).is_none());
    feed.put(A, prepared.parts[0].clone());
    assert_eq!(
        resolve(&feed, &A, version::Use::Refused),
        Some(claim.clone())
    );
    feed.put(A, prepared.parts[0].clone());
    assert_eq!(resolve(&feed, &A, version::Use::Refused), Some(claim));
    assert!(resolve(&feed, &A, version::Use::Published).is_none());
    assert!(
        version::Parts::read(&feed.entries)
            .resolve(
                &A,
                &hex::encode_lower(&[9; 16]),
                version::Use::Refused,
                &manifest
            )
            .unwrap()
            .is_none()
    );
}

/// A `Retired` record takes whatever evidence and note it is handed, a person's note typed on the
/// vault's admin Mac included: each control character is a space and each field is held to its
/// cap, so the closed schema never refuses it.
#[test]
fn a_retired_record_holds_no_control_character_whatever_it_is_handed() {
    let long = "x".repeat(MAX_NOTE + 9);
    let record = retired_record(
        &[7; 16],
        Reason::Declined,
        "a person\non the admin",
        &format!("no\tnotary\n{long}"),
    );
    assert!(check(&record).is_ok(), "{record:?}");
    let field = |name: &str| record.body.get(name).and_then(Value::as_str).unwrap();
    assert_eq!(field("evidence"), "a person on the admin");
    assert!(
        field("note").starts_with("no notary x"),
        "{}",
        field("note")
    );
    assert_eq!(field("note").chars().count(), MAX_NOTE);
}

/// What a requester takes off its own request's thread beside the custodian's first decision:
/// a `NeedsPerson` on an ask it made standing in for a silent custodian replaces that ask, a
/// window decision whose unanswered members have each swept since restates as every member's
/// sweep, and a late offer moves an ask once, naming who the window's end named.
/// NEGATIVE CONTROL: an ask every member's sweep made takes nothing, nor one a member has still
/// not answered, nor one a late offer already moved.
#[test]
fn a_requester_restates_an_ask_its_thread_shows_rested_on_less() {
    let facts = need::Facts {
        sight: None,
        commons: need::Commons::Writer,
        policy: need::Policy::compiled(true),
        offered: false,
        now: 0,
    };
    let asked = |proof: need::Proof, late_offer| Need {
        state: State::AwaitingPerson,
        asking: true,
        posted: true,
        proof: Some(proof),
        late_offer,
        ..need::fresh([1; 16], Want::Notary, Purpose::ByHand, &facts)
    };
    let proof = need::Proof {
        seq: 1,
        absent: true,
        every_locker_open: true,
        before_commons: false,
        swept_unheld: false,
        window_ended: false,
        custodian_silent: false,
    };
    let window = need::Proof {
        window_ended: true,
        ..proof
    };
    let stood_in = need::Proof {
        custodian_silent: true,
        ..proof
    };
    let every = need::Proof {
        swept_unheld: true,
        ..proof
    };
    let mut feed = Feed::new();
    let id = feed.request(A, 1, "notary");
    let mut sweep = Map::new();
    sweep.insert(
        "covers".into(),
        Value::Array(vec![Value::from(hex::encode_lower(&id))]),
    );
    feed.put(B, Record::new("sweep", &[8; 16], sweep.clone()));
    let members = context(0).members;
    let decide = |feed: &Feed, need: &Need| {
        let book = feed.book(0);
        decision_for(&book.threads[&id], need, &members, &book.macs)
    };
    assert_eq!(
        decide(&feed, &asked(window, false)),
        None,
        "C has not answered"
    );
    assert_eq!(
        decide(&feed, &asked(stood_in, false)),
        None,
        "no custodian record"
    );
    feed.put(C, Record::new("sweep", &[9; 16], sweep));
    assert_eq!(
        decide(&feed, &asked(window, false)),
        Some(Decision::NeedsPerson(Unanswered::default())),
        "every member has answered since"
    );
    assert_eq!(decide(&feed, &asked(every, false)), None);
    let named = Unanswered {
        hosts: vec!["m3".into()],
        others: 0,
    };
    let mut body = Map::new();
    body.insert("template".into(), Value::from("vault-lacks"));
    body.insert("unanswered".into(), Value::Array(vec![Value::from("m3")]));
    feed.put(C, attested(Record::new("needs-person", &id, body)));
    assert_eq!(
        decide(&feed, &asked(stood_in, false)),
        Some(Decision::NeedsPerson(named.clone())),
        "the custodian's own decision replaces the stand-in"
    );
    feed.put(B, Record::new("offer", &id, Map::new()));
    assert_eq!(
        decide(&feed, &asked(window, false)),
        Some(Decision::Offered(named)),
        "a late offer names who the window named"
    );
    assert_eq!(decide(&feed, &asked(window, true)), None, "once");
}

/// A custodian's attested decision counts only for the request it names: a member that copies
/// its body and attestation onto another request (one of the same requester's, or another
/// Mac's) gets nothing from it.
#[test]
fn a_custodians_decision_counts_only_for_the_request_it_names() {
    let mut feed = Feed::new();
    let decided = feed.request(A, 1, "notary");
    let mine = feed.request(A, 2, "mailbox:m29");
    let theirs = feed.request(B, 3, "notary");
    let mut template = Map::new();
    template.insert("template".into(), Value::from("vault-lacks"));
    let mut publication = Map::new();
    publication.insert("vault".into(), Value::from("alab"));
    publication.insert("seq".into(), Value::from(7_u64));
    publication.insert("recipient".into(), Value::from(hex::encode_lower(&[4; 32])));
    let records = [
        attested(Record::new("needs-person", &decided, template)),
        attested(Record::new("published", &decided, publication)),
        attested(retired(
            &decided,
            Reason::NotHeld,
            "every member swept; the vault lacks it",
        )),
    ];
    for record in &records {
        feed.put(C, record.clone());
        for target in [mine, theirs] {
            let mut replayed = record.clone();
            replayed.key = hex::encode_lower(&target);
            feed.put(B, replayed);
        }
    }
    let book = feed.book(need::WANT_WINDOW_SECS * 1000);
    assert!(book.threads[&decided].needs_person && book.threads[&decided].published.is_some());
    for target in [mine, theirs] {
        let thread = &book.threads[&target];
        assert!(
            !thread.needs_person && thread.published.is_none() && thread.retired.is_none(),
            "{thread:?}"
        );
    }
}

/// The whole path over a real scratch Commons: m2 lacks `release-notary`, which the vault holds in
/// m1's own section; m2's pass posts the request, m1's pass sweeps and its custodian pass
/// copies the entry into `[m2]` alone and publishes, and m2 applies it. Only the notary
/// consumer's live success fulfills the need.
#[test]
fn a_request_reaches_the_custodian_and_the_vault_gives_it_to_the_requester_alone() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let m1 = Audience::Hosts(vec!["m1".into()]);
    doc.set_entry(&m1, "release-notary", b"apple").unwrap();
    doc.set_entry(&m1, "m1-only", b"stays").unwrap();
    doc.set_entry(&m1, "m29_password", b"fixture-mailbox")
        .unwrap();
    client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }
    let raised = need::raise(&lab.paths[1], Want::Notary, Purpose::ReleaseNotary).unwrap();
    assert_eq!(raised.state, State::Local);
    let now = 1_000_000_000;
    let posted = pass_at(&lab.paths[1], now).unwrap();
    assert!(posted.writer);
    assert_eq!(posted.posted, 1);
    let open = &need::list(&lab.paths[1]).unwrap()[0];
    assert_eq!((open.state, open.posted), (State::Open, true));
    assert_eq!(
        pass_at(&lab.paths[1], now).unwrap().posted,
        0,
        "a second pass posts nothing new"
    );
    repair_commons(&lab, 3);
    let swept = pass_at(&lab.paths[0], now).unwrap();
    assert_eq!(swept.swept, 1);
    let custodian = custodian_pass_at(&lab.paths[0], now).unwrap().unwrap();
    assert_eq!(custodian.published.len(), 1, "{custodian:?}");
    assert!(
        custodian_pass_at(&lab.paths[1], now).unwrap().is_none(),
        "a Mac without the vault's key is no custodian"
    );
    let reopened = client::admin_open(&lab.paths[0]).unwrap();
    let text = &reopened.doc;
    let m2 = Audience::Hosts(vec!["m2".into()]);
    assert!(
        text.entry(&m2, "release-notary")
            .is_some_and(|entry| entry.if_absent)
    );
    assert!(
        text.entry(&m2, "m1-only").is_none(),
        "the copy took one entry"
    );
    assert!(text.entry(&m1, "m1-only").is_some());
    repair_commons(&lab, 3);
    let heard = pass_at(&lab.paths[1], now).unwrap();
    assert_eq!(heard.decided, 1);
    let published = &need::list(&lab.paths[1]).unwrap()[0];
    assert_eq!(published.state, State::Published, "{}", published.line);
    client::sync(&lab.paths[1], None).unwrap();
    assert_eq!(
        need::list(&lab.paths[1]).unwrap()[0].state,
        State::Published
    );
    let done = need::satisfied(&lab.paths[1], &Want::Notary, Purpose::ReleaseNotary)
        .unwrap()
        .unwrap();
    assert_eq!(
        done.state,
        State::Fulfilled,
        "the live consumer accepted this Mac's entry: {}",
        done.line
    );
    assert_eq!(
        pass_at(&lab.paths[1], now).unwrap().answered,
        1,
        "Fulfilled is written"
    );
    let book = pass_at(&lab.paths[1], now).unwrap();
    assert!(book.threads[0].fulfilled.is_some(), "{:?}", book.threads);

    let floor = done.seq.unwrap();
    let refused =
        need::raise_refused(&lab.paths[1], Want::Notary, Purpose::ReleaseNotary, floor).unwrap();
    assert_ne!(refused.id, done.id);
    let requester = observe(&lab.paths[1]).unwrap().observer;
    let refused_key = request_key(&requester, &refused.id);
    pass_at(&lab.paths[1], now).unwrap();
    repair_commons(&lab, 3);
    let before = client::admin_open(&lab.paths[0]).unwrap();
    let before_text = before.doc.render();
    let same = custodian_pass_at(&lab.paths[0], now).unwrap().unwrap();
    assert!(same.published.is_empty(), "{same:?}");
    assert!(same.waiting.iter().any(|(id, _)| *id == refused_key));
    let unchanged = client::admin_open(&lab.paths[0]).unwrap();
    assert_eq!(unchanged.seq(), floor);
    assert!(unchanged.doc.render().as_slice() == before_text.as_slice());
    pass_at(&lab.paths[1], now).unwrap();
    let waiting = need::peek(&lab.paths[1]).unwrap();
    assert_eq!(waiting[0].id, refused.id);
    assert!(!waiting[0].got && !waiting[0].state.ended());

    // Another real copy may publish the batch, but must not launder this request's
    // refused source through that new version.
    let mailbox = need::raise(
        &lab.paths[1],
        Want::Mailbox("m29".into()),
        Purpose::HarnessSignIn,
    )
    .unwrap();
    let mailbox_key = request_key(&requester, &mailbox.id);
    pass_at(&lab.paths[1], now).unwrap();
    repair_commons(&lab, 3);
    let mixed = custodian_pass_at(&lab.paths[0], now).unwrap().unwrap();
    assert!(
        mixed.published.iter().any(|(id, _)| *id == mailbox_key),
        "{mixed:?}"
    );
    assert!(
        !mixed.published.iter().any(|(id, _)| *id == refused_key),
        "{mixed:?}"
    );

    // The admin really publishes a later value. That version permits a new consumer
    // attempt; version ordering alone does not claim the credential authenticates.
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&m2, "release-notary", b"replacement")
        .unwrap();
    let later = client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    assert!(later.seq > floor);
    let delivered = custodian_pass_at(&lab.paths[0], now).unwrap().unwrap();
    assert!(
        delivered.published.iter().any(|(id, version)| {
            *id == refused_key
                && matches!(version, client::version::RefusalClaim::LegacySequence(sequence) if *sequence > floor)
        }),
        "{delivered:?}"
    );
    repair_commons(&lab, 3);
    client::sync(&lab.paths[1], None).unwrap();
    pass_at(&lab.paths[1], now).unwrap();
    let delivered = need::peek(&lab.paths[1]).unwrap();
    let delivered = delivered.iter().find(|need| need.id == refused.id).unwrap();
    assert_eq!(delivered.state, State::Published);
    assert!(delivered.seq.is_some_and(|seq| seq > floor));
    let accepted = need::satisfied(&lab.paths[1], &Want::Notary, Purpose::ReleaseNotary)
        .unwrap()
        .unwrap();
    assert_eq!(accepted.state, State::Fulfilled);

    // A native log handout is a supported recipient too. Its signed record history
    // advances while its envelope sequence remains zero; no scalar is invented for it.
    struct HeldServer(Option<aterm_secrets::server::Running>);
    impl HeldServer {
        fn stop(&mut self) {
            if let Some(server) = self.0.take() {
                server.stop();
            }
        }
    }
    impl Drop for HeldServer {
        fn drop(&mut self) {
            self.stop();
        }
    }
    let serve = |store| {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let listen = listener.local_addr().unwrap();
        let server = aterm_secrets::server::start(
            listener,
            &aterm_secrets::server::Options {
                listen,
                store,
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(10),
                keep_awake: false,
            },
        )
        .unwrap();
        (HeldServer(Some(server)), format!("http://{listen}/v1/team"))
    };
    let root = aterm_tempfile::tempdir().unwrap();
    let store = root.path().join("served");
    let (server, url) = serve(store.clone());
    let source = Paths::for_home(&root.path().join("m1"));
    let created = client::create(&source, &url, None, Some("m1")).unwrap();
    let mut logged = Lab {
        _home: root,
        paths: vec![source],
        signer: created.signer,
        source: store.join("team.vault"),
    };
    let mut server = server;
    let want = Want::MailboxComponent(
        aterm_secrets::mailbox_component::Component::for_mailbox(
            "https://mail.example",
            "log-recipient@example",
            aterm_secrets::mailbox_component::Part::Password,
        )
        .unwrap(),
    );
    let entry = want.entry().unwrap();
    let purpose = Purpose::HarnessSignIn;
    let source = &logged.paths[0];
    let admin = client::admin_open(source).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&m1, &entry, b"first-log-token").unwrap();
    client::admin_publish(source, &admin, &doc).unwrap();
    client::seed_vault_genesis(source, &logged._home.path().join("genesis.v2")).unwrap();
    let recipient = Paths::for_home(&logged._home.path().join("log-recipient"));
    client::machine_identity(&recipient).unwrap();
    let device = aterm_secrets::commons::bootstrap::Device::open(&recipient)
        .unwrap()
        .unwrap();
    let bootstrap = logged.bootstrap(0);
    let trust = bootstrap.trust();
    let request = device.request(trust.network, trust.authority).unwrap();
    let allowed = bootstrap
        .device()
        .with_seed(|seed| aterm_secrets::members::Allowed::sign(request.binding(), "m2", seed))
        .unwrap();
    let handout = client::hand_over(
        source,
        &aterm_secrets::members::desk::Handing {
            request: &request,
            node: request.binding().recipient().0,
            name: allowed.name(),
            allowed: &allowed,
            reader: aterm_secrets::members::bundle::Reader::LegacyLog,
            restore: false,
            node_proven: false,
            held: None,
        },
    )
    .unwrap();
    assert!(handout.logged);
    let bundle = aterm_secrets::members::Bundle::decode(handout.bundle.as_slice()).unwrap();
    client::write_from_log_bundle(&recipient, &bundle).unwrap();
    let config = client::config(&recipient).unwrap().unwrap();
    let admitted = client::with_materialization_ready(&recipient, &config, || Ok(()))
        .unwrap()
        .unwrap();
    assert_eq!(admitted.snapshot.sequence(), 0);
    assert!(!recipient.dir.join("vault").exists());
    logged.paths.push(recipient);
    for paths in &logged.paths {
        need::test_alab_policy(paths);
    }
    let source = &logged.paths[0];
    let recipient = &logged.paths[1];
    need::raise(recipient, want.clone(), purpose).unwrap();
    assert_eq!(pass_at(recipient, now).unwrap().posted, 1);
    repair_commons(&logged, 3);
    let delivered = custodian_pass_at(source, now).unwrap().unwrap();
    assert_eq!(delivered.published.len(), 1, "{delivered:?}");

    // The owned primary is unavailable; the real Commons exchange still delivers
    // its signed records, and normal sync materializes that log-only view.
    server.stop();
    repair_commons(&logged, 3);
    client::sync(recipient, None).unwrap();
    pass_at(recipient, now).unwrap();
    let paired = || {
        let config = client::config(recipient).unwrap().unwrap();
        client::with_materialization_ready_for_entries(
            recipient,
            &config,
            &[entry.as_str()],
            || Ok(()),
        )
        .unwrap()
        .unwrap()
        .snapshot
    };
    let first = paired();
    assert_eq!(first.sequence(), 0);
    assert!(!recipient.dir.join("vault").exists());
    assert_eq!(
        std::fs::read(recipient.home.join(".secrets").join(&entry)).unwrap(),
        b"first-log-token"
    );
    let delivered = need::peek(recipient).unwrap().remove(0);
    assert_eq!(delivered.state, State::Published);
    let accepted = need::acknowledge_from_snapshot(recipient, &delivered, &first, || Ok(()))
        .unwrap()
        .unwrap();
    assert_eq!(accepted.state, State::Fulfilled);
    let refused =
        need::raise_refused_from_snapshot(recipient, want, purpose, &first, || Ok(())).unwrap();
    assert_ne!(refused.id, accepted.id);
    pass_at(recipient, now).unwrap();
    repair_commons(&logged, 3);
    let unchanged = custodian_pass_at(source, now).unwrap().unwrap();
    assert!(unchanged.published.is_empty(), "{unchanged:?}");

    let admin = client::admin_cached(source).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&m2, &entry, b"newer-log-token").unwrap();
    let (_mirror, mirror_url) = serve(logged._home.path().join("mirror"));
    client::admin_publish_with_candidates(source, &admin, &doc, &[mirror_url]).unwrap();
    let delivered = custodian_pass_at(source, now).unwrap().unwrap();
    assert_eq!(delivered.published.len(), 1, "{delivered:?}");
    assert!(matches!(
        delivered.published[0].1,
        client::version::RefusalClaim::Authenticated(_)
    ));
    repair_commons(&logged, 3);
    client::sync(recipient, None).unwrap();
    pass_at(recipient, now).unwrap();
    let newer = paired();
    assert_eq!(newer.sequence(), 0);
    assert!(!recipient.dir.join("vault").exists());
    assert_eq!(
        std::fs::read(recipient.home.join(".secrets").join(&entry)).unwrap(),
        b"newer-log-token"
    );
    let delivered = need::peek(recipient).unwrap().remove(0);
    assert_eq!(delivered.id, refused.id);
    assert_eq!(delivered.state, State::Published);
    let accepted = need::acknowledge_from_snapshot(recipient, &delivered, &newer, || Ok(()))
        .unwrap()
        .unwrap();
    assert_eq!(accepted.state, State::Fulfilled);

    // Reuse the first real two-writer world for the repository consumer's protocol. An
    // unrelated signed publication may retry the same value; delivery cannot accept it.
    let source = &lab.paths[0];
    let recipient = &lab.paths[1];
    let admin = client::admin_open(source).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        "gh_access_token",
        b"fixture-development-value",
    )
    .unwrap();
    client::admin_publish(source, &admin, &doc).unwrap();
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }
    let paired_gh = || {
        let config = client::config(recipient).unwrap().unwrap();
        client::with_materialization_ready_for_entries(
            recipient,
            &config,
            &["gh_access_token"],
            || Ok(()),
        )
        .unwrap()
        .unwrap()
        .snapshot
    };
    let want = Want::Gh(need::GhProfile::Dev);
    let purpose = Purpose::AghDev;
    let refused = need::raise_refused_from_snapshot(
        recipient,
        want.clone(),
        purpose,
        &paired_gh(),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(refused.state, State::Local);
    assert_eq!(pass_at(recipient, now).unwrap().posted, 1);
    let posted = need::peek(recipient)
        .unwrap()
        .into_iter()
        .find(|row| row.id == refused.id)
        .unwrap();
    assert_eq!(posted.state, State::Open);
    assert!(posted.posted && posted.record.is_some());
    repair_commons(&lab, 3);
    let admin = client::admin_open(source).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "retry-marker", b"later")
        .unwrap();
    client::admin_publish(source, &admin, &doc).unwrap();
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }
    let copied = custodian_pass_at(source, now).unwrap().unwrap();
    let key = request_key(&observe(recipient).unwrap().observer, &refused.id);
    assert!(
        copied.published.iter().any(|(id, _)| *id == key),
        "{copied:?}"
    );
    repair_commons(&lab, 3);
    assert!(pass_at(recipient, now).unwrap().decided > 0);
    let delivered = need::list(recipient)
        .unwrap()
        .into_iter()
        .find(|row| row.id == refused.id)
        .unwrap();
    assert_eq!(
        delivered.state,
        State::Published,
        "an actual attested delivery cannot replace the AghDev consumer's check"
    );
    assert!(delivered.got && delivered.refused.is_some());
    assert_eq!(
        std::fs::read(recipient.home.join(".secrets/gh_access_token")).unwrap(),
        b"fixture-development-value"
    );
    // This is the abstract ledger consumer boundary, not a GitHub authentication claim.
    // The existing Native fixture separately exercises actual parsed 200/401/Unknown.
    let accepted = need::acknowledge_from_snapshot(recipient, &delivered, &paired_gh(), || Ok(()))
        .unwrap()
        .unwrap();
    assert_eq!(accepted.state, State::Fulfilled);
}

/// A requester posts at most 16 live requests, the most the table counts from one Mac; a 17th
/// stays in its ledger, unposted, rather than going inert on the table and waiting for good.
#[test]
fn a_pass_posts_at_most_sixteen_and_keeps_the_rest_in_the_ledger() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    for label in 0..=need::MAX_OPEN {
        let want = Want::parse(&format!("mailbox:m{label}")).unwrap();
        need::raise(&lab.paths[1], want, Purpose::HarnessSignIn).unwrap();
    }
    let now = 1_000_000_000;
    let pass = pass_at(&lab.paths[1], now).unwrap();
    assert_eq!(pass.posted, need::MAX_OPEN);
    assert!(pass.inert.is_empty(), "{:?}", pass.inert);
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().posted, 0);
    let needs = need::list(&lab.paths[1]).unwrap();
    let local: Vec<&need::Need> = needs.iter().filter(|n| n.state == State::Local).collect();
    assert_eq!(local.len(), 1, "the 17th waits in the ledger");
    need::retire(&lab.paths[1], "mailbox:m0", false, "").unwrap();
    pass_at(&lab.paths[1], now).unwrap();
    let after = pass_at(&lab.paths[1], now).unwrap();
    assert!(after.inert.is_empty(), "{:?}", after.inert);
    assert!(
        need::list(&lab.paths[1])
            .unwrap()
            .iter()
            .all(|n| n.state != State::Local),
        "a slot that ends lets the 17th post"
    );
}

/// A want the vault lacks and no member holds goes to a person once every member swept, with
/// the ask on the requester; a decline there retires it on the table too.
#[test]
fn a_want_nobody_holds_goes_to_a_person_and_a_decline_retires_it_in_the_lab() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    need::raise(&lab.paths[1], Want::Notary, Purpose::ReleaseNotary).unwrap();
    let now = 1_000_000_000;
    pass_at(&lab.paths[1], now).unwrap();
    repair_commons(&lab, 3);
    assert_eq!(pass_at(&lab.paths[0], now).unwrap().offered, 0);
    let custodian = custodian_pass_at(&lab.paths[0], now).unwrap().unwrap();
    assert_eq!(custodian.asked.len(), 1, "{custodian:?}");
    repair_commons(&lab, 3);
    pass_at(&lab.paths[1], now).unwrap();
    let asked = &need::list(&lab.paths[1]).unwrap()[0];
    assert_eq!(asked.state, State::AwaitingPerson);
    assert!(
        asked
            .ask
            .as_ref()
            .is_some_and(|ask| ask.needs_you.starts_with("NEEDS YOU:"))
    );
    assert_eq!(need::exceptions(std::slice::from_ref(asked)).len(), 1);
    need::retire(&lab.paths[1], "notary", true, "no notary account here").unwrap();
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().answered, 1);
    repair_commons(&lab, 3);
    let seen = pass_at(&lab.paths[0], now).unwrap();
    assert_eq!(
        seen.threads[0].retired.as_ref().map(|(r, _)| r.reason),
        Some(Reason::Declined)
    );
    assert!(need::exceptions(&need::list(&lab.paths[1]).unwrap()).is_empty());
}

/// A request awaiting a person whose requester is gone, or cannot decline it there, is declined
/// on the vault's admin Mac by the id `need ls --all` prints: one attested `declined` that keeps
/// the person's note, which the requester takes in, its ask taken down. On the admin Mac its own
/// request takes its table id too, and a want key its ledger names only in an ended row still
/// reaches another Mac's request.
/// NEGATIVE CONTROL: a member that is not the admin, and the admin while its custody does not
/// open, refuse and say why; an open request no custodian handed to a person, and a published
/// one, are not declined.
#[test]
fn a_request_awaiting_a_person_is_declined_on_the_vaults_admin_mac() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let (custodian, requester) = (&lab.paths[0], &lab.paths[1]);
    let mailbox = Want::parse("mailbox:m29").unwrap();
    need::raise(requester, Want::Notary, Purpose::ReleaseNotary).unwrap();
    need::raise(requester, mailbox.clone(), Purpose::ByHand).unwrap();
    need::raise(custodian, mailbox.clone(), Purpose::HarnessSignIn).unwrap();
    let now = 1_000_000_000;
    assert_eq!(pass_at(requester, now).unwrap().posted, 2);
    assert_eq!(pass_at(custodian, now).unwrap().posted, 1);
    repair_commons(&lab, 3);
    let threads = pass_at(custodian, now).unwrap().threads;
    let id_of = |want: &Want, purpose| {
        threads
            .iter()
            .find(|t| t.want == *want && t.purpose == purpose)
            .unwrap()
            .id
    };
    let id = id_of(&Want::Notary, Purpose::ReleaseNotary);
    let theirs = id_of(&mailbox, Purpose::ByHand);
    let own = id_of(&mailbox, Purpose::HarnessSignIn);
    pass_at(requester, now).unwrap();
    repair_commons(&lab, 3);
    let asked = custodian_pass_at(custodian, now).unwrap().unwrap();
    assert_eq!(asked.asked.len(), 3, "{asked:?}");
    repair_commons(&lab, 3);
    for paths in [requester, custodian] {
        pass_at(paths, now).unwrap();
        for need in need::list(paths).unwrap() {
            assert_eq!(need.state, State::AwaitingPerson, "{}", need.line);
        }
    }
    let retire = |paths: &Paths, selector: &str| {
        let words = [
            "retire",
            selector,
            "--declined",
            "--note",
            "no notary\nhere",
        ];
        cli::run(paths, &words.map(str::to_owned))
    };
    let live = |paths: &Paths, id: &[u8; 16]| {
        let pass = pass_at(paths, now).unwrap();
        pass.threads
            .into_iter()
            .find(|t| t.id == *id)
            .unwrap()
            .retired
    };
    let (refused, _) = retire(requester, &hex::encode_lower(&id)).unwrap_err();
    assert!(
        refused.contains("no need on this Mac")
            && refused.contains("opens the vault's admin custody, which it cannot now ("),
        "{refused}"
    );
    assert_eq!(
        live(requester, &id),
        None,
        "a member that is not the admin writes nothing"
    );
    let cache = custodian.dir.join("vault");
    let aside = custodian.dir.join("vault.aside");
    std::fs::rename(&cache, &aside).unwrap();
    let (shut, _) = retire(custodian, &hex::encode_lower(&id)).unwrap_err();
    std::fs::rename(&aside, &cache).unwrap();
    assert!(
        shut.contains("which it cannot now (no cached admin vault)"),
        "the admin Mac says why its custody is shut: {shut}"
    );
    let mine = retire(custodian, &hex::encode_lower(&own)).unwrap();
    assert!(
        mine.contains("mailbox:m29  retired  harness.sign-in"),
        "its own request by its table id: {mine}"
    );
    let by_key = retire(custodian, "mailbox:m29").unwrap();
    assert!(
        by_key.contains("mailbox:m29  retired declined  by-hand"),
        "another Mac's, past an ended row of its own: {by_key}"
    );
    let text = retire(custodian, &hex::encode_lower(&id)).unwrap();
    assert!(text.contains("notary  retired declined"), "{text}");
    repair_commons(&lab, 3);
    assert_eq!(pass_at(requester, now).unwrap().decided, 2);
    let needs = need::list(requester).unwrap();
    let declined = needs.iter().find(|need| need.want == Want::Notary).unwrap();
    assert_eq!(declined.state, State::Retired, "{}", declined.line);
    assert!(
        declined
            .line
            .starts_with("declined on the vault's admin Mac"),
        "{}",
        declined.line
    );
    assert_eq!(
        declined
            .retired
            .as_ref()
            .map(|r| (r.reason, r.note.as_str())),
        Some((Reason::Declined, "no notary here"))
    );
    assert!(need::exceptions(&needs).is_empty(), "the asks are down");
    let seen = live(requester, &id).map(|(retired, attested)| (retired.reason, attested));
    assert_eq!(
        seen,
        Some((Reason::Declined, true)),
        "the requester's replica reads it"
    );
    assert!(live(requester, &theirs).is_some());
    need::raise(requester, Want::Notary, Purpose::ByHand).unwrap();
    assert_eq!(pass_at(requester, now).unwrap().posted, 1);
    let open = pass_at(requester, now)
        .unwrap()
        .threads
        .into_iter()
        .find(|t| t.want == Want::Notary && !t.ended())
        .unwrap()
        .id;
    repair_commons(&lab, 3);
    let (refused, _) = retire(custodian, &hex::encode_lower(&open)).unwrap_err();
    assert!(refused.contains("awaits a person"), "{refused}");
    assert_eq!(
        live(custodian, &open),
        None,
        "an open request is not declined"
    );
    // Handed to a person, then published once the vault holds it: a decline by its want key
    // passes it by.
    let handed = custodian_pass_at(custodian, now).unwrap().unwrap();
    assert_eq!(handed.asked, [open], "{handed:?}");
    let admin = client::admin_open(custodian).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Hosts(vec!["m1".into()]),
        aterm_secrets::need::catalog::NOTARY_ENTRY,
        b"apple",
    )
    .unwrap();
    client::admin_publish(custodian, &admin, &doc).unwrap();
    client::sync(custodian, None).unwrap();
    let given = custodian_pass_at(custodian, now).unwrap().unwrap();
    assert_eq!(given.published.len(), 1, "{given:?}");
    let (passed, _) = retire(custodian, "notary").unwrap_err();
    assert!(passed.contains("awaits a person"), "{passed}");
    assert_eq!(
        live(custodian, &open),
        None,
        "a published request is not declined"
    );
}

/// A mailbox's TOTP travels only with its password: a vault that holds the TOTP alone copies
/// nothing, and the request goes to a person. The same pass publishes another request's copy,
/// so a TOTP put in the draft would reach the vault.
#[test]
fn a_mailbox_totp_never_travels_without_its_password() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let m1 = Audience::Hosts(vec!["m1".into()]);
    doc.set_entry(&m1, "m29_totp", b"seed").unwrap();
    doc.set_entry(&m1, "release-notary", b"apple").unwrap();
    client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }
    let mailbox = Want::parse("mailbox:m29").unwrap();
    need::raise(&lab.paths[1], mailbox, Purpose::HarnessSignIn).unwrap();
    need::raise(&lab.paths[1], Want::Notary, Purpose::ReleaseNotary).unwrap();
    let now = 1_000_000_000;
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().posted, 2);
    repair_commons(&lab, 3);
    pass_at(&lab.paths[0], now).unwrap();
    let custodian = custodian_pass_at(&lab.paths[0], now).unwrap().unwrap();
    assert_eq!(custodian.asked.len(), 1, "{custodian:?}");
    assert_eq!(custodian.published.len(), 1, "the notary: {custodian:?}");
    let reopened = client::admin_open(&lab.paths[0]).unwrap();
    let m2 = Audience::Hosts(vec!["m2".into()]);
    assert!(
        reopened.doc.entry(&m2, "release-notary").is_some(),
        "this pass published"
    );
    assert!(
        reopened.doc.entry(&m2, "m29_totp").is_none(),
        "the TOTP stays where it was"
    );
}

/// A live request of this Mac's that no need in its ledger stands for (its need ended and was
/// raised again before a pass wrote the end) is superseded by the requester, so the new
/// request is the one live for its want and purpose, and the lab stops answering the old.
#[test]
fn a_request_no_need_stands_for_is_superseded_so_its_successor_is_live() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let now = 1_000_000_000;
    need::raise(&lab.paths[1], Want::Notary, Purpose::ByHand).unwrap();
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().posted, 1);
    need::retire(&lab.paths[1], "notary", false, "").unwrap();
    let again = need::raise(&lab.paths[1], Want::Notary, Purpose::ByHand).unwrap();
    assert_eq!(again.state, State::Local);
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().posted, 1);
    let book = pass_at(&lab.paths[1], now).unwrap();
    assert!(book.inert.is_empty(), "{:?}", book.inert);
    let live: Vec<&Thread> = book.threads.iter().filter(|t| !t.ended()).collect();
    assert_eq!(live.len(), 1, "{:?}", book.threads);
    assert!(book.threads.iter().any(|t| {
        t.retired
            .as_ref()
            .is_some_and(|(r, _)| r.reason == Reason::Superseded)
    }));
    let open = &need::list(&lab.paths[1]).unwrap()[0];
    assert_eq!(open.state, State::Open, "{}", open.line);
}

#[test]
fn custodian_pass_requires_commons_writer_before_publishing_vault() {
    let lab = Lab::new(&[Role::Reader, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let m1 = Audience::Hosts(vec!["m1".into()]);
    doc.set_entry(&m1, "release-notary", b"apple").unwrap();
    let published = client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }
    let now = 1_000_000_000;
    need::raise(&lab.paths[1], Want::Notary, Purpose::ReleaseNotary).unwrap();
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().posted, 1);
    repair_commons(&lab, 3);

    // m1 is a Commons Reader: custodian_pass_at must refuse before copying/publishing the vault.
    assert!(custodian_pass_at(&lab.paths[0], now).is_err());
    let reopened = client::admin_open(&lab.paths[0]).unwrap();
    assert_eq!(reopened.seq(), published.seq);
    let m2 = Audience::Hosts(vec!["m2".into()]);
    assert!(reopened.doc.entry(&m2, "release-notary").is_none());
}

#[test]
fn roster_denied_binding_excluded_from_credential_copy_and_sweep_quorum() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let m1 = Audience::Hosts(vec!["m1".into()]);
    doc.set_entry(&m1, "release-notary", b"apple").unwrap();
    let published = client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }
    let now = 1_000_000_000;
    // Both m2 and m3 post requests before m3's machine id is denied in aterm-swarm-roster.
    need::raise(
        &lab.paths[1],
        Want::Mailbox("absent".into()),
        Purpose::ByHand,
    )
    .unwrap();
    need::raise(&lab.paths[2], Want::Notary, Purpose::ReleaseNotary).unwrap();
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().posted, 1);
    assert_eq!(pass_at(&lab.paths[2], now).unwrap().posted, 1);
    repair_commons(&lab, 3);

    // Deny m3's machine id in aterm-swarm-roster on all replicas.
    let m3_binding = lab.bootstrap(2).request().unwrap().binding().clone();
    let roster_toml = format!(
        "deny_nodes = [\"{}\"]\n",
        aterm_codec::hex::encode_lower(m3_binding.machine()),
    );
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        aterm_secrets::swarm_roster::ROSTER_ENTRY,
        roster_toml.as_bytes(),
    )
    .unwrap();
    let roster_pub = client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    let _ = published;
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }

    // m1 sweeps m2's request; m3 is roster-denied so m1 + m2 are the only active members.
    let m1_pass = pass_at(&lab.paths[0], now).unwrap();
    assert_eq!(m1_pass.swept, 1, "m1 sweeps only m2's active request");
    let custodian = custodian_pass_at(&lab.paths[0], now).unwrap().unwrap();
    assert!(
        custodian.published.is_empty(),
        "roster-denied m3 must not receive a custodian copy: {custodian:?}"
    );
    assert_eq!(
        custodian.asked.len(),
        1,
        "m2's request reaches needs-person without waiting for roster-denied m3's sweep: {custodian:?}"
    );
    let reopened = client::admin_open(&lab.paths[0]).unwrap();
    assert_eq!(reopened.seq(), roster_pub.seq);
    let m3 = Audience::Hosts(vec!["m3".into()]);
    assert!(reopened.doc.entry(&m3, "release-notary").is_none());
}

/// NEGATIVE CONTROL: symlinked or non-private (`0644`) holder files never post an unreadable
/// offer, while a private (`0600`) mailbox password beside a blank `*_totp` file validates cleanly.
#[test]
fn unprivate_or_symlinked_holder_files_do_not_offer_and_blank_totp_is_ignored() {
    use std::os::unix::fs::PermissionsExt as _;

    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let want = Want::Mailbox("m29".into());
    need::raise(&lab.paths[2], want.clone(), Purpose::HarnessSignIn).unwrap();
    let now = 1_000_000_000;
    assert_eq!(pass_at(&lab.paths[2], now).unwrap().posted, 1);
    repair_commons(&lab, 3);

    let secrets_dir = lab.paths[1].home.join(".secrets");
    aterm_secrets::fsx::ensure_private_dir(&secrets_dir).unwrap();
    let pw_path = secrets_dir.join("m29_password");
    let target_file = lab.paths[1].home.join("outside-secret");
    aterm_secrets::fsx::write_atomic(&target_file, b"fixture-password\n", 0o600).unwrap();
    std::os::unix::fs::symlink(&target_file, &pw_path).unwrap();
    assert!(!super::holds(&lab.paths[1], &want));
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().offered, 0);

    std::fs::remove_file(&pw_path).unwrap();
    std::fs::write(&pw_path, b"fixture-password\n").unwrap();
    std::fs::set_permissions(&pw_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!super::holds(&lab.paths[1], &want));

    std::fs::set_permissions(&pw_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let totp_path = secrets_dir.join("m29_totp");
    aterm_secrets::fsx::write_atomic(&totp_path, b"   \n", 0o600).unwrap();
    assert!(super::holds(&lab.paths[1], &want));

    let seen = pass_at(&lab.paths[2], now).unwrap();
    let thread = seen.threads.iter().find(|t| t.want == want).unwrap();
    let values = super::source::read(&lab.paths[1], [9; 16], thread, now).unwrap();
    assert_eq!(values.0.len(), 1);
    assert_eq!(values.0[0].0, "m29_password");
    assert_eq!(super::value::validate(&want, &values).unwrap(), None);
}

/// A pass beside a window whose runtime keeps the control journal's writer past the 2 s an open
/// that names no deadline waits in product (one repair exchange's sessions back to back) waits it
/// out and reaches the table, rather than telling the person the table was not reached. It waits
/// holding no Commons custody, so meanwhile this Mac still reads as a Writer (the window's
/// `sync.requests` read, a need's facts), which takes custody for at most a second.
#[test]
fn a_pass_waits_out_a_window_that_holds_the_journal_holding_no_custody() {
    /// What an open that names no deadline waits in product ([`crate::admission`]); this test
    /// binary's opens wait the hang detector.
    const PRODUCT_ADMISSION: std::time::Duration = std::time::Duration::from_secs(2);
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    need::raise(&lab.paths[1], Want::Notary, Purpose::ByHand).unwrap();
    let bootstrap = lab.bootstrap(1);
    let held = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let (waited, waiting) = std::sync::mpsc::channel();
    let paths = lab.paths[1].clone();
    let passing = std::thread::spawn(move || {
        let mut since = None;
        aterm_uds::file_lock::with_lock_wait_observer(
            move |_| {
                // Hold on past the product's admission, counted from the pass's first wait.
                let since = *since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() > PRODUCT_ADMISSION + std::time::Duration::from_secs(1) {
                    let _ = waited.send(());
                }
            },
            || pass(&paths),
        )
    });
    match waiting.recv_timeout(aterm_spec::wait::MUST_HAPPEN) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!(
            "the pass gave up before the window let go: {:?}",
            passing.join().unwrap().map(|passed| passed.posted)
        ),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("the pass never waited on the held journal")
        }
    }
    assert!(
        writer_here(&lab.paths[1]),
        "a pass waiting on the journal kept custody"
    );
    drop(held);
    let passed = passing.join().unwrap().unwrap();
    assert!(passed.writer);
    assert_eq!(passed.posted, 1);
}

/// A withdrawal by hand reaches the lab's table as the requester's `not-needed`, resting on the
/// person's word with their note (a control character in it read as a space, since the record
/// refuses one), and `need ls` shows the note in its text and its JSON.
#[test]
fn a_withdrawal_by_hand_reaches_the_table_with_its_note_and_need_ls_shows_it() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let now = 1_000_000_000;
    need::raise(&lab.paths[1], Want::Notary, Purpose::ByHand).unwrap();
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().posted, 1);
    need::retire(&lab.paths[1], "notary", false, "no notarizing\non this Mac").unwrap();
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().answered, 1);
    repair_commons(&lab, 3);
    let seen = pass_at(&lab.paths[0], now).unwrap();
    let (retirement, _) = seen.threads[0].retired.clone().unwrap();
    let line = need::list(&lab.paths[1]).unwrap()[0].line.clone();
    assert!(
        line.contains("withdrawn by hand on this Mac (m2)"),
        "{line}"
    );
    assert_eq!(
        (
            retirement.reason,
            retirement.evidence.as_str(),
            retirement.note.as_str()
        ),
        (
            Reason::NotNeeded,
            "withdrawn by hand on m2",
            "no notarizing on this Mac"
        )
    );
    let text = cli::run(&lab.paths[1], &["ls".to_owned()]).unwrap();
    assert!(
        text.contains("\n    note: no notarizing on this Mac\n"),
        "{text}"
    );
    let json = cli::run(&lab.paths[1], &["ls".to_owned(), "--json".to_owned()]).unwrap();
    let row = &aterm_json::from_str::<Value>(&json).unwrap()["needs"][0];
    assert_eq!(
        (row["evidence"].as_str(), row["note"].as_str()),
        (
            Some("withdrawn by hand on m2"),
            Some("no notarizing on this Mac")
        )
    );
}

/// A note an earlier build kept in the ledger as typed, a newline and all, reaches the table and
/// `need ls` with each control character read as a space: the record refuses one, and a pass
/// that wrote it raw would fail every pass on that Mac until the request ended.
#[test]
fn a_note_an_earlier_build_kept_with_a_newline_still_reaches_the_table() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let now = 1_000_000_000;
    need::raise(&lab.paths[1], Want::Notary, Purpose::ByHand).unwrap();
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().posted, 1);
    need::retire(&lab.paths[1], "notary", false, "no notarizing on this Mac").unwrap();
    let ledger = lab.paths[1].dir.join(need::LEDGER);
    let kept = std::fs::read_to_string(&ledger).unwrap();
    let typed = kept.replace(
        "note = \"no notarizing on this Mac\"",
        "note = \"no notarizing\\non this Mac\"",
    );
    assert_ne!(typed, kept, "{kept}");
    std::fs::write(&ledger, typed).unwrap();
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().answered, 1);
    let text = cli::run(&lab.paths[1], &["ls".to_owned()]).unwrap();
    assert!(
        text.contains("\n    note: no notarizing on this Mac\n"),
        "{text}"
    );
}

/// A copy whose publication fails waits alone: the custodian's other decisions in that pass (a
/// person asked here) are committed all the same, and only the copy waits, naming the error,
/// until a pass whose publish goes through publishes it. The vault's publish lock, held by
/// another admin's publish, refuses this pass's.
/// NEGATIVE CONTROL: a pass that returns on the error, as before, commits no ask; one that books
/// the copy at the version the vault already held reports it published; a `need pass` that
/// prints only the counts hides the failure.
#[test]
fn a_failed_publication_waits_alone_and_the_passs_other_decisions_stand() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let (custodian, requester) = (&lab.paths[0], &lab.paths[1]);
    let admin = client::admin_open(custodian).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Hosts(vec!["m1".into()]),
        "release-notary",
        b"apple",
    )
    .unwrap();
    client::admin_publish(custodian, &admin, &doc).unwrap();
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }
    let mailbox = Want::parse("mailbox:m29").unwrap();
    need::raise(requester, mailbox.clone(), Purpose::HarnessSignIn).unwrap();
    need::raise(requester, Want::Notary, Purpose::ReleaseNotary).unwrap();
    let now = 1_000_000_000;
    assert_eq!(pass_at(requester, now).unwrap().posted, 2);
    repair_commons(&lab, 3);
    let swept = pass_at(custodian, now).unwrap();
    assert_eq!(swept.swept, 2);
    let thread = |want: &Want| swept.threads.iter().find(|t| t.want == *want).unwrap().id;
    let (notary, asked) = (thread(&Want::Notary), thread(&mailbox));
    let mut lock = lab.source.clone().into_os_string();
    lock.push(".lock");
    std::fs::write(&lock, "m9 1\n").unwrap();
    let refused = custodian_pass_at(custodian, now).unwrap().unwrap();
    assert_eq!(refused.asked, [asked], "{refused:?}");
    assert!(refused.published.is_empty(), "{refused:?}");
    assert!(
        matches!(refused.failed.as_slice(),
            [(id, why)] if *id == notary && why.contains("another publish to this vault holds it")),
        "{refused:?}"
    );
    let summary = refused.summary();
    assert!(
        summary.contains("1 handed to a person")
            && summary.contains(", 1 failed: the vault's publication failed: "),
        "{summary}"
    );
    // `aterm secrets need pass`, the item's remedy, says the failure too.
    let said = cli::run(custodian, &["pass".to_owned()]).unwrap();
    assert!(
        said.contains("1 failed: the vault's publication failed")
            && said.contains("another publish to this vault holds it"),
        "{said}"
    );
    let m2 = Audience::Hosts(vec!["m2".into()]);
    assert!(
        client::admin_open(custodian)
            .unwrap()
            .doc
            .entry(&m2, "release-notary")
            .is_none(),
        "nothing was published"
    );
    repair_commons(&lab, 3);
    pass_at(requester, now).unwrap();
    let state = |want: &Want| {
        let needs = need::list(requester).unwrap();
        let need = needs.iter().find(|need| need.want == *want).unwrap();
        (need.state, need.line.clone())
    };
    assert_eq!(
        state(&mailbox).0,
        State::AwaitingPerson,
        "{}",
        state(&mailbox).1
    );
    let (waits, line) = state(&Want::Notary);
    assert!(waits != State::Published && !waits.ended(), "{line}");
    std::fs::remove_file(&lock).unwrap();
    let published = custodian_pass_at(custodian, now).unwrap().unwrap();
    assert_eq!(
        published
            .published
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [notary],
        "{published:?}"
    );
    assert!(
        published.asked.is_empty() && published.failed.is_empty(),
        "the ask stands once: {published:?}"
    );
    repair_commons(&lab, 3);
    assert_eq!(pass_at(requester, now).unwrap().decided, 1);
    assert_eq!(
        state(&Want::Notary).0,
        State::Published,
        "{}",
        state(&Want::Notary).1
    );
    assert_eq!(state(&mailbox).0, State::AwaitingPerson);
}

/// A copy the draft refuses leaves the draft as that request found it, and the request waits
/// alone. Here the vault's text, long with comments (which only the admins' locker seals, so it
/// still publishes), is within half a TOTP of its limit: the mailbox's password copies, and its
/// TOTP would take the text past the limit. The password copied before the refusal is not
/// published, the request is the pass's one failure, naming the refusal, and another request's
/// copy in the same pass is published all the same.
/// NEGATIVE CONTROL: a pass that returns on the refusal, as before, publishes nothing; one that
/// keeps the draft the refused request wrote publishes the password into the requester's section
/// without its TOTP or any record.
#[test]
fn a_refused_copy_leaves_the_draft_as_its_request_found_it_and_waits_alone() {
    use aterm_secrets::text::{Document, MAX_TEXT};
    const TOTP: usize = 1 << 16;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let (custodian, requester) = (&lab.paths[0], &lab.paths[1]);
    let admin = client::admin_open(custodian).unwrap();
    let mut doc = admin.doc.clone();
    let m1 = Audience::Hosts(vec!["m1".into()]);
    doc.set_entry(&m1, "release-notary", b"apple").unwrap();
    doc.set_entry(&m1, "m29_password", b"hunter2").unwrap();
    doc.set_entry(&m1, "m29_totp", &vec![b't'; TOTP]).unwrap();
    let held = doc.render();
    let held = held.as_str().unwrap();
    let line = format!("# {}\n", "c".repeat(TOTP - 3));
    let room = MAX_TEXT - TOTP / 2 - held.len();
    let mut text = line.repeat(room / line.len());
    text.push_str(&"#\n".repeat(room % line.len() / 2));
    text.push_str(held);
    assert!(text.len() + TOTP > MAX_TEXT && text.len() + TOTP / 4 < MAX_TEXT);
    client::admin_publish(custodian, &admin, &Document::parse(&text).unwrap()).unwrap();
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }
    let mailbox = Want::parse("mailbox:m29").unwrap();
    need::raise(requester, mailbox.clone(), Purpose::HarnessSignIn).unwrap();
    need::raise(requester, Want::Notary, Purpose::ReleaseNotary).unwrap();
    let now = 1_000_000_000;
    assert_eq!(pass_at(requester, now).unwrap().posted, 2);
    repair_commons(&lab, 3);
    let swept = pass_at(custodian, now).unwrap();
    assert_eq!(swept.swept, 2);
    let thread = |want: &Want| swept.threads.iter().find(|t| t.want == *want).unwrap().id;
    let (notary, refused) = (thread(&Want::Notary), thread(&mailbox));
    let pass = custodian_pass_at(custodian, now).unwrap().unwrap();
    assert!(
        matches!(pass.failed.as_slice(),
            [(id, why)] if *id == refused
                && why.starts_with("the copy from vault text failed: ")
                && why.contains("larger than")),
        "{pass:?}"
    );
    assert_eq!(
        pass.published.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [notary],
        "{pass:?}"
    );
    assert!(pass.asked.is_empty() && pass.waiting.is_empty(), "{pass:?}");
    let reopened = client::admin_open(custodian).unwrap();
    let m2 = Audience::Hosts(vec!["m2".into()]);
    assert!(
        reopened.doc.entry(&m2, "release-notary").is_some(),
        "the notary's copy was published"
    );
    for entry in ["m29_password", "m29_totp"] {
        assert!(
            reopened.doc.entry(&m2, entry).is_none(),
            "{entry} stays where it was"
        );
    }
    repair_commons(&lab, 3);
    pass_at(requester, now).unwrap();
    let needs = need::list(requester).unwrap();
    let waits = needs.iter().find(|need| need.want == mailbox).unwrap();
    assert!(
        waits.state != State::Published && !waits.state.ended(),
        "the mailbox waits: {}",
        waits.line
    );
}

/// A request the lab lapsed (no word from this Mac on it for 30 days: its windows stayed shut)
/// is raised again at once while its need holds, as its line says, with no consumer to raise
/// it: a need by hand posts a new request, under a new id, at the next pass, and supersedes the
/// lapsed one, once, so a member whose clock does not count the lapse yet (it took this Mac's
/// last word in later) reads the new request live and sweeps it rather than drop it as a second
/// live request for its want. A retirement whose reason still holds (a decline, `not-held` with
/// no Mac holding the value) stays retired.
/// NEGATIVE CONTROL: the supersede is written once; a later pass writes no other.
#[test]
fn a_lapsed_request_is_raised_again_at_once_while_its_need_holds() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let (custodian, requester) = (&lab.paths[0], &lab.paths[1]);
    let mailbox = Want::parse("mailbox:m29").unwrap();
    need::raise(requester, Want::Notary, Purpose::ByHand).unwrap();
    need::raise(requester, mailbox.clone(), Purpose::ByHand).unwrap();
    let now = aterm_time::unix_millis().unwrap();
    assert_eq!(pass_at(requester, now).unwrap().posted, 2);
    let first: Vec<[u8; 16]> = need::list(requester)
        .unwrap()
        .iter()
        .map(|need| need.id)
        .collect();
    repair_commons(&lab, 3);
    let lapse = aterm_time::unix_millis().unwrap() + need::LAPSE_SECS * MS;
    let lapsed = custodian_pass_at(custodian, lapse).unwrap().unwrap();
    assert_eq!(
        lapsed
            .retired
            .iter()
            .map(|(_, reason)| *reason)
            .collect::<Vec<_>>(),
        [Reason::Lapsed; 2],
        "{lapsed:?}"
    );
    repair_commons(&lab, 3);
    assert_eq!(pass_at(requester, lapse).unwrap().decided, 2);
    for need in need::list(requester).unwrap() {
        assert_eq!(need.state, State::Local, "{}", need.line);
        assert!(!first.contains(&need.id), "a new request id: {need:?}");
        assert_eq!((need.raises, need.raised_again_on_change), (2, Some(true)));
    }
    let reposted = pass_at(requester, lapse).unwrap();
    assert_eq!(
        (reposted.posted, reposted.answered),
        (2, 2),
        "the next pass posts both again and supersedes the lapsed two"
    );
    let book = pass_at(requester, lapse).unwrap();
    assert_eq!(
        book.answered, 2,
        "the new requests' Stills (30 days on): the supersede is not written again"
    );
    assert!(
        book.inert
            .iter()
            .all(|(_, why)| why == "the request has ended"),
        "{:?}",
        book.inert
    );
    assert_eq!(
        book.threads.iter().filter(|thread| !thread.ended()).count(),
        2,
        "{:?}",
        book.threads
    );
    repair_commons(&lab, 3);
    let early = pass_at(custodian, now).unwrap();
    assert!(
        !early
            .inert
            .iter()
            .any(|(_, why)| why == "a second live request for one want and purpose"),
        "a member that does not count the lapse yet reads the new requests live: {:?}",
        early.inert
    );
    assert_eq!(
        early
            .threads
            .iter()
            .filter(|thread| !thread.ended())
            .count(),
        2,
        "{:?}",
        early.threads
    );
    assert_eq!(early.swept, 2, "and sweeps them");
    let needs = need::list(requester).unwrap();
    let retire = |want: &Want, reason| {
        let need = needs.iter().find(|need| need.want == *want).unwrap();
        assert!(!need.state.ended(), "{}", need.line);
        let retirement = Retirement {
            reason,
            evidence: "a custodian".into(),
            note: String::new(),
            generation: None,
        };
        (need.id, Absorb::Decided(Decision::Retired(retirement)))
    };
    let held = [
        retire(&Want::Notary, Reason::Declined),
        retire(&mailbox, Reason::NotHeld),
    ];
    assert_eq!(need::absorb(requester, &held).unwrap().len(), 2);
    for need in need::list(requester).unwrap() {
        assert_eq!(need.state, State::Retired, "{}", need.line);
        assert!(held.iter().any(|(id, _)| *id == need.id), "{need:?}");
        assert_eq!(need.raises, 2);
    }
    assert_eq!(pass_at(requester, lapse).unwrap().posted, 0);
}

/// A private holder file at `path`, as a holder's copy is kept: the hold probe takes no other.
fn hold(path: &std::path::Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    aterm_secrets::fsx::write_atomic(path, bytes, 0o600).unwrap();
}

/// A posted request the vault admin's aterm writes nothing on (its Mac off, or on a build with
/// no custodian pass) does not wait on it for good: past the silence (the want window and a day,
/// so a live custodian, whose own window starts when its replica took the request in, decides
/// first) a person-sourced vault-only want whose absence proof holds here goes to a person, as
/// before Commons, and any other names the silent admin Mac in its line and files one fact for
/// later. A request a Mac offered on is its offer's, never silent. Running the hand route the
/// line names, on the admin Mac, delivers the value into this Mac's own section, which ends the
/// request and takes the fact down. Inside the silence nothing is said.
/// NEGATIVE CONTROL: at the want window's end alone, nothing calls the custodian silent; a
/// pass that read silence before an offer would call the offered request silent.
#[test]
fn a_request_no_custodian_answers_reaches_a_person_after_the_want_window() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let (custodian, requester) = (&lab.paths[0], &lab.paths[1]);
    let mailbox = Want::parse("mailbox:m29").unwrap();
    need::raise(requester, mailbox.clone(), Purpose::HarnessSignIn).unwrap();
    need::raise(requester, Want::Notary, Purpose::ByHand).unwrap();
    need::raise(requester, Want::SiteDeployerKey, Purpose::ByHand).unwrap();
    let before = aterm_time::unix_millis().unwrap();
    assert_eq!(pass_at(requester, before).unwrap().posted, 3);
    let after = aterm_time::unix_millis().unwrap();
    let window = need::WANT_WINDOW_SECS * MS;
    let silence = need::SILENCE_SECS * MS;
    let quiet = |now: u64| {
        pass_at(requester, now).unwrap();
        let needs = need::list(requester).unwrap();
        for need in &needs {
            assert_eq!(need.state, State::Open, "{}", need.line);
            assert!(need.line.starts_with("posted to the lab"), "{}", need.line);
        }
        assert!(need::exceptions(&needs).is_empty(), "{needs:?}");
    };
    quiet(before + window - MS);
    quiet(after + window);
    hold(
        &Want::SiteDeployerKey.hold_path(&custodian.home).unwrap(),
        b"key",
    );
    repair_commons(&lab, 3);
    assert_eq!(pass_at(custodian, before).unwrap().offered, 1);
    repair_commons(&lab, 3);
    pass_at(requester, after + silence).unwrap();
    let needs = need::list(requester).unwrap();
    let find = |needs: &[Need], want: &Want| {
        needs
            .iter()
            .find(|need| need.want == *want)
            .cloned()
            .unwrap()
    };
    let asked = find(&needs, &mailbox);
    assert_eq!(asked.state, State::AwaitingPerson, "{}", asked.line);
    assert!(
        asked
            .ask
            .as_ref()
            .is_some_and(|ask| ask.needs_you.starts_with("NEEDS YOU:")),
        "{asked:?}"
    );
    let proof = asked.proof.unwrap();
    assert!(
        proof.warrants_asking()
            && proof.custodian_silent
            && !proof.before_commons
            && !proof.swept_unheld,
        "{proof:?}"
    );
    let offered = find(&needs, &Want::SiteDeployerKey);
    assert_eq!(
        (offered.state, offered.custodian_silent),
        (State::Offered, false),
        "{}",
        offered.line
    );
    let notary = find(&needs, &Want::Notary);
    assert_eq!(notary.state, State::Open, "{}", notary.line);
    for words in [
        "the vault's admin Mac",
        "96 hours",
        "aterm secrets set release-notary --for m2 --file <file>",
    ] {
        assert!(notary.line.contains(words), "{words}: {}", notary.line);
    }
    let filed = need::exceptions(&needs);
    assert_eq!(filed.len(), 2, "{filed:?}");
    assert!(
        filed
            .iter()
            .any(|entry| entry.key() == "secrets.need.custodian.silent"),
        "{filed:?}"
    );
    for entry in &filed {
        entry
            .message()
            .unwrap_or_else(|fault| panic!("{entry:?}: {fault:?}"));
    }
    let admin = client::admin_open(custodian).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Hosts(vec!["m2".into()]),
        "release-notary",
        b"apple",
    )
    .unwrap();
    client::admin_publish(custodian, &admin, &doc).unwrap();
    client::sync(requester, None).unwrap();
    pass_at(requester, after + silence).unwrap();
    let needs = need::list(requester).unwrap();
    let given = find(&needs, &Want::Notary);
    assert_eq!(
        given.state,
        State::Fulfilled,
        "the hand route ends it: {}",
        given.line
    );
    let filed = need::exceptions(&needs);
    assert!(
        filed
            .iter()
            .all(|entry| entry.key() != "secrets.need.custodian.silent"),
        "{filed:?}"
    );
}

/// A custodian that answers inside the window decides as before (its `NeedsPerson`, resting on
/// every member's sweep), and nothing past the silence calls it silent.
#[test]
fn a_custodian_that_answers_in_the_window_is_never_called_silent() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let mailbox = Want::parse("mailbox:m29").unwrap();
    need::raise(&lab.paths[1], mailbox, Purpose::HarnessSignIn).unwrap();
    let now = aterm_time::unix_millis().unwrap();
    assert_eq!(pass_at(&lab.paths[1], now).unwrap().posted, 1);
    repair_commons(&lab, 3);
    pass_at(&lab.paths[0], now).unwrap();
    let custodian = custodian_pass_at(&lab.paths[0], now).unwrap().unwrap();
    assert_eq!(custodian.asked.len(), 1, "{custodian:?}");
    repair_commons(&lab, 3);
    let late = aterm_time::unix_millis().unwrap() + need::SILENCE_SECS * MS;
    pass_at(&lab.paths[1], late).unwrap();
    let needs = need::list(&lab.paths[1]).unwrap();
    let asked = &needs[0];
    assert_eq!(asked.state, State::AwaitingPerson, "{}", asked.line);
    assert!(
        asked
            .proof
            .is_some_and(|proof| proof.swept_unheld && !proof.custodian_silent)
    );
    assert!(!asked.line.contains("96 hours"), "{}", asked.line);
    let filed = need::exceptions(&needs);
    assert_eq!(filed.len(), 1, "{filed:?}");
    assert_ne!(filed[0].key(), "secrets.need.custodian.silent");
}

/// The want window's end decides a request some member never answered, and the decision says
/// so: the person is asked, but the proof does not claim every member swept, and the ask names
/// the Mac that did not answer, so a copy it holds is looked for before a new value is made. A
/// holder that answers late and offers moves the ask to that copy, which the custodian's runtime
/// takes, and `need ls --all` shows the offer. A request every member swept keeps its words, and
/// an offer on another request leaves them. A week on, each request awaiting a person still
/// hears from its requester.
/// NEGATIVE CONTROL: the request m3 swept reads as every member's sweep and names nobody; the
/// late offer moves the ask once.
#[test]
fn a_window_decision_names_who_did_not_answer_and_a_late_offer_moves_the_ask() {
    use crate::table::tests::repair_between;
    let lab = Lab::new(&[Role::Writer; 3]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let (custodian, requester, late) = (&lab.paths[0], &lab.paths[1], &lab.paths[2]);
    let now = aterm_time::unix_millis().unwrap();
    need::raise(requester, Want::Notary, Purpose::ReleaseNotary).unwrap();
    assert_eq!(pass_at(requester, now).unwrap().posted, 1);
    repair_between(&lab, 1, 0, 3);
    repair_between(&lab, 0, 2, 3);
    assert_eq!(pass_at(late, now).unwrap().swept, 1, "m3 sweeps the first");
    need::raise(requester, Want::Notary, Purpose::ByHand).unwrap();
    assert_eq!(pass_at(requester, now).unwrap().posted, 1);
    repair_between(&lab, 1, 0, 3);
    repair_between(&lab, 0, 2, 3);
    assert_eq!(pass_at(custodian, now).unwrap().swept, 2);
    let window_end = aterm_time::unix_millis().unwrap() + need::WANT_WINDOW_SECS * MS;
    let decided = custodian_pass_at(custodian, window_end).unwrap().unwrap();
    assert_eq!(decided.asked.len(), 2, "{decided:?}");
    repair_between(&lab, 1, 0, 3);
    let heard = pass_at(requester, window_end).unwrap();
    let named = |purpose| {
        let thread = heard
            .threads
            .iter()
            .find(|thread| thread.purpose == purpose);
        thread.map(|thread| (thread.needs_person, thread.unanswered.clone()))
    };
    assert_eq!(
        named(Purpose::ReleaseNotary),
        Some((true, Unanswered::default()))
    );
    assert_eq!(
        named(Purpose::ByHand),
        Some((
            true,
            Unanswered {
                hosts: vec!["m3".into()],
                others: 0
            }
        )),
        "the requester itself is never one that did not answer"
    );
    let asks = |purpose| {
        let needs = need::list(requester).unwrap();
        let need = needs.iter().find(|need| need.purpose == purpose).unwrap();
        assert_eq!(need.state, State::AwaitingPerson, "{}", need.line);
        (need.proof.unwrap(), need.ask.clone().unwrap())
    };
    let (swept, every) = asks(Purpose::ReleaseNotary);
    assert!(swept.swept_unheld && swept.warrants_asking(), "{swept:?}");
    assert!(
        every.needs_you.contains("is on no Mac the lab can reach")
            && !every.needs_you.contains("m3"),
        "{}",
        every.needs_you
    );
    let (window, unanswered) = asks(Purpose::ByHand);
    assert!(
        !window.swept_unheld && window.window_ended && window.warrants_asking(),
        "m3 never swept it: {window:?}"
    );
    assert!(
        unanswered.needs_you.contains("m3 did not answer")
            && !unanswered
                .needs_you
                .contains("is on no Mac the lab can reach"),
        "{}",
        unanswered.needs_you
    );
    let copy = unanswered.more.iter().position(|line| {
        line.starts_with(
            "m3 did not answer: where one holds ~/.secrets/release-notary, that copy is the <file>",
        )
    });
    let source = unanswered
        .more
        .iter()
        .position(|line| line.starts_with("it comes from:"));
    assert!(copy.is_some() && copy < source, "{:?}", unanswered.more);
    assert!(
        !format!("{unanswered:?}").contains("offers it at its next pass"),
        "a Mac that did not answer may have no sweep: {unanswered:?}"
    );
    hold(&Want::Notary.hold_path(&late.home).unwrap(), b"apple");
    repair_between(&lab, 0, 2, 3);
    assert_eq!(
        pass_at(late, window_end).unwrap().offered,
        1,
        "the second alone"
    );
    repair_between(&lab, 2, 0, 3);
    repair_between(&lab, 1, 0, 3);
    assert_eq!(pass_at(requester, window_end).unwrap().decided, 1);
    let (_, offered) = asks(Purpose::ByHand);
    assert!(
        offered
            .needs_you
            .contains("offered it (m3 had not answered when the window ended)")
            && offered.needs_you.contains(
                "the vault custodian's running aterm takes over the authenticated LAN connection"
            )
            && !offered.needs_you.contains("did not answer"),
        "{}",
        offered.needs_you
    );
    assert!(
        offered
            .more
            .iter()
            .any(|line| line.contains("~/.secrets/release-notary")),
        "{:?}",
        offered.more
    );
    assert_eq!(
        asks(Purpose::ReleaseNotary).1,
        every,
        "no offer on the first"
    );
    assert_eq!(
        pass_at(requester, window_end).unwrap().decided,
        0,
        "an offer moves the ask once"
    );
    let listed = cli::run(requester, &["ls".to_owned(), "--all".to_owned()]).unwrap();
    assert!(listed.contains("awaiting-person, offered by 1"), "{listed}");
    assert!(listed.contains("awaiting-person, swept by 2"), "{listed}");
    assert_eq!(
        need::needs_you(&need::list(requester).unwrap())
            .into_iter()
            .filter(|line| line.contains("offered it"))
            .count(),
        1,
        "the person sees the ask that names the copy"
    );
    // A week on, each request awaiting a person still hears from this Mac, whatever its pass
    // takes in, so none lapses while it waits.
    let week = window_end + need::STILL_SECS * MS;
    assert_eq!(pass_at(requester, week).unwrap().answered, 2);
    for need in need::list(requester).unwrap() {
        need::exception(&need)
            .unwrap()
            .message()
            .unwrap_or_else(|fault| panic!("{need:?}: {fault:?}"));
    }
}
