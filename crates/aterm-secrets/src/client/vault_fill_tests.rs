// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The log's byte bound as a budget by who authors (`membership_log/budgets.rs`, FL-145), where
//! this Mac authors: an automatic writer is refused first, at seven eighths of the bound, a
//! person's write next, at fifteen sixteenths, a credential's too, and a removal with the
//! re-key it makes due, an admission and a delete still land in the sixteenth the stops kept; an
//! automatic writer signs no `Put` over 64 KiB at any fill and is held to what its seat signed
//! in a day, a person never; and the vault text's door gives no fleet configuration name a new
//! value over 64 KiB while one the log already holds stays readable. Each test runs the shipped
//! commands in the two-Mac lab, with the stops the same shares of a bound a little past the
//! lab's log ([`crate::membership_log::RecordBudgets::within_bytes`]) or at their production
//! values, and the same write below its stop as its control.

use super::*;
use crate::membership_log::budgets::{AUTOMATIC_PUT_BUDGET, AUTOMATIC_PUT_LIMIT, author_stop};
use crate::membership_log::{Class, RecordBudgets};

const KIB: usize = 1024;
/// A large value a person writes under a name that is no configuration's: what fills a test's
/// log, each version replacing the one before.
const ARCHIVE: usize = 300 * KIB;
/// The inventory an automatic writer publishes: a little under the 64 KiB its `Put` may be.
const INVENTORY: usize = 60 * KIB;
const UPKEEP: Author<'static> = Author::Automatic("the repository inventory upkeep");

fn until() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

/// The vault text `m22` reads, with `name` in `audience` a value of `len` bytes of `fill`.
fn with_entry(
    m22: &Paths,
    audience: &Audience,
    name: &str,
    fill: u8,
    len: usize,
) -> (Admin, Document) {
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(audience, name, &vec![fill; len])
        .expect("the entry");
    (admin, doc)
}

/// A member's plain `set` of `name`.
fn member_entry(name: &str, value: &[u8]) -> log_entries::MemberEntry {
    log_entries::MemberEntry {
        name: name.to_owned(),
        value: SecretBytes::from(value.to_vec()),
        if_absent: false,
        anchor: log_entries::Anchor::Keep,
        fleet_reads_anchors: false,
        read_as: None,
        settles: None,
    }
}

/// `m22` publishes `versions` 300 KiB versions of one entry and a small one that replaces them,
/// so the log is large and what is current in it is small, as the lab's was: a removal's re-key,
/// which seals again only what is current, is small beside it.
fn fill_with_replaced_versions(m22: &Paths, versions: u8) {
    for version in 0..versions {
        let (admin, doc) = with_entry(
            m22,
            &Audience::Everyone,
            "lab_archive",
            b'a' + version,
            ARCHIVE,
        );
        admin_publish(m22, &admin, &doc).expect("an archive");
    }
    let (admin, doc) = with_entry(m22, &Audience::Everyone, "lab_archive", b'z', KIB);
    admin_publish(m22, &admin, &doc).expect("a small one replaces them");
}

/// A bound in which a log of `held` bytes is past the person's stop (fifteen sixteenths) with a
/// twentieth of the bound left below it.
const fn past_the_persons_stop(held: usize) -> usize {
    held / 19 * 20
}

/// The refusal of a person's write past the person's stop, as its command says it.
fn refused_as_a_persons_write(error: &Error) {
    assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
    assert!(
        error.to_string().starts_with(
            "this write was not signed: it is a person's write (a person's own Put, a \
             credential's too, or a name) of "
        ) && error.to_string().contains("a membership write ("),
        "{error}"
    );
}

/// A log past its automatic stop refuses an automatic writer's small entry, signing and
/// publishing nothing, and takes the same `Put` from a person; past the person's stop it refuses
/// a person's configuration write too, and an entry changed beside a delete (the person's write,
/// though the delete's fresh epoch seals it again with what stays), and still takes a peer's records, an admin's delete with
/// the reseal it owes (the entries that stay, sealed again, are no write of the person's) and an
/// admission, each in the sixteenth the stops kept free.
///
/// CONTROL, in the test: the entry the upkeep is refused lands as a person's write at the same
/// fill.
#[test]
fn vault_fill_refuses_an_automatic_writer_first_then_a_person_and_still_takes_membership() {
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"v1".as_slice()),
        (Audience::Everyone, "old_token", b"leaked".as_slice()),
    ]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let everyone = Audience::Everyone;
    fill_with_replaced_versions(m22, 6);

    // Past the automatic stop, short of the person's: the log is nine tenths of the bound.
    let held = log_of(m22).held_bytes();
    let bound = held / 9 * 10;
    let (ids, seq) = (convergence::ids(m22), admin_open(m22).unwrap().seq);
    RecordBudgets::within_bytes(bound, || {
        assert!(
            held > author_stop(Class::Automatic) && held + 16 * KIB < author_stop(Class::Person),
            "CONTROL: past the automatic stop alone"
        );
        let (admin, doc) = with_entry(m22, &everyone, "inventory.json", b'i', KIB);
        let error = admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
            .expect_err("the upkeep's inventory is refused");
        assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
        let text = error.to_string();
        assert!(
            text.starts_with(
                "the repository inventory upkeep's write was not signed: it is an automatic \
                 write (one no person started: the repository inventory's upkeep, the account \
                 broker's publish, a re-key nothing waits on) of "
            ) && text.contains("an automatic write stops at ")
                && text.contains(
                    "a person's write (a person's own Put, a credential's too, or a name) "
                )
                && text.contains("a membership write ("),
            "the refusal names the class, the fill and what still fits: {text}"
        );
        assert!(!text.contains("iiii"), "and no value: {text}");
        assert_eq!(convergence::ids(m22), ids, "nothing is signed");
        assert_eq!(
            admin_open(m22).unwrap().seq,
            seq,
            "and no version published"
        );

        let (admin, doc) = with_entry(m22, &everyone, "inventory.json", b'i', KIB);
        admin_publish(m22, &admin, &doc).expect("CONTROL: a person's same Put lands");
    });

    // Past the person's stop.
    let held = log_of(m22).held_bytes();
    let bound = past_the_persons_stop(held);
    RecordBudgets::within_bytes(bound, || {
        assert!(held > author_stop(Class::Person), "CONTROL: past it");
        // A peer's records come in at any fill.
        sync(m7, None).expect("m7 syncs what m22 signed");
        assert_eq!(
            convergence::ids(m7),
            convergence::ids(m22),
            "m7 holds every record of a log past both stops"
        );

        let ids = convergence::ids(m22);
        let (admin, doc) = with_entry(m22, &Audience::Everyone, "lab-routes.json", b'r', 400);
        let error = admin_publish(m22, &admin, &doc).expect_err("a configuration write");
        refused_as_a_persons_write(&error);
        assert_eq!(convergence::ids(m22), ids, "nothing is signed");
        // An entry changed beside a delete is the person's write, though the delete's fresh
        // epoch seals it with the reseal it owes: the batch is a person's write whole.
        let admin = admin_open(m22).expect("admin");
        let mut doc = admin.doc.clone();
        doc.set_entry(&everyone, "shared", b"v2").expect("shared");
        doc.unset_entry(&everyone, "old_token").expect("old_token");
        let error = admin_publish(m22, &admin, &doc).expect_err("a changed entry beside a delete");
        refused_as_a_persons_write(&error);
        assert_eq!(convergence::ids(m22), ids, "nothing is signed");

        // An admin's delete is membership with the reseal it owes: a fresh epoch, and every
        // entry that stays sealed again under it.
        let admin = admin_open(m22).expect("admin");
        let mut doc = admin.doc.clone();
        doc.unset_entry(&Audience::Everyone, "old_token")
            .expect("the entry");
        admin_publish(m22, &admin, &doc).expect("an admin's delete and its reseal");
        let signed = convergence::signed_since(m22, &ids);
        let count = |kind: Kind| signed.iter().filter(|r| r.kind() == kind).count();
        assert_eq!(
            (count(Kind::Delete), count(Kind::Epoch)),
            (1, 1),
            "the Delete under a fresh epoch"
        );
        assert!(
            count(Kind::Put) >= 2,
            "CONTROL: the entries that stay are sealed again: {}",
            count(Kind::Put)
        );
        assert_eq!(
            waiting::reads(m22, true, "old_token"),
            None,
            "the entry is gone"
        );
        assert!(waiting::reads(m22, true, "inventory.json").is_some());
        sync(m7, None).expect("m7 syncs the delete");

        // An admission.
        let (_m41, actor41, _) = let_in(&macs, "m41", "m41");
        assert!(log_of(m7).membership().contains(&actor41), "m41 is let in");
        let log = log_of(m7);
        assert!(
            log.held_bytes() > author_stop(Class::Person) && log.held_bytes() <= bound,
            "all of it past the person's stop, within the bound"
        );
    });
    macs.server.stop();
}

