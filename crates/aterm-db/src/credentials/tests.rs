// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::{Lab, repair_commons};
use aterm_secrets::{
    commons::roster::Role,
    need::{Purpose, Reason, State, Want},
    text::Audience,
};

const SEED: [u8; 32] = [5; 32];
const NETWORK: [u8; 16] = [9; 16];
const A: Actor = [1; 48];
const B: Actor = [2; 48];
const C: Actor = [3; 48];
const DAY_MS: u64 = 86_400_000;

fn signer() -> [u8; 32] {
    aterm_secrets::envelope::signer_public(&SEED).unwrap()
}

fn context(now_ms: u64) -> Context {
    Context {
        policy: need::Policy::compiled(true),
        signer: Some(signer()),
        network: NETWORK,
        members: [A, B, C].into_iter().collect(),
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
    let key = request_key(actor, &nonce);
    (key, Record::new("request", &key, body))
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
        Some(("alab".into(), 7))
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

/// The whole path over a real scratch Commons: m2 lacks `notary`, which the vault holds in
/// m1's own section; m2's pass posts the request, m1's pass sweeps and its custodian pass
/// copies the entry into `[m2]` alone and publishes, and m2 applies it, which fulfils a need
/// no consumer checks for itself.
#[test]
fn a_request_reaches_the_custodian_and_the_vault_gives_it_to_the_requester_alone() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let m1 = Audience::Hosts(vec!["m1".into()]);
    doc.set_entry(&m1, "notary", b"apple").unwrap();
    doc.set_entry(&m1, "m1-only", b"stays").unwrap();
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
        text.entry(&m2, "notary")
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
    let done = &need::list(&lab.paths[1]).unwrap()[0];
    assert_eq!(
        done.state,
        State::Fulfilled,
        "the applied version gives this Mac the entry: {}",
        done.line
    );
    assert_eq!(
        pass_at(&lab.paths[1], now).unwrap().answered,
        1,
        "Fulfilled is written"
    );
    let book = pass_at(&lab.paths[1], now).unwrap();
    assert!(book.threads[0].fulfilled.is_some(), "{:?}", book.threads);
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
    doc.set_entry(&m1, "notary", b"apple").unwrap();
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
        reopened.doc.entry(&m2, "notary").is_some(),
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