/// A credential is a person's write: past the person's stop a member's `set` of a 400-byte
/// credential is refused, as the command's answer, signing nothing, while a removal with the
/// re-key it makes due still lands in the room above that stop. That room is for removals: a
/// lab whose sign-ins rotate must not spend what takes a Mac out.
///
/// CONTROLS, in the test: below the person's stop the same `set` is signed; the removal signs
/// more bytes than the credential that was refused. With a changed `Put` classed as membership
/// (the rule this replaces, for a credential) the `set` is signed and the test fails.
#[test]
fn vault_fill_refuses_a_credential_past_the_persons_stop_and_a_removal_still_lands() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_m40, actor40, rec40) = let_in(&macs, "m40", "m40");
    sync(m22, None).expect("m22 syncs the admission");
    fill_with_replaced_versions(m22, 6);
    sync(m7, None).expect("m7 syncs");
    let set = |value: &[u8]| {
        log_entries::member_set(
            m7,
            &member_entry("gh_access_token", value),
            log_entries::OnBusy::Refuse(until()),
        )
    };
    let written = set(&[b's'; 400]).expect("CONTROL: below the stop the credential is signed");
    assert!(
        matches!(
            written,
            log_entries::MemberWritten::Posted | log_entries::MemberWritten::SignedHere
        ),
        "{written:?}"
    );

    let held = log_of(m7).held_bytes();
    let bound = past_the_persons_stop(held);
    RecordBudgets::within_bytes(bound, || {
        assert!(
            held > author_stop(Class::Person) && held + 2 * KIB < bound,
            "CONTROL: past the person's stop, with room below the bound for the credential"
        );
        let ids = convergence::ids(m7);
        let error = set(&[b't'; 400]).expect_err("a credential past the person's stop");
        refused_as_a_persons_write(&error);
        assert!(!error.to_string().contains("tttt"), "and no value: {error}");
        assert_eq!(convergence::ids(m7), ids, "nothing is signed");
        assert!(
            !log_entries::member_waits_here(m7, "gh_access_token").unwrap(),
            "and nothing is queued"
        );
        assert_eq!(
            waiting::reads(m7, true, "gh_access_token"),
            Some(vec![b's'; 400]),
            "the credential stays as it was"
        );

        // A removal, and the re-key it makes due.
        remove_member(m7, &join_code(&rec40), false).expect("m7 removes m40");
        let log = log_of(m7);
        assert!(!log.membership().contains(&actor40), "m40 is out");
        assert!(log.recipient_is_removed(&rec40));
        let signed = convergence::signed_since(m7, &ids);
        assert!(
            signed.iter().any(|record| record.kind() == Kind::Remove)
                && signed.iter().any(|record| record.kind() == Kind::Epoch),
            "the Remove and its rotation are signed"
        );
        assert!(
            log.rekey_dues().iter().all(|due| !due.rotation),
            "and no rotation stays due: {:?}",
            log.rekey_dues()
        );
        let removal: usize = signed.iter().map(|record| record.bytes().len()).sum();
        assert!(
            removal > 2 * KIB,
            "CONTROL: the removal signed more than the credential refused, so the class and \
             not the room decided: {removal}"
        );
        assert!(
            log.held_bytes() > author_stop(Class::Person) && log.held_bytes() <= bound,
            "all of it past the person's stop, within the bound"
        );
    });
    macs.server.stop();
}

/// An automatic writer's `Put` over 64 KiB is refused at any fill, in words that name the
/// writer, the size and the limit: here in a log far below every stop, from a seat that signed
/// nothing that day. Nothing is signed or published.
///
/// CONTROL, in the test: the same entry as a person's publish lands, and so does the writer's
/// own entry within the limit.
#[test]
fn vault_fill_refuses_an_automatic_writers_put_over_64_kib_at_any_fill() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m22 = &macs.m22;
    let (_, me, _) = member_key(m22);
    assert!(
        log_of(m22).held_bytes() + ARCHIVE < author_stop(Class::Automatic) / 8
            && log_of(m22).authored_bytes_since(&me, 0) < 16 * KIB,
        "CONTROL: far below the automatic stop and the seat's day"
    );
    let (ids, seq) = (convergence::ids(m22), admin_open(m22).unwrap().seq);
    let (admin, doc) = with_entry(m22, &Audience::Everyone, "lab_archive", b'q', ARCHIVE);
    let error = admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
        .expect_err("an automatic writer's 300 KiB Put");
    assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
    let text = error.to_string();
    assert!(
        text.starts_with(
            "the repository inventory upkeep's write was not signed: one Put of it is 300."
        ) && text.ends_with(
            " KiB, and an automatic writer signs no Put over 64 KiB, at any fill of the vault log"
        ),
        "{text}"
    );
    assert!(!text.contains("qqqq"), "and no value: {text}");
    assert_eq!(convergence::ids(m22), ids, "nothing is signed");
    assert_eq!(
        admin_open(m22).unwrap().seq,
        seq,
        "and no version published"
    );
    admin_publish(m22, &admin, &doc).expect("CONTROL: a person's same Put lands");
    let (admin, doc) = with_entry(m22, &Audience::Everyone, "inventory.json", b'i', INVENTORY);
    admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
        .expect("CONTROL: the writer's Put within the limit lands");
    let largest = convergence::signed_since(m22, &ids)
        .iter()
        .filter(|record| record.kind() == Kind::Put && record.actor() == &me)
        .map(|record| record.bytes().len())
        .filter(|bytes| *bytes < ARCHIVE)
        .max();
    assert!(
        largest.is_some_and(|bytes| bytes > INVENTORY && bytes <= AUTOMATIC_PUT_LIMIT),
        "CONTROL: it is a Put of nearly the limit: {largest:?}"
    );
    macs.server.stop();
}

/// An automatic writer is refused once its seat's `Put`s of the last 24 hours and its batch
/// pass 1 MiB, far below any class stop, signing and publishing nothing; a person's publish of
/// the same text from the same seat lands, so does a person's batch of more than 1 MiB of Puts
/// alone, and so does a member's write.
///
/// CONTROL, in the test: the writer's first inventories, within the budget, land.
#[test]
fn vault_fill_budgets_an_automatic_writer_and_never_a_persons_write() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_, me, _) = member_key(m22);
    let signed = |paths: &Paths| log_of(paths).authored_bytes_since(&me, 0);
    // An automatic writer publishes only what it drafted into this Mac's log, where it was
    // asked: from a view whose version the log is behind, its edit would go out with no records
    // and a mirror, which asks nothing, would sign it later.
    let (mut admin, doc) = with_entry(m22, &Audience::Everyone, "inventory.json", b'z', 64);
    let (covers, seq) = (admin.log_base, admin.seq);
    admin.log_base = log_entries::LogBase::Behind;
    let error = admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
        .expect_err("an automatic writer whose edit the log cannot take");
    assert!(
        error.to_string().starts_with(
            "the repository inventory upkeep's write was not published: this Mac's vault log \
             does not hold the version it edits yet"
        ),
        "{error}"
    );
    assert_ne!(
        error.kind(),
        crate::ErrorKind::MembershipCapacity,
        "no refusal by the fill: the writer offers it again"
    );
    assert_eq!(admin_open(m22).unwrap().seq, seq, "no version published");
    admin.log_base = covers;
    admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
        .expect("CONTROL: the same edit, drafted into the log, is published");
    let mut round = 0_u8;
    while signed(m22) + INVENTORY + KIB <= AUTOMATIC_PUT_BUDGET {
        let (admin, doc) = with_entry(
            m22,
            &Audience::Everyone,
            "inventory.json",
            b'a' + round,
            INVENTORY,
        );
        admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
            .unwrap_or_else(|error| panic!("CONTROL: inventory {round} is in budget: {error}"));
        round += 1;
    }
    let before = signed(m22);
    assert!(
        round >= 16 && before + INVENTORY > AUTOMATIC_PUT_BUDGET,
        "CONTROL: {round} inventories landed and one more passes the budget: {before}"
    );
    assert!(
        log_of(m22).held_bytes() + INVENTORY < author_stop(Class::Automatic) / 8,
        "CONTROL: far below the automatic stop"
    );

    let (ids, seq) = (convergence::ids(m22), admin_open(m22).unwrap().seq);
    let (admin, doc) = with_entry(m22, &Audience::Everyone, "inventory.json", b'Z', INVENTORY);
    let error = admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
        .expect_err("one inventory more in a day");
    assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
    let text = error.to_string();
    assert!(
        text.starts_with(
            "the repository inventory upkeep's write was not signed: this Mac's seat signed "
        ) && text.contains(" of Puts in the last 24 hours, and this one's ")
            && text.contains(
                " would pass the 1 MiB an automatic writer may sign in a day, which leaves it "
            )
            && text.ends_with("; a person's own write is not held to it"),
        "{text}"
    );
    assert_eq!(convergence::ids(m22), ids, "nothing is signed");
    assert_eq!(
        admin_open(m22).unwrap().seq,
        seq,
        "and no version published"
    );

    // The same text as a person's own publish, from the same seat.
    admin_publish(m22, &admin, &doc).expect("a person's write is never held to the budget");
    assert!(signed(m22) > before + INVENTORY, "it is signed");
    // Nor is a person's batch whose own Puts are more than an automatic writer's whole day.
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    for name in ["lab_archive_a", "lab_archive_b"] {
        doc.set_entry(&Audience::Everyone, name, &vec![b'p'; 600 * KIB])
            .expect("an archive");
    }
    let before = signed(m22);
    admin_publish(m22, &admin, &doc).expect("a person's batch of more than 1 MiB of Puts");
    assert!(
        signed(m22) > before + AUTOMATIC_PUT_BUDGET,
        "CONTROL: the batch alone is more than the budget"
    );
    // And a member's write on another Mac.
    sync(m7, None).expect("m7 syncs");
    let written = log_entries::member_set(
        m7,
        &member_entry("gh_access_token", b"token-v2"),
        log_entries::OnBusy::Refuse(until()),
    )
    .expect("a member's write");
    assert!(
        matches!(
            written,
            log_entries::MemberWritten::Posted | log_entries::MemberWritten::SignedHere
        ),
        "{written:?}"
    );
    // The seat stays past its budget: the smallest automatic write is still refused.
    let (admin, doc) = with_entry(m22, &Audience::Everyone, "inventory.json", b'e', 64);
    let error = admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
        .expect_err("past the budget");
    assert!(
        error
            .to_string()
            .ends_with("which leaves it 0 bytes; a person's own write is not held to it"),
        "{error}"
    );
    macs.server.stop();
}

/// `aterm secrets status`' `log:` line and the JSON under `log` are one reading of the log this
/// Mac holds: its bytes of the bound, every class open in a small log, what was signed in 24
/// hours with the two seats that signed most by their labels, and each kind's records. A Mac
/// with no vault has no line.
#[test]
fn vault_fill_status_reads_the_log_this_mac_holds() {
    use crate::membership_log::budgets::size_words;
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m7 = &macs.m7;
    let log = log_of(m7);
    let read = log_fill(m7).expect("the records read").expect("a log");
    assert_eq!(read.fill.bytes, log.held_bytes());
    let genesis = read.fill.by_kind[0];
    assert_eq!((genesis.0, genesis.1), (Kind::Genesis, 1));
    let signed = log.held_bytes() - genesis.2;
    let line = read.line();
    assert!(
        line.starts_with(&format!(
            "log: {} of 64 MiB · every class open · signed in 24 h: {} (",
            size_words(log.held_bytes()),
            size_words(signed)
        )),
        "{line}"
    );
    assert!(
        line.contains(") · full in: ") && line.ends_with(" d"),
        "{line}"
    );
    assert!(!line.contains("a Mac"), "each seat by its label: {line}");

    let json = read.json();
    let number = |value: &aterm_json::Value| value.as_u64().and_then(|n| usize::try_from(n).ok());
    assert_eq!(number(&json["bytes"]), Some(log.held_bytes()));
    assert_eq!(
        number(&json["bound"]),
        Some(crate::membership_log::MAX_RECORDS_STREAM)
    );
    assert_eq!(
        json["classes"].as_object().map(aterm_json::Map::len),
        Some(3),
        "the three classes and no other"
    );
    for (class, stop) in [("automatic", 56), ("person", 60), ("membership", 64)] {
        assert_eq!(
            number(&json["classes"][class]["stop"]),
            Some(stop * 1024 * KIB),
            "{class}"
        );
        assert_eq!(json["classes"][class]["open"].as_bool(), Some(true));
        assert_eq!(
            number(&json["classes"][class]["room"]),
            Some(stop * 1024 * KIB - log.held_bytes())
        );
    }
    assert_eq!(
        number(&json["by_kind"]["put"]["records"]),
        Some(
            log.records()
                .filter(|record| record.kind() == Kind::Put)
                .count()
        )
    );
    assert_eq!(number(&json["signed_24h"]["bytes"]), Some(signed));
    let seats = json["signed_24h"]["seats"].as_array().expect("seats");
    assert!(
        !seats.is_empty()
            && seats
                .iter()
                .all(|seat| seat["seat"].as_str().is_some_and(|label| label != "a Mac")),
        "{seats:?}"
    );
    assert!(json["full_in_ms"].as_u64().is_some_and(|ms| ms > 0));

    let nowhere = aterm_tempfile::tempdir().unwrap();
    assert_eq!(
        log_fill(&Paths::for_home(nowhere.path())).expect("no vault reads"),
        None,
        "a Mac with no vault holds no log"
    );
    macs.server.stop();
}

/// A write that waits for a removal's new key is asked its class where its command queues it,
/// and never again where the vault pass signs it. While m22 holds a `Remove` and not its
/// rotation: an automatic writer's write is refused, queueing nothing (a pass would sign it as a
/// person's, unasked); a person's write is queued below the person's stop and refused past it,
/// as the command's answer, a configuration entry and a credential alike; and a publish that
/// queues nothing more asks nothing again of the write that already waits. Once the rotation is
/// here the pass signs the write that waits, though the log is past the person's stop by then:
/// it was accepted.
///
/// CONTROLS, in the test: the writes refused past the stop are the size of the one queued below
/// it; the automatic writer's write lands once nothing waits. With the ask left in the pass
/// (FL-145 as first written) the pass fails, "this write was not signed", signs nothing and
/// keeps the write waiting.
#[test]
fn vault_fill_asks_a_waiting_write_where_it_is_queued_and_never_in_the_pass() {
    let lab = convergence::six_with(true);
    let m22 = &lab.all[0];
    fill_with_replaced_versions(m22, 3);
    convergence::carry(&lab.all, 0);
    let chain = waiting::held_remove(&lab);
    let waits = || admin_open(m22).expect("admin").waiting();

    // An automatic writer's write never waits.
    let seq = admin_open(m22).unwrap().seq;
    let (admin, doc) = with_entry(m22, &Audience::Everyone, "inventory.json", b'i', 64);
    let error = admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
        .expect_err("the upkeep's write would wait");
    assert!(
        error.to_string().starts_with(
            "the repository inventory upkeep's write was not signed: a removal's new key is \
             not here yet, and an automatic writer's write does not wait for one"
        ),
        "{error}"
    );
    assert_ne!(error.kind(), crate::ErrorKind::MembershipCapacity);
    assert_eq!(
        (waits(), admin_open(m22).unwrap().seq),
        (Vec::<String>::new(), seq),
        "nothing waits and no version is published"
    );

    // Below the person's stop a person's configuration write is queued.
    let (admin, doc) = with_entry(m22, &Audience::Everyone, "lab-routes.json", b'r', 400);
    let published = admin_publish(m22, &admin, &doc).expect("CONTROL: below the stop it waits");
    assert_eq!(published.waiting, ["lab-routes.json"]);

    let held = log_of(m22).held_bytes();
    let chain_bytes: usize = chain.iter().map(|record| record.bytes().len()).sum();
    let bound = past_the_persons_stop(held);
    RecordBudgets::within_bytes(bound, || {
        assert!(
            held > author_stop(Class::Person) && held + chain_bytes + 8 * KIB < bound,
            "CONTROL: past the person's stop, with room for the rotation and the write"
        );
        // Past it the same write is refused where the command queues it, and so is a
        // credential: both are a person's write.
        let (ids, seq) = (convergence::ids(m22), admin_open(m22).unwrap().seq);
        for name in ["lab-notes.json", "gh_access_token"] {
            let (admin, doc) = with_entry(m22, &Audience::Everyone, name, b'n', 400);
            let error = admin_publish(m22, &admin, &doc).expect_err("a person's write");
            refused_as_a_persons_write(&error);
            assert_eq!(
                (waits(), admin_open(m22).unwrap().seq, convergence::ids(m22)),
                (vec!["lab-routes.json".to_owned()], seq, ids.clone()),
                "{name} is not queued, published or signed"
            );
        }
        // A member's `set` is asked the same way.
        let error = log_entries::member_set(
            m22,
            &member_entry("team_cert_bundle", &[b'b'; 400]),
            log_entries::OnBusy::Refuse(until()),
        )
        .expect_err("a member's credential");
        refused_as_a_persons_write(&error);
        assert!(
            !log_entries::member_waits_here(m22, "team_cert_bundle").unwrap(),
            "it is not queued"
        );
        // A publish that queues nothing more asks nothing again of the write that waits: it
        // was asked when it was queued.
        let admin = admin_open(m22).expect("admin");
        let published = admin_publish(m22, &admin, &admin.doc.clone())
            .expect("the write that waits is not asked again");
        assert_eq!(published.waiting, ["lab-routes.json"]);

        // The rotation arrives: the pass signs what was accepted and fails nothing.
        save_records(m22, &encode_records(&chain)).unwrap();
        let signed = waiting::two_passes(m22);
        assert_eq!(
            signed.iter().filter(|r| r.kind() == Kind::Put).count(),
            1,
            "the waiting write is signed: {:?}",
            signed.iter().map(Record::kind).collect::<Vec<_>>()
        );
        assert_eq!(
            waiting::reads(m22, true, "lab-routes.json"),
            Some(vec![b'r'; 400])
        );
        assert_eq!(waits(), Vec::<String>::new(), "nothing waits");
    });
    // Nothing waits now: the automatic writer's write is taken.
    let (admin, doc) = with_entry(m22, &Audience::Everyone, "inventory.json", b'i', 64);
    admin_publish_conditional_until(m22, &admin, &doc, until(), UPKEEP)
        .expect("CONTROL: with the key here the upkeep's write lands");
    lab.macs.server.stop();
}

/// Writes that wait for a key are weighed together. Each is asked where its command queues it,
/// beside every write that already waits on this Mac and that it does not replace, since the
/// vault pass that signs them all once the key is here asks nothing: queued one at a time, each
/// alone within the person's stop, together they would pass it and spend the room kept for
/// removals. While m22 holds a `Remove` and not its rotation, in a log with room below the
/// person's stop for one 16 KiB write and not two, an admin's second write and a member's
/// credential `set` beside the first are refused, in words that name what waits, queueing
/// nothing; a write that replaces the one that waits is weighed in its place and queued.
///
/// CONTROLS, in the test: the first write is queued at that fill; the write that replaces it is
/// queued, by an admin's edit and by a member's `set`. With the writes that wait left out of the
/// ask (`Authoring::beside_waiting` never called, FL-145 as first written) the second write is
/// queued, and the test fails.
#[test]
fn vault_fill_weighs_a_waiting_write_beside_every_write_that_waits_with_it() {
    const WRITE: usize = 16 * KIB;
    let lab = convergence::six_with(true);
    let m22 = &lab.all[0];
    convergence::carry(&lab.all, 0);
    let chain = waiting::held_remove(&lab);
    let waits = || admin_open(m22).expect("admin").waiting();
    let held = log_of(m22).held_bytes();
    // The person's stop a write and a half past what the log holds.
    let bound = (held + WRITE + WRITE / 2) / 15 * 16 + 16;
    RecordBudgets::within_bytes(bound, || {
        let stop = author_stop(Class::Person);
        assert!(
            held + WRITE + KIB < stop && held + 2 * WRITE > stop,
            "CONTROL: room below the person's stop for one write and not two: {held} of {stop}"
        );
        let (admin, doc) = with_entry(m22, &Audience::Everyone, "first_token", b'f', WRITE);
        let published = admin_publish(m22, &admin, &doc).expect("CONTROL: the first write waits");
        assert_eq!(published.waiting, ["first_token"]);

        let (ids, seq) = (convergence::ids(m22), admin_open(m22).unwrap().seq);
        let (admin, doc) = with_entry(m22, &Audience::Everyone, "second_token", b's', WRITE);
        let error = admin_publish(m22, &admin, &doc).expect_err("a second write beside it");
        refused_as_a_persons_write(&error);
        assert!(
            error
                .to_string()
                .contains("and this Mac's writes waiting for a key "),
            "the refusal names what waits: {error}"
        );
        let error = log_entries::member_set(
            m22,
            &member_entry("third_token", &[b't'; WRITE]),
            log_entries::OnBusy::Refuse(until()),
        )
        .expect_err("a member's credential beside it");
        refused_as_a_persons_write(&error);
        assert!(
            !log_entries::member_waits_here(m22, "third_token").unwrap(),
            "it is not queued"
        );
        assert_eq!(
            (waits(), admin_open(m22).unwrap().seq, convergence::ids(m22)),
            (vec!["first_token".to_owned()], seq, ids),
            "nothing more waits, is published or signed"
        );

        // A write that replaces the one that waits is weighed in its place.
        let (admin, doc) = with_entry(m22, &Audience::Everyone, "first_token", b'g', WRITE);
        let published =
            admin_publish(m22, &admin, &doc).expect("CONTROL: an edit of the write that waits");
        assert_eq!(published.waiting, ["first_token"]);
        let written = log_entries::member_set(
            m22,
            &member_entry("first_token", &[b'h'; WRITE]),
            log_entries::OnBusy::Refuse(until()),
        )
        .expect("CONTROL: a member's set of the name that waits");
        assert!(
            matches!(written, log_entries::MemberWritten::Waiting(_)),
            "{written:?}"
        );
        assert_eq!(waits(), ["first_token"]);

        // The rotation arrives: the pass signs the one write that waits.
        save_records(m22, &encode_records(&chain)).unwrap();
        let signed = waiting::two_passes(m22);
        assert_eq!(
            signed.iter().filter(|r| r.kind() == Kind::Put).count(),
            1,
            "{:?}",
            signed.iter().map(Record::kind).collect::<Vec<_>>()
        );
        assert_eq!(
            waiting::reads(m22, true, "first_token"),
            Some(vec![b'h'; WRITE])
        );
    });
    lab.macs.server.stop();
}

/// A mirror is asked no class, and so a person's edit that would reach the log only by one is
/// refused past the person's stop. A version published with no records (its writer's log was
/// behind the version it edited, so the edit went out unstamped) is read by every Mac from its
/// lockers already; the pass that imports its text into the log past the person's stop signs
/// it, as before, where a refusal would fail that pass at every turn and leave the version with
/// no records for good. Past the stop, the same edit from a log behind is refused before it is
/// sent, in the fill's words, since no class could be asked of what the mirror would sign.
///
/// CONTROLS, in the test: before the pass the log does not hold the entry, and the same entry as
/// a person's publish drafted into the log is refused at that fill; below the stop the edit from
/// a log behind is published. Without the refusal of a person's publish that drafted nothing
/// (`client::publish_as_with_removal`), the edit from a log behind is published past the stop.
#[test]
fn vault_fill_asks_nothing_of_a_mirror() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m22 = &macs.m22;
    fill_with_replaced_versions(m22, 3);
    let held = log_of(m22).held_bytes();
    RecordBudgets::within_bytes(past_the_persons_stop(held), || {
        assert!(held > author_stop(Class::Person), "CONTROL: past it");
        let (mut admin, doc) = with_entry(m22, &Audience::Everyone, "lab-notes.json", b'n', 400);
        let error = admin_publish(m22, &admin, &doc).expect_err("CONTROL: drafted, it is asked");
        assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
        let seq = admin.seq;
        admin.log_base = log_entries::LogBase::Behind;
        let error = admin_publish(m22, &admin, &doc).expect_err("an edit from a log behind");
        assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
        assert!(
            error.to_string().starts_with(
                "this write was not published: this Mac's vault log does not hold the version \
                 it edits yet"
            ) && error.to_string().contains("past the "),
            "{error}"
        );
        assert_eq!(admin_open(m22).unwrap().seq, seq, "nothing is published");
    });
    // Below the stop the edit from a log behind goes out with no records.
    let (mut admin, doc) = with_entry(m22, &Audience::Everyone, "lab-routes.json", b'r', 400);
    admin.log_base = log_entries::LogBase::Behind;
    admin_publish(m22, &admin, &doc).expect("CONTROL: a version with no records");
    assert_eq!(
        waiting::reads(m22, true, "lab-routes.json"),
        None,
        "CONTROL: the log does not hold the entry"
    );
    RecordBudgets::within_bytes(past_the_persons_stop(held), || {
        admin_mirror_log(m22, &[]).expect("the pass that mirrors the version");
        assert_eq!(
            waiting::reads(m22, true, "lab-routes.json"),
            Some(vec![b'r'; 400]),
            "the mirror signed it past the person's stop"
        );
    });
    macs.server.stop();
}

/// The bound, in bytes, of what a coalesce of `[everyone]` signs as `paths` reads its log
/// ([`log_entries::rekey_bound`]).
fn coalesce_bytes(paths: &Paths) -> usize {
    let log = log_of(paths);
    let identity = read_identity(paths).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let (_, me, _) = member_key(paths);
    let flight = log_entries::Flight::of_batch(&log, &opened, &me);
    log_entries::rekey_bound(
        &log,
        &opened,
        &flight,
        &EpochAudience::Everyone,
        &log.current_recipients(&EpochAudience::Everyone),
        &log.removed_recipients(),
        None,
    )
    .bytes
}

/// The automatic stop holds no re-key a Mac waits on. Two `[everyone]` heads no `Remove` set
/// apart ([`stop_lab`]) are due a coalesce, and until it is signed no member seals an
/// `[everyone]` write: there is no epoch to seal under. In a log that the coalesce takes past
/// the automatic stop, short of the person's, the passes sign it, nothing stays due, and a
/// member's credential write is signed.
///
/// NEGATIVE CONTROL, run in the test ([`AUTOMATIC_STOP_HOLDS_WAITED`], FL-145 as first written):
/// held to the automatic stop, the coalesce is never signed, `[everyone]` stays due, and the same
/// write, 400 bytes in a log with room for a person's write, waits for a key no pass will mint.
#[test]
fn vault_fill_holds_no_coalesce_a_writer_waits_on_at_the_automatic_stop() {
    let run = |held_to_stop: bool| {
        let (lab, all, _, _, _) = stop_lab();
        let (held, cost) = (log_of(&all[0]).held_bytes(), coalesce_bytes(&all[0]));
        // The coalesce ends a little past seven eighths of the bound.
        let bound = (held + cost) / 7 * 8 - 64;
        let t0 = aterm_time::unix_millis().unwrap_or(0);
        let outcome = RecordBudgets::within_bytes(bound, || {
            assert!(
                held + cost > author_stop(Class::Automatic)
                    && held + cost + KIB < author_stop(Class::Person),
                "CONTROL: the coalesce passes the automatic stop alone: {held} and {cost} of \
                 {bound}"
            );
            AUTOMATIC_STOP_HOLDS_WAITED.with(|mutant| mutant.set(held_to_stop));
            let signed = passes(&all, t0, 14);
            AUTOMATIC_STOP_HOLDS_WAITED.with(|mutant| mutant.set(false));
            let written = log_entries::member_set(
                &all[2],
                &member_entry("gh_access_token", &[b't'; 400]),
                log_entries::OnBusy::Refuse(until()),
            )
            .expect("a member's write");
            let due = log_of(&all[0])
                .rekey_dues()
                .iter()
                .any(|due| due.audience == EpochAudience::Everyone);
            (convergence::epochs_by_audience(&signed), due, written)
        });
        lab.macs.server.stop();
        outcome
    };

    let (epochs, due, written) = run(false);
    assert_eq!(
        epochs,
        BTreeMap::from([(EpochAudience::Everyone, 1)]),
        "the coalesce is signed past the automatic stop"
    );
    assert!(!due, "and nothing stays due");
    assert!(
        matches!(
            written,
            log_entries::MemberWritten::Posted | log_entries::MemberWritten::SignedHere
        ),
        "the member's credential is signed: {written:?}"
    );

    let (epochs, due, written) = run(true);
    assert_eq!(
        (epochs, due),
        (BTreeMap::new(), true),
        "NEGATIVE CONTROL: held to the automatic stop, the coalesce is never signed"
    );
    assert_eq!(
        written,
        log_entries::MemberWritten::Waiting(log_entries::WaitFor::KeysUnsettled),
        "NEGATIVE CONTROL: and the credential waits on it"
    );
}

/// A heal is asked no class stop. In a log past the person's stop ([`stop_lab`]: m44 was let in
/// beside a re-key that does not wrap it), the passes still sign the `Wrap` that hands m44 the
/// head it lacks, so the Mac the log let in opens every newest `[everyone]` epoch. Held at a
/// class stop, as FL-145 first had it, the heal signs nothing there and the admission, which is
/// membership and landed, stays half done: m44 reads nothing written after it.
#[test]
fn vault_fill_heals_a_mac_let_in_past_the_persons_stop() {
    // The log is large before the heads part, so the heal's one `Wrap` fits in the sixteenth of
    // the bound past the person's stop.
    let (lab, all, rec44, _, _) = stop_lab_after(|lab| {
        fill_with_replaced_versions(&lab.all[0], 3);
        convergence::carry(&lab.all, 0);
    });
    let held = log_of(&all[0]).held_bytes();
    let wrap = Kind::Wrap.most_bytes();
    let bound = held + wrap + 64;
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    RecordBudgets::within_bytes(bound, || {
        assert!(
            held > author_stop(Class::Person),
            "CONTROL: the log is past the person's stop ({held} of {bound})"
        );
        assert!(
            !log_of(&all[0]).wraps_maximal_audience_to(&EpochAudience::Everyone, &rec44),
            "CONTROL: a newest epoch does not wrap m44"
        );
        let signed = passes(&all, t0, 14);
        assert!(
            signed.iter().any(|record| {
                matches!(record.body(), Body::Wrap { recipient, .. } if *recipient == rec44)
            }),
            "a Wrap to m44 is signed: {:?}",
            signed.iter().map(Record::kind).collect::<Vec<_>>()
        );
        assert!(
            log_of(&all[6]).wraps_maximal_audience_to(&EpochAudience::Everyone, &rec44),
            "m44 holds every newest epoch"
        );
    });
    lab.macs.server.stop();
}

/// Past the person's stop a `Name` is refused, in the fill's words, and the `Node` that binds
/// this Mac's fleet key lands: a Mac the log let in whose key no peer admits is half admitted.
///
/// CONTROL, in the test: below the stop the same `Name` is signed.
#[test]
fn vault_fill_past_the_persons_stop_refuses_a_name_and_binds_a_node_key() {
    use crate::membership_log::node_proof_message;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    fill_with_replaced_versions(m22, 3);
    sync(m7, None).expect("m7 syncs");
    let number = aterm_types::machine_name::Number::parse("m51").expect("a number");
    let pair = Ed25519KeyPair::from_seed_unchecked(&[0x51; 32]).unwrap();
    let key: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
    let (_, me, _) = member_key(m7);

    let held = log_of(m7).held_bytes();
    RecordBudgets::within_bytes(past_the_persons_stop(held), || {
        assert!(held > author_stop(Class::Person), "CONTROL: past it");
        let ids = convergence::ids(m7);
        let error = publish_local_chosen_name(m7, number).expect_err("a name");
        refused_as_a_persons_write(&error);
        assert_eq!(convergence::ids(m7), ids, "nothing is signed");

        let node = publish_local_node_key(m7, key, |network, actor| {
            pair.sign(&node_proof_message(network, actor))
                .as_ref()
                .try_into()
                .unwrap()
        })
        .expect("a node key past the person's stop")
        .expect("a Node record");
        assert_eq!(node.kind(), Kind::Node);
        assert_eq!(
            log_of(m7).membership().get(&me).and_then(|m| m.node_key),
            Some(key),
            "the log binds m7's node key"
        );
    });
    publish_local_chosen_name(m7, number)
        .expect("CONTROL: below the stop the name is signed")
        .expect("a Name record");
    macs.server.stop();
}

/// A publish that lets a Mac in is membership where it names who is in, and a person's write
/// in what else it changes. Past the person's stop an admin's approval of a Mac lands with the
/// swarm roster's row it changes (and the Commons control it shares, where the vault has one),
/// where the same roster change alone is refused; an approval that carries a credential the
/// person changed is refused whole, signing nothing, as the credential alone is, so no write
/// rides an admission into the room kept for removals.
///
/// CONTROLS, in the test: the roster change and the credential, each alone, are refused at that
/// fill. With the approval's publish asked no class (`approved.is_empty().then_some(author)`, as
/// FL-145 first had it) the credential lands with it; with the roster's `Put` its author's write
/// in an approval too (no [`log_entries::PutNotes`] `admits`), the approval with its row is
/// refused.
#[test]
fn vault_fill_takes_an_admission_with_its_roster_row_and_refuses_a_credential_beside_it() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m22 = &macs.m22;
    crate::client::admin_seed_swarm_roster(m22, &[]).expect("roster");
    fill_with_replaced_versions(m22, 3);
    let s = Paths::for_home(&macs.root.path().join("s"));
    let recipient = machine_identity(&s).expect("identity").recipient();
    // The roster as it names `s`: a row's change, here a line a reader skips.
    let roster_row = |doc: &mut Document| {
        let roster = doc
            .entry(&Audience::Everyone, crate::SWARM_ROSTER_ENTRY)
            .expect("CONTROL: the vault holds a roster")
            .value
            .as_slice()
            .to_vec();
        let mut row = roster;
        row.extend_from_slice(b"\n# s is let in\n");
        doc.set_entry(&Audience::Everyone, crate::SWARM_ROSTER_ENTRY, &row)
            .expect("the roster");
    };
    let held = log_of(m22).held_bytes();
    RecordBudgets::within_bytes(past_the_persons_stop(held), || {
        assert!(held > author_stop(Class::Person), "CONTROL: past it");
        let ids = convergence::ids(m22);
        let (admin, doc) = with_entry(m22, &Audience::Everyone, "welcome_note", b'w', 400);
        let error = admin_publish(m22, &admin, &doc).expect_err("CONTROL: the credential alone");
        refused_as_a_persons_write(&error);
        let admin = admin_open(m22).expect("admin");
        let mut doc = admin.doc.clone();
        roster_row(&mut doc);
        let error = admin_publish(m22, &admin, &doc).expect_err("CONTROL: the roster alone");
        refused_as_a_persons_write(&error);
        assert_eq!(convergence::ids(m22), ids, "nothing is signed");

        // An approval that carries the credential is refused whole.
        let (admin, mut doc) = with_entry(m22, &Audience::Everyone, "welcome_note", b'w', 400);
        doc.add_machine("s", &recipient.to_bech32()).expect("s");
        let error = admin_publish_approved(m22, &admin, &doc, &[recipient.to_bech32()])
            .expect_err("an approval with a credential beside it");
        refused_as_a_persons_write(&error);
        assert_eq!(convergence::ids(m22), ids, "nothing is signed");
        assert_eq!(waiting::reads(m22, true, "welcome_note"), None);

        // The approval with the roster's row lands.
        let admin = admin_open(m22).expect("admin");
        let mut doc = admin.doc.clone();
        roster_row(&mut doc);
        doc.add_machine("s", &recipient.to_bech32()).expect("s");
        admin_publish_approved(m22, &admin, &doc, &[recipient.to_bech32()])
            .expect("the approval lands with the roster row it changes");
        let signed = convergence::signed_since(m22, &ids);
        assert!(
            signed.iter().any(|record| {
                matches!(record.body(), Body::Wrap { recipient: to, .. } if *to == recipient)
            }),
            "a key is wrapped to the Mac let in: {:?}",
            signed.iter().map(Record::kind).collect::<Vec<_>>()
        );
        assert!(
            waiting::reads(m22, true, crate::SWARM_ROSTER_ENTRY)
                .is_some_and(|roster| roster.ends_with(b"# s is let in\n")),
            "and the roster's row is written"
        );
    });
    macs.server.stop();
}

/// The steward's tail of a removal is membership, as the removal is. After a member's plain
/// removal of m40, in a log past the person's stop, the steward's pass that completes it (m40's
/// label and node denied on the swarm roster, which a revoke puts in its own batch) lands, where
/// the same roster change as a person's edit is refused. Held to a person's stop, the removed
/// Mac's swarm label would never be denied while the log stays past it.
///
/// CONTROL, in the test: the roster change as an admin's edit is refused at that fill. With the
/// steward's publish asked as a person's edit (`admin_publish` in `steward_finish_revokes`, as
/// FL-145 first had it) the pass fails, "this write was not signed: it is a person's write".
#[test]
fn vault_fill_takes_the_stewards_tail_of_a_removal_past_the_persons_stop() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        b"servers = []\n",
    )
    .expect("policy");
    admin_publish(m22, &admin, &doc).expect("publish");
    crate::client::admin_seed_swarm_roster(m22, &[]).expect("roster");
    fill_with_replaced_versions(m22, 3);
    sync(m7, None).expect("m7 syncs");
    let (paths40, request40, allowed40) = asker(macs.root.path(), m7, "m40", "m40");
    let (bundle, logged) = hand(m7, &request40, &allowed40, true);
    assert!(logged, "CONTROL: m7 hands the log path");
    write_from_log_bundle(&paths40, &bundle).expect("m40 writes");
    post_records_to_servers(m7).expect("m7 posts");
    let label = log_of(m7)
        .membership()
        .get(&request40.binding().actor())
        .expect("m40 is in")
        .label
        .clone();
    remove_member(m7, &label, false).expect("m7's plain removal of m40");
    sync(m22, None).expect("m22 syncs the removal");
    let held = log_of(m22).held_bytes();
    let bound = past_the_persons_stop(held);
    RecordBudgets::within_bytes(bound, || {
        assert!(held > author_stop(Class::Person), "CONTROL: past it");
        let roster = |doc: &Document| {
            crate::swarm_roster::SwarmRoster::from_document(doc)
                .expect("roster")
                .expect("a roster")
        };
        // The same change as an admin's edit is a person's write.
        let admin = admin_open(m22).expect("admin");
        let mut edited = roster(&admin.doc);
        edited.deny_labels.push(label.clone());
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            edited.to_toml().as_bytes(),
        )
        .expect("the roster");
        let error = admin_publish(m22, &admin, &doc).expect_err("CONTROL: an admin's edit");
        refused_as_a_persons_write(&error);

        let signer22 = config(m22)
            .expect("config")
            .expect("a vault")
            .signer
            .expect("a pin");
        envelope::TEST_RETIRED.with(|cell| {
            *cell.borrow_mut() = Some(crate::envelope::Retired {
                keys: Vec::new(),
                current: Some(signer22),
            });
        });
        let finished = steward_finish_revokes(m22);
        envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        assert!(
            matches!(finished, Ok(StewardRevoke::Done(_))),
            "the steward completes the removal past the person's stop: {finished:?}"
        );
        assert!(
            roster(&admin_open(m22).expect("admin").published_doc).is_label_denied(&label),
            "{label} is denied"
        );
        let log = log_of(m22);
        assert!(
            log.held_bytes() > author_stop(Class::Person) && log.held_bytes() <= bound,
            "all of it past the person's stop, within the bound"
        );
    });
    macs.server.stop();
}

/// An entry of a host list this Mac opens no key of is written against the text the edit
/// started from, and an entry the edit changed there is the person's write all the same: past
/// the person's stop an admin's edit of `m7-cred` in `[m7]`, which m22 holds no key of, is
/// refused, signing nothing.
///
/// CONTROL, in the test: m22 holds no `[m7]` key; below the stop the same edit is signed. With
/// every entry written there read as one the edit left alone (`same_entry` against the base
/// always true), the batch holds nothing the person changed and lands as membership.
#[test]
fn vault_fill_refuses_a_changed_entry_of_a_host_list_this_mac_holds_no_key_of() {
    let ring = Audience::Hosts(vec!["m7".to_owned()]);
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"v1".as_slice()),
        (ring.clone(), "m7-cred", b"ring-v1".as_slice()),
    ]);
    let m22 = &macs.m22;
    let admin = admin_open(m22).expect("admin");
    admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    fill_with_replaced_versions(m22, 3);
    let identity = read_identity(m22).unwrap().unwrap();
    let opened = log_of(m22)
        .open_audience_entries(&identity)
        .unwrap()
        .unwrap();
    assert!(
        !opened.by_audience.contains_key(&ring),
        "CONTROL: m22 holds no [m7] key"
    );
    let held = log_of(m22).held_bytes();
    RecordBudgets::within_bytes(past_the_persons_stop(held), || {
        assert!(held > author_stop(Class::Person), "CONTROL: past it");
        let ids = convergence::ids(m22);
        let (admin, doc) = with_entry(m22, &ring, "m7-cred", b'r', 400);
        let error = admin_publish(m22, &admin, &doc).expect_err("a changed entry of [m7]");
        refused_as_a_persons_write(&error);
        assert_eq!(convergence::ids(m22), ids, "nothing is signed");
    });
    let ids = convergence::ids(m22);
    let (admin, doc) = with_entry(m22, &ring, "m7-cred", b'r', 400);
    admin_publish(m22, &admin, &doc).expect("CONTROL: below the stop it is signed");
    assert!(
        convergence::signed_since(m22, &ids)
            .iter()
            .any(|record| record.kind() == Kind::Put),
        "CONTROL: a Put is signed"
    );
    macs.server.stop();
}

/// A person's edit that only changes who opens an audience is membership: past the person's
/// stop an admin's publish that lists a Mac, and so wraps `[everyone]`'s key to it, lands, where
/// an edit that changes an entry is refused. Its records are an `Epoch`, `Wrap`s and the entries
/// sealed again for the new list, none of which the person changed, so the batch holds nothing
/// classed and its author says what its keys are for.
///
/// CONTROL, in the test: an edit that changes an entry is refused at that fill. With the batch
/// left as its records class it (no `keys_as`), the publish fails: "this write was not signed:
/// it is a person's write".
#[test]
fn vault_fill_takes_an_edit_that_only_lists_a_mac_past_the_persons_stop() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m22 = &macs.m22;
    fill_with_replaced_versions(m22, 3);
    let s = Paths::for_home(&macs.root.path().join("s"));
    let recipient = machine_identity(&s).expect("identity").recipient();
    let held = log_of(m22).held_bytes();
    RecordBudgets::within_bytes(past_the_persons_stop(held), || {
        assert!(held > author_stop(Class::Person), "CONTROL: past it");
        let (admin, doc) = with_entry(m22, &Audience::Everyone, "welcome_note", b'w', 400);
        let error = admin_publish(m22, &admin, &doc).expect_err("CONTROL: a changed entry");
        refused_as_a_persons_write(&error);

        let ids = convergence::ids(m22);
        let admin = admin_open(m22).expect("admin");
        let mut doc = admin.doc.clone();
        doc.add_machine("s", &recipient.to_bech32()).expect("s");
        admin_publish(m22, &admin, &doc).expect("an edit that only lists a Mac");
        let signed = convergence::signed_since(m22, &ids);
        assert!(
            signed.iter().any(|record| {
                matches!(record.body(), Body::Wrap { recipient: to, .. } if *to == recipient)
            }),
            "a key is wrapped to the Mac listed: {:?}",
            signed.iter().map(Record::kind).collect::<Vec<_>>()
        );
        assert!(
            signed
                .iter()
                .all(|record| matches!(record.kind(), Kind::Epoch | Kind::Wrap | Kind::Put)),
            "CONTROL: the batch holds no kind that is membership by itself: {:?}",
            signed.iter().map(Record::kind).collect::<Vec<_>>()
        );
    });
    macs.server.stop();
}

/// The vault text's door, through the shipped publish: no author gives a fleet configuration
/// name a new value over 64 KiB, and a 300 KiB value the log already holds (an older build
/// wrote it) stays readable on every Mac, is sealed again whole when a delete beside it owes a
/// reseal, survives an unrelated edit, and can be deleted.
///
/// CONTROL, in the test: the same name takes a 64 KiB value.
#[test]
fn vault_fill_door_keeps_a_large_configuration_value_the_log_holds_readable() {
    const NAME: &str = "lab-inventory.json";
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"v1".as_slice()),
        (Audience::Everyone, "old_token", b"leaked".as_slice()),
    ]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let everyone = Audience::Everyone;
    let large = vec![b'L'; 300 * KIB];
    // As a build before the door wrote it: the value goes into the text with no door asked.
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.hold_entry_with(&everyone, NAME, &large, false, None)
        .expect("held");
    admin_publish(m22, &admin, &doc).expect("the large value, as an older build published it");

    // It reads, in the admin's text and on another Mac.
    let admin = admin_open(m22).expect("admin");
    assert_eq!(
        admin.doc.entry(&everyone, NAME).map(|e| e.value.as_slice()),
        Some(large.as_slice()),
        "the admin's text shows it"
    );
    sync(m7, None).expect("m7 syncs");
    assert_eq!(waiting::reads(m7, true, NAME), Some(large.clone()));

    // No author changes it to another large value: the refusal is the text's, in words.
    let mut doc = admin.doc.clone();
    let error = doc
        .set_entry(&everyone, NAME, &vec![b'M'; 300 * KIB])
        .expect_err("a changed 300 KiB value");
    assert!(
        error.to_string().starts_with(
            "`lab-inventory.json` was not set: the value it would take is 300 KiB, and a \
             fleet configuration value over 64 KiB is not set or changed"
        ),
        "{error}"
    );
    // The value it holds, set again as it is, is no change: the door lets it stand.
    doc.set_entry(&everyone, NAME, &large)
        .expect("the value the log holds, set again unchanged");
    // An unrelated edit, and a delete beside it that owes a reseal: the large value is sealed
    // again whole, by a person and with no door asked of what the person did not change.
    let ids = convergence::ids(m22);
    doc.set_entry(&everyone, "shared", b"v2").expect("shared");
    doc.unset_entry(&everyone, "old_token").expect("old_token");
    admin_publish(m22, &admin, &doc).expect("an unrelated edit beside the large value");
    let signed = convergence::signed_since(m22, &ids);
    assert!(
        signed
            .iter()
            .any(|record| record.kind() == Kind::Put && record.bytes().len() > large.len()),
        "CONTROL: the reseal signed the large value again: {:?}",
        signed
            .iter()
            .map(|record| (record.kind(), record.bytes().len()))
            .collect::<Vec<_>>()
    );
    sync(m7, None).expect("m7 syncs");
    assert_eq!(
        waiting::reads(m7, true, NAME),
        Some(large),
        "it survives the edit"
    );
    assert_eq!(waiting::reads(m7, true, "shared"), Some(b"v2".to_vec()));

    // A 64 KiB value lands, and the entry can be deleted.
    let (admin, doc) = with_entry(m22, &everyone, NAME, b's', 64 * KIB);
    admin_publish(m22, &admin, &doc).expect("CONTROL: a 64 KiB value lands");
    assert_eq!(waiting::reads(m22, true, NAME), Some(vec![b's'; 64 * KIB]));
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    assert!(doc.unset_entry(&everyone, NAME).expect("the entry"));
    admin_publish(m22, &admin, &doc).expect("a delete of it");
    assert_eq!(waiting::reads(m22, true, NAME), None, "it is gone");
    macs.server.stop();
}

/// What is no author's change is read into a text with no door. A 300 KiB fleet configuration
/// value the log holds is written into a text that lacks it (the log's overlay, which the pass
/// that mirrors the log runs), and one that waits for a removal's new key, queued by a build
/// before the door, is shown in the admin's text. Held to the door, either would fail the read
/// that shows it, and the vault would not open for its admin.
///
/// CONTROL, in the test: the same value set by an author into the same text is refused.
#[test]
fn vault_fill_door_reads_a_large_value_the_log_or_a_waiting_write_holds() {
    const NAME: &str = "lab-inventory.json";
    let lab = convergence::six_with(true);
    let m22 = &lab.all[0];
    let everyone = Audience::Everyone;
    let large = vec![b'L'; 300 * KIB];
    let admin = admin_open(m22).expect("admin");
    let mut before = admin.doc.clone();
    let mut doc = admin.doc.clone();
    doc.hold_entry_with(&everyone, NAME, &large, false, None)
        .expect("held");
    admin_publish(m22, &admin, &doc).expect("the large value, as an older build published it");
    assert!(
        before.entry(&everyone, NAME).is_none()
            && before.clone().set_entry(&everyone, NAME, &large).is_err(),
        "CONTROL: the text before lacks it, and an author's set of it is refused"
    );
    let identity = read_identity(m22).unwrap().unwrap();
    log_entries::overlay_log_onto_document(&log_of(m22), &identity, &mut before, None)
        .expect("the log's value is read into the text");
    assert_eq!(
        before.entry(&everyone, NAME).map(|e| e.value.as_slice()),
        Some(large.as_slice()),
        "the text shows what the log holds"
    );

    // A write that waits: m22 holds a Remove and not its rotation.
    convergence::carry(&lab.all, 0);
    let _chain = waiting::held_remove(&lab);
    let waits = vec![b'W'; 300 * KIB];
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.hold_entry_with(&everyone, NAME, &waits, false, None)
        .expect("held");
    let published = admin_publish(m22, &admin, &doc).expect("it is queued");
    assert_eq!(published.waiting, [NAME], "CONTROL: the write waits");
    let admin = admin_open(m22).expect("the admin's text opens with a large write waiting");
    assert_eq!(
        admin.doc.entry(&everyone, NAME).map(|e| e.value.as_slice()),
        Some(waits.as_slice()),
        "and shows it"
    );
    lab.macs.server.stop();
}
