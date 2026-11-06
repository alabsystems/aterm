// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One-mesh W3 (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11): a waiting write is seen, can be
//! taken back, and is never dropped unseen. The tests of `log_entries/waiting.rs` that need the
//! removal lab ([`convergence::six_with`]), declared beside the removal tests for its fixtures.

use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// m7 removes m42 and keeps its batch to itself; m22 takes only the `Remove`, so a rotation is
/// due on m22 and its `[everyone]` writes wait. Returns m7's batch.
fn held_remove(lab: &convergence::Six) -> Vec<Record> {
    let (m22, m7) = (&lab.all[0], &lab.all[1]);
    let base = convergence::ids(m7);
    test_remove_unposted(m7, &join_code(&lab.recipients[4])).expect("m7 removes m42");
    let chain = convergence::signed_since(m7, &base);
    save_records(m22, &encode_records(&chain[..1])).unwrap();
    assert!(
        log_of(m22)
            .rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone && due.rotation),
        "CONTROL: m22 holds the Remove, not its rotation"
    );
    chain
}

/// Open the vault on `paths` as an admin, apply `edit` to its text and publish it.
fn publish_edit(paths: &Paths, edit: impl FnOnce(&mut Document)) -> Published {
    let admin = admin_open(paths).expect("an admin");
    let mut doc = admin.doc.clone();
    edit(&mut doc);
    admin_publish(paths, &admin, &doc).expect("the publish")
}

/// Set `[everyone]`'s `name` to `value` in `doc`.
fn set(doc: &mut Document, name: &str, value: &[u8]) {
    doc.set_entry(&Audience::Everyone, name, value)
        .expect("edit");
}

/// `[everyone]`'s `name` as the admin view shows it.
fn shows(admin: &Admin, name: &str) -> Option<Vec<u8>> {
    admin
        .doc
        .entry(&Audience::Everyone, name)
        .map(|entry| entry.value.as_slice().to_vec())
}

/// `name` in `[everyone]`, or in any host list, as the Mac at `paths` opens its log.
fn reads(paths: &Paths, everyone: bool, name: &str) -> Option<Vec<u8>> {
    let identity = read_identity(paths).unwrap().unwrap();
    reads_in(&log_of(paths), &identity, everyone, name)
}

/// [`reads`] over `log`, as `identity` opens it.
fn reads_in(log: &Log, identity: &Identity, everyone: bool, name: &str) -> Option<Vec<u8>> {
    let opened = log.open_audience_entries(identity).unwrap()?;
    opened
        .by_audience
        .iter()
        .filter(|(audience, _)| (**audience == Audience::Everyone) == everyone)
        .find_map(|(_, entries)| entries.get(name))
        .map(|entry| entry.value.as_slice().to_vec())
}

/// Two vault passes on m22, a step apart from now, and what m22 signed in them.
fn two_passes(m22: &Paths) -> Vec<Record> {
    let before = convergence::ids(m22);
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    vault_pass_at(m22, t0).unwrap();
    vault_pass_at(m22, t0 + log_entries::GAP_STEP_MS).unwrap();
    convergence::signed_since(m22, &before)
}

/// What [`take_back`] saw.
struct TakeBack {
    lab: convergence::Six,
    /// What the first publish said waits.
    first: Vec<String>,
    /// What the view showed waiting after it, and its value of `shared`.
    shown: (Vec<String>, Option<Vec<u8>>),
    /// What the view showed waiting after the change, and its values of `shared` and `fresh`.
    changed: (Vec<String>, Option<Vec<u8>>, Option<Vec<u8>>),
    /// What the view showed waiting after the take-back.
    after: Vec<String>,
    /// What m22's passes signed once the rotation arrived.
    signed: Vec<Record>,
}

/// m22, which holds m7's `Remove` of m42 and not its rotation ([`held_remove`]), publishes
/// `shared` = v2, which waits. It opens the vault again, changes `shared` to v3 and writes
/// `fresh`, a name the log does not hold; then opens it again and takes both back: `shared` to
/// the log's v1, `fresh` removed. Then the rotation arrives, m22's passes run, and every Mac takes
/// its records. `control` sets [`log_entries::VIEW_WITHOUT_WAITING`] for the edits.
fn take_back(control: bool) -> TakeBack {
    let lab = convergence::six_with(true);
    let chain = held_remove(&lab);
    let m22 = &lab.all[0];
    log_entries::VIEW_WITHOUT_WAITING.with(|cell| cell.set(control));
    let first = publish_edit(m22, |doc| set(doc, "shared", b"v2")).waiting;
    let admin = admin_open(m22).expect("an admin");
    let shown = (admin.waiting(), shows(&admin, "shared"));
    let mut doc = admin.doc.clone();
    set(&mut doc, "shared", b"v3");
    set(&mut doc, "fresh", b"f1");
    admin_publish(m22, &admin, &doc).expect("the change");
    let admin = admin_open(m22).expect("an admin");
    let changed = (
        admin.waiting(),
        shows(&admin, "shared"),
        shows(&admin, "fresh"),
    );
    let mut doc = admin.doc.clone();
    set(&mut doc, "shared", b"v1");
    let _ = doc.unset_entry(&Audience::Everyone, "fresh");
    admin_publish(m22, &admin, &doc).expect("the take-back");
    let after = admin_open(m22).expect("an admin").waiting();
    log_entries::VIEW_WITHOUT_WAITING.with(|cell| cell.set(false));

    save_records(m22, &encode_records(&chain)).unwrap();
    let signed = two_passes(m22);
    convergence::carry(&lab.all, 0);
    TakeBack {
        lab,
        first,
        shown,
        changed,
        after,
        signed,
    }
}

/// One-mesh W3 ([`take_back`]): the admin view shows this Mac's `[everyone]` writes that wait for
/// a removal's new key over the log's values, marked as waiting, and a later save drafts against
/// that view. A change of a waiting write replaces it, and setting it back to the log's value, or
/// removing a name only the waiting write added, takes it back: once the rotation arrives the
/// passes sign nothing, nothing waits, and every Mac reads `shared` = v1 and no `fresh`.
///
/// NEGATIVE CONTROL: [`log_entries::VIEW_WITHOUT_WAITING`] (the view shows the log alone, as
/// before W3), run in the test: the view shows nothing waiting, the take-back is no change, and
/// the passes sign the change the person took back, so every Mac reads `shared` = v3, as the
/// CONTROL asserts say; with the switch set for the first run too, the test fails "the view
/// shows the waiting write".
#[test]
fn a_waiting_write_shows_in_the_view_and_a_save_changes_it_or_takes_it_back() {
    let run = take_back(false);
    let m22 = &run.lab.all[0];
    assert_eq!(
        run.first,
        vec!["shared".to_owned()],
        "the publish says what waits"
    );
    assert_eq!(
        run.shown,
        (vec!["shared".to_owned()], Some(b"v2".to_vec())),
        "the view shows the waiting write"
    );
    assert_eq!(
        run.changed,
        (
            vec!["fresh".to_owned(), "shared".to_owned()],
            Some(b"v3".to_vec()),
            Some(b"f1".to_vec())
        ),
        "the view shows the change"
    );
    assert!(
        run.after.is_empty(),
        "nothing waits once both are taken back"
    );
    assert_eq!(
        everyone_puts(&run.signed),
        0,
        "nothing is signed for a write taken back"
    );
    assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
    assert!(!log_entries::waiting_pending(m22), "nothing is pending");
    for paths in &run.lab.all[..4] {
        assert_eq!(
            reads(paths, true, "shared"),
            Some(b"v1".to_vec()),
            "the take-back holds"
        );
        assert_eq!(reads(paths, true, "fresh"), None, "fresh is taken back");
    }
    run.lab.macs.server.stop();

    let control = take_back(true);
    assert!(
        control.shown.0.is_empty(),
        "CONTROL: the view without waiting writes shows none"
    );
    assert_eq!(
        reads(&control.lab.all[1], true, "shared"),
        Some(b"v3".to_vec()),
        "CONTROL: the take-back the person could not see is lost"
    );
    control.lab.macs.server.stop();
}

/// What [`overtaken`] saw: the lab, what m22's passes signed, m22's journal after them, and m7's
/// `Put` of `shared` = v9.
struct Overtook {
    lab: convergence::Six,
    signed: Vec<Record>,
    journal: Vec<Overtaken>,
    put: RecordId,
}

/// m22 publishes `shared` = v2 while a rotation is due ([`held_remove`]), and it waits; m7, the
/// remover, which holds the rotation, publishes `shared` = v9. m22 takes m7's records and its
/// passes run, `control` setting [`log_entries::OVERTAKEN_UNRECORDED`] for them.
fn overtaken(control: bool) -> Overtook {
    let lab = convergence::six_with(true);
    held_remove(&lab);
    let (m22, m7) = (&lab.all[0], &lab.all[1]);
    publish_edit(m22, |doc| set(doc, "shared", b"v2"));
    let base = convergence::ids(m7);
    publish_edit(m7, |doc| set(doc, "shared", b"v9"));
    let put = convergence::signed_since(m7, &base)
        .iter()
        .find(|record| {
            matches!(
                record.body(),
                Body::Put {
                    audience: EpochAudience::Everyone,
                    ..
                }
            )
        })
        .map(|record| *record.id())
        .expect("CONTROL: m7 signs its write at once");
    save_records(m22, &load_records(m7).unwrap().encode()).unwrap();
    log_entries::OVERTAKEN_UNRECORDED.with(|cell| cell.set(control));
    let signed = two_passes(m22);
    log_entries::OVERTAKEN_UNRECORDED.with(|cell| cell.set(false));
    let journal = log_entries::read_overtaken(m22).unwrap();
    Overtook {
        lab,
        signed,
        journal,
        put,
    }
}

/// One-mesh W3 ([`overtaken`]): a waiting write a later change overtook is still dropped, so the
/// value the edit never saw stands, and it leaves a record in m22's journal: the entry, the op
/// that holds it now (m7's `Put`, its signer's label and when) and when the write was dropped,
/// never a value. The admin view shows the record once: once shown, a later view shows it no
/// more, and the journal keeps it.
///
/// NEGATIVE CONTROL: [`log_entries::OVERTAKEN_UNRECORDED`] (the write dropped with no record, as
/// before W3), run in the test: the journal is empty, as the CONTROL assert says; with the switch
/// set for the first run too, the test fails "the dropped write leaves one record".
#[test]
fn an_overtaken_waiting_write_is_dropped_with_a_record_the_view_shows_once() {
    let run = overtaken(false);
    let m22 = &run.lab.all[0];
    assert_eq!(
        everyone_puts(&run.signed),
        0,
        "the overtaken write is not signed"
    );
    assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
    assert_eq!(run.journal.len(), 1, "the dropped write leaves one record");
    let record = &run.journal[0];
    let label = log_of(m22)
        .membership()
        .label(&run.lab.actors[1])
        .unwrap_or("a Mac")
        .to_owned();
    assert_eq!(record.name, "shared");
    assert!(!record.shown, "no view showed it yet");
    let by = record.by.as_ref().expect("the record names the change");
    assert_eq!(
        (by.record, by.label.as_str(), by.deleted),
        (run.put, label.as_str(), false),
        "the record names m7's Put"
    );
    let line = record.line(|ms| format!("at {ms}"));
    assert!(
        line.contains("shared") && line.contains(&label),
        "the line names the entry and the change: {line}"
    );
    assert!(
        !line.contains("v9") && !line.contains("v2"),
        "never a value: {line}"
    );

    let admin = admin_open(m22).expect("an admin");
    assert_eq!(
        admin.overtaken(),
        run.journal.as_slice(),
        "the view shows it"
    );
    overtaken_shown(m22, &admin).expect("shown");
    assert!(
        admin_open(m22).expect("an admin").overtaken().is_empty(),
        "a later view shows it no more"
    );
    let kept = log_entries::read_overtaken(m22).unwrap();
    assert!(
        kept.len() == 1 && kept[0].shown,
        "the journal keeps the record"
    );
    convergence::carry(&run.lab.all, 1);
    for paths in &run.lab.all[..4] {
        assert_eq!(
            reads(paths, true, "shared"),
            Some(b"v9".to_vec()),
            "the change the waiting write never saw stands"
        );
    }
    run.lab.macs.server.stop();

    let control = overtaken(true);
    assert!(
        control.journal.is_empty(),
        "CONTROL: the write is dropped unseen"
    );
    control.lab.macs.server.stop();
}

/// What [`moved`] saw: the lab, m22's log before the move, what the move's publish signed and
/// said waits.
struct Moved {
    lab: convergence::Six,
    before: Log,
    signed: Vec<Record>,
    waiting: Vec<String>,
}

/// m22 puts `moved` = h1 in the host list `[m22 m7]`, then, while a rotation is due
/// ([`held_remove`]), moves it into `[everyone]`: the host list's entry removed and `[everyone]`'s
/// set to the same value, in one edit. `control` sets [`log_entries::MOVES_WAIT`] for the move.
fn moved(control: bool) -> Moved {
    let lab = convergence::six_with(true);
    let m22 = &lab.all[0];
    let ring = Audience::Hosts(vec!["m22".to_owned(), "m7".to_owned()]);
    publish_edit(m22, |doc| {
        doc.set_entry(&ring, "moved", b"h1").expect("ring");
    });
    convergence::carry(&lab.all, 0);
    held_remove(&lab);
    assert_eq!(
        reads(m22, false, "moved"),
        Some(b"h1".to_vec()),
        "CONTROL: the host list holds it"
    );
    let before = log_of(m22);
    let ids = convergence::ids(m22);
    log_entries::MOVES_WAIT.with(|cell| cell.set(control));
    let waiting = publish_edit(m22, |doc| {
        assert!(doc.unset_entry(&ring, "moved").expect("unset"));
        set(doc, "moved", b"h1");
    })
    .waiting;
    log_entries::MOVES_WAIT.with(|cell| cell.set(false));
    let signed = convergence::signed_since(m22, &ids);
    Moved {
        lab,
        before,
        signed,
        waiting,
    }
}

/// One-mesh W3 ([`moved`]): a name moved from a host list into `[everyone]` while a rotation is
/// due writes at once, as a name that leaves `[everyone]` does: the host list's `Delete` and the
/// `[everyone]` `Put` in one batch, under a fresh `[everyone]` epoch no `Wrap` hands m42. The
/// name is in one set at every point: each prefix of the batch, taken into m22's log as it stood
/// before, reads it in `[everyone]` or in the host list. Nothing waits.
///
/// NEGATIVE CONTROL: [`log_entries::MOVES_WAIT`] (the `[everyone]` `Put` waits for the new key
/// while the host list's `Delete` is signed at once, as before W3), run in the test: after the
/// publish the name is in neither set, as the CONTROL assert says; with the switch set for the
/// first run too, the test fails "the move writes at once".
#[test]
fn a_name_moved_from_a_host_list_into_everyone_while_a_rotation_is_due_is_always_in_one_set() {
    let run = moved(false);
    let m22 = &run.lab.all[0];
    let identity = read_identity(m22).unwrap().unwrap();
    assert!(run.waiting.is_empty(), "the move writes at once");
    let mut log = run.before.clone();
    let mut read = 0;
    for (taken, record) in run.signed.iter().enumerate() {
        let _ = log.insert(record.clone()).expect("insert");
        // A prefix whose newest epochs this Mac does not open yet reads as a log it waits on
        // (its lockers are read meanwhile): nothing in it is in or out of a set.
        if log
            .open_audience_entries(&identity)
            .unwrap()
            .is_none_or(|opened| opened.hidden_epochs)
        {
            continue;
        }
        read += 1;
        assert!(
            reads_in(&log, &identity, true, "moved").is_some()
                || reads_in(&log, &identity, false, "moved").is_some(),
            "moved is in one set after {} of the batch's records",
            taken + 1
        );
    }
    assert!(read > 0, "CONTROL: some prefix reads");
    assert_eq!(reads(m22, true, "moved"), Some(b"h1".to_vec()));
    assert_eq!(
        reads(m22, false, "moved"),
        None,
        "the host list's entry goes"
    );
    let fresh = run
        .signed
        .iter()
        .find(|record| {
            matches!(
                record.body(),
                Body::Epoch {
                    audience: EpochAudience::Everyone,
                    ..
                }
            )
        })
        .map(|record| *record.id())
        .expect("a fresh [everyone] epoch");
    assert!(
        !epochs_wrapped_to(&log_of(m22), &run.lab.recipients[4]).contains(&fresh),
        "no Wrap hands m42 the fresh epoch"
    );
    assert!(
        !log_entries::waiting_file(m22).exists() && !log_entries::waiting_pending(m22),
        "nothing waits"
    );
    run.lab.macs.server.stop();

    let control = moved(true);
    let m22 = &control.lab.all[0];
    assert!(
        reads(m22, true, "moved").is_none() && reads(m22, false, "moved").is_none(),
        "CONTROL: a waiting Put beside a Delete signed at once leaves the name in neither set"
    );
    control.lab.macs.server.stop();
}

/// What [`raced`] saw.
struct Raced {
    lab: convergence::Six,
    /// Whether another thread took this Mac's state lock between the draft's two reads.
    taken_mid_draft: Option<bool>,
    /// What the pass that raced the publish signed.
    racing: Vec<Record>,
    /// What the publish said waits.
    waiting: Vec<String>,
    /// What m22 signed from the publish on, less the racing pass: the publish's own records, its
    /// sync's pass, and one more pass after it.
    after: Vec<Record>,
}

/// What the person does in [`raced`], and when the rotation arrives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Race {
    /// `shared` from v2, waiting, to v3; the rotation arrives in the draft's window, so the
    /// change waits.
    Change,
    /// `fresh`, a name only the waiting write adds, removed; the rotation arrives in the window.
    TakeBack,
    /// `shared` from v2, waiting, to v3 once the rotation arrived, before any pass ran: no
    /// rotation is due, so the change's records sign v3 themselves.
    Signed,
}

/// The controls a run of [`raced`] sets for the publish.
#[derive(Clone, Copy, Default)]
struct Off {
    /// [`log_entries::DRAFT_UNCLAIMED`].
    claim: bool,
    /// [`log_entries::BASES_LOG_ONLY`] and [`log_entries::UNOBSERVED_DRAFT`].
    bases: bool,
}

/// m22 publishes `shared` = v2 (or, for [`Race::TakeBack`], `fresh` = f1) while a rotation is
/// due ([`held_remove`]), and it waits; then edits it as `race` says from a view that shows it
/// waiting. Between that draft and the pending set it keeps, m22's own pass runs
/// (`AFTER_DRAFT`), the rotation arriving just before it unless it arrived before the view; during
/// the draft another thread tries to take m22's state lock between the draft's read of the log
/// and its read of the waiting writes (`DRAFT_READS`). The publish's own sync runs a pass; then
/// m22's pass runs once more and every Mac takes its records.
fn raced(race: Race, off: Off) -> Raced {
    let lab = convergence::six_with(true);
    let chain = encode_records(&held_remove(&lab));
    let m22 = lab.all[0].clone();
    let name = if race == Race::TakeBack {
        "fresh"
    } else {
        "shared"
    };
    publish_edit(&m22, |doc| set(doc, name, b"v2"));
    if race == Race::Signed {
        save_records(&m22, &chain).unwrap();
    }
    let admin = admin_open(&m22).expect("an admin");
    assert_eq!(
        admin.waiting(),
        vec![name.to_owned()],
        "CONTROL: the view shows the write waiting"
    );
    let mut doc = admin.doc.clone();
    if race == Race::TakeBack {
        let _ = doc.unset_entry(&Audience::Everyone, name);
    } else {
        set(&mut doc, name, b"v3");
    }

    let taken = Rc::new(Cell::new(None));
    let seen = Rc::clone(&taken);
    log_entries::DRAFT_READS.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |paths: &Paths| {
            let paths = paths.clone();
            let got = std::thread::spawn(move || {
                StateLock::try_take(&paths)
                    .expect("a take of the state lock")
                    .is_some()
            })
            .join()
            .expect("the thread");
            seen.set(Some(got));
        }));
    });
    let racing = Rc::new(RefCell::new(Vec::new()));
    let signed = Rc::clone(&racing);
    let pass_on = m22.clone();
    AFTER_DRAFT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            if race != Race::Signed {
                save_records(&pass_on, &chain).unwrap();
            }
            let before = convergence::ids(&pass_on);
            vault_pass_at(&pass_on, aterm_time::unix_millis().unwrap_or(0)).unwrap();
            *signed.borrow_mut() = convergence::signed_since(&pass_on, &before);
        }));
    });
    log_entries::DRAFT_UNCLAIMED.with(|cell| cell.set(off.claim));
    log_entries::BASES_LOG_ONLY.with(|cell| cell.set(off.bases));
    log_entries::UNOBSERVED_DRAFT.with(|cell| cell.set(off.bases));
    let before = convergence::ids(&m22);
    let published = admin_publish(&m22, &admin, &doc);
    log_entries::DRAFT_UNCLAIMED.with(|cell| cell.set(false));
    log_entries::BASES_LOG_ONLY.with(|cell| cell.set(false));
    log_entries::UNOBSERVED_DRAFT.with(|cell| cell.set(false));
    let waiting = published.expect("the edit").waiting;
    vault_pass_at(
        &m22,
        aterm_time::unix_millis().unwrap_or(0) + log_entries::GAP_STEP_MS,
    )
    .unwrap();
    let racing = racing.borrow().clone();
    let me = member_key(&m22).1;
    let after: Vec<Record> = convergence::signed_since(&m22, &before)
        .into_iter()
        .filter(|record| *record.actor() == me && !racing.iter().any(|r| r.id() == record.id()))
        .collect();
    convergence::carry(&lab.all, 0);
    Raced {
        lab,
        taken_mid_draft: taken.get(),
        racing,
        waiting,
        after,
    }
}

/// Whether `[everyone]`'s `name` is a conflict as the Mac at `paths` opens its log.
fn conflicted(paths: &Paths, name: &str) -> bool {
    let identity = read_identity(paths).unwrap().unwrap();
    log_of(paths)
        .open_audience_entries(&identity)
        .unwrap()
        .is_some_and(|opened| {
            opened
                .conflicts
                .contains(&(Audience::Everyone, name.to_owned()))
        })
}

/// One-mesh W3 ([`raced`], [`Race::Change`]): a draft that races its own vault pass never reads
/// the newer edit as overtaken. The publish claims this Mac's waiting writes from before its
/// draft reads the log until its pending set is written, so no pass signs between them; the draft
/// reads the log and the waiting writes under one observation of the state, so no pass takes the
/// state between the two reads; and where a pass did sign the earlier write between them, the
/// change's bases hold the waiting write the view showed, so v3 still stands over it. The next
/// pass signs v3, nothing is recorded as overtaken, and every Mac reads v3.
///
/// NEGATIVE CONTROLS, run in the test: [`log_entries::DRAFT_UNCLAIMED`] (the publish takes no
/// claim, as before W3): the racing pass signs v2, and the bases still leave v3 standing; with
/// [`log_entries::UNOBSERVED_DRAFT`] (the draft's two reads with no observation between them) and
/// [`log_entries::BASES_LOG_ONLY`] (a write's one base is the log's value the draft read) too,
/// another thread takes the state lock mid-draft and the pass reads v3 as overtaken by m22's own
/// v2, and every Mac reads v2. The CONTROL asserts say so; with `DRAFT_UNCLAIMED` set for the first
/// run too, the test fails "no pass signs between a publish's draft and its pending set".
#[test]
fn a_draft_racing_its_own_pass_never_reads_the_newer_edit_as_overtaken() {
    for claim in [false, true] {
        let run = raced(
            Race::Change,
            Off {
                claim,
                bases: false,
            },
        );
        let m22 = &run.lab.all[0];
        assert_eq!(
            run.taken_mid_draft,
            Some(false),
            "no pass takes the state between the draft's reads"
        );
        if claim {
            assert_eq!(
                everyone_puts(&run.racing),
                1,
                "CONTROL: unclaimed, the racing pass signs the earlier write"
            );
        } else {
            assert!(
                run.racing.is_empty(),
                "no pass signs between a publish's draft and its pending set"
            );
        }
        assert_eq!(
            run.waiting,
            vec!["shared".to_owned()],
            "CONTROL: the change waited"
        );
        assert_eq!(
            everyone_puts(&run.after),
            1,
            "a later pass signs the newer edit"
        );
        assert!(
            log_entries::read_overtaken(m22).unwrap().is_empty(),
            "the newer edit is never read as overtaken"
        );
        for paths in &run.lab.all[..4] {
            assert_eq!(reads(paths, true, "shared"), Some(b"v3".to_vec()));
        }
        run.lab.macs.server.stop();
    }

    let control = raced(
        Race::Change,
        Off {
            claim: true,
            bases: true,
        },
    );
    assert_eq!(
        control.taken_mid_draft,
        Some(true),
        "CONTROL: with no observation a pass may take the state mid-draft"
    );
    assert_eq!(
        reads(&control.lab.all[1], true, "shared"),
        Some(b"v2".to_vec()),
        "CONTROL: with the log's value its one base the newer edit is dropped"
    );
    control.lab.macs.server.stop();
}

/// One-mesh W3 ([`raced`], [`Race::TakeBack`]): a take-back that races this Mac's own pass never
/// publishes the value the person took back. No pass signs the earlier write between the draft
/// and its pending set, the take-back lands over it and is done: nothing is signed, nothing is
/// recorded as overtaken, the publish names nothing as waiting, and no Mac reads `fresh`.
///
/// NEGATIVE CONTROL: [`log_entries::DRAFT_UNCLAIMED`] (the publish takes no claim, as before
/// W3), run in the test: the racing pass signs `fresh` = v2, which every Mac reads, and the
/// take-back that finds it there is recorded as overtaken, never signed as a `Delete` under the
/// reused epoch, as the CONTROL asserts say; with the switch set for the first run too, the test
/// fails "no pass signs the write the person is taking back".
#[test]
fn a_take_back_racing_its_own_pass_never_publishes_the_value_taken_back() {
    let run = raced(Race::TakeBack, Off::default());
    let m22 = &run.lab.all[0];
    assert!(
        run.racing.is_empty(),
        "no pass signs the write the person is taking back"
    );
    assert!(
        run.waiting.is_empty(),
        "a take-back is never named as waiting"
    );
    assert_eq!(everyone_puts(&run.after), 0, "nothing is signed for it");
    assert!(
        log_entries::read_overtaken(m22).unwrap().is_empty(),
        "nothing is recorded as overtaken"
    );
    assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
    for paths in &run.lab.all[..4] {
        assert_eq!(reads(paths, true, "fresh"), None);
    }
    run.lab.macs.server.stop();

    let control = raced(
        Race::TakeBack,
        Off {
            claim: true,
            bases: false,
        },
    );
    assert_eq!(
        everyone_puts(&control.racing),
        1,
        "CONTROL: unclaimed, the racing pass signs the write being taken back"
    );
    assert_eq!(
        reads(&control.lab.all[1], true, "fresh"),
        Some(b"v2".to_vec()),
        "CONTROL: and every Mac reads it"
    );
    assert_eq!(
        log_entries::read_overtaken(&control.lab.all[0])
            .unwrap()
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>(),
        vec!["fresh"],
        "CONTROL: the take-back that finds it is recorded, never signed as a Delete"
    );
    control.lab.macs.server.stop();
}

/// One-mesh W3 ([`raced`], [`Race::Signed`]): a change whose own records sign it, made from a view
/// that shows a waiting write the pass has not signed yet, never meets a concurrent `Put` of the
/// write it replaces. No pass signs between the draft and its pending set; the change lands, signed
/// once, the waiting write is settled, and every Mac reads v3 with no conflict.
///
/// NEGATIVE CONTROL: [`log_entries::DRAFT_UNCLAIMED`] (the publish takes no claim, as before
/// W3), run in the test: the racing pass signs v2 under the new epoch, a `Put` concurrent with the
/// change's v3. Which wins is the two records' order: where v3 wins, `shared` stays a conflict;
/// where v2 wins, every Mac reads v2 until a later pass signs v3 a second time. The CONTROL
/// asserts say so, whichever won; with the switch set for the first run too, the test fails "no
/// pass signs the write the change replaces".
#[test]
fn a_change_whose_records_sign_it_racing_a_pass_leaves_no_conflict() {
    let run = raced(Race::Signed, Off::default());
    let m22 = &run.lab.all[0];
    assert!(
        run.racing.is_empty(),
        "no pass signs the write the change replaces"
    );
    assert!(run.waiting.is_empty(), "CONTROL: the change does not wait");
    assert_eq!(everyone_puts(&run.after), 1, "the change is signed once");
    assert!(!conflicted(m22, "shared"), "no conflict");
    assert!(
        log_entries::read_overtaken(m22).unwrap().is_empty(),
        "nothing is recorded as overtaken"
    );
    for paths in &run.lab.all[..4] {
        assert_eq!(reads(paths, true, "shared"), Some(b"v3".to_vec()));
    }
    run.lab.macs.server.stop();

    let control = raced(
        Race::Signed,
        Off {
            claim: true,
            bases: false,
        },
    );
    assert_eq!(
        everyone_puts(&control.racing),
        1,
        "CONTROL: unclaimed, the racing pass signs the write the change replaces"
    );
    assert!(
        conflicted(&control.lab.all[0], "shared") || everyone_puts(&control.after) == 2,
        "CONTROL: beside the change's own v3 it is a conflict, or v3 is signed again to settle it"
    );
    control.lab.macs.server.stop();
}

/// What [`in_flight`] saw: the lab, what m22's pass signed while the change was in flight, and
/// after the sync that kept it.
struct InFlight {
    lab: convergence::Six,
    during: Vec<Record>,
    after: Vec<Record>,
}

/// m22 publishes `shared` = v2 while a rotation is due ([`held_remove`]), and it waits; the
/// rotation arrives, and m22 changes `shared` to v3 from a view that shows v2 waiting: no rotation
/// is due now, so the change's records sign v3 themselves. The publish ends once the server
/// stored its version, before it keeps its records (`CUT_AFTER_STORE`); m22's pass runs, then its
/// sync reads the stored version back, then its pass runs again. `control` sets
/// [`log_entries::RACE_UNDEFERRED`] for the passes.
fn in_flight(control: bool) -> InFlight {
    let lab = convergence::six_with(true);
    let chain = held_remove(&lab);
    let m22 = &lab.all[0];
    publish_edit(m22, |doc| set(doc, "shared", b"v2"));
    save_records(m22, &encode_records(&chain)).unwrap();
    let admin = admin_open(m22).expect("an admin");
    assert_eq!(
        admin.waiting(),
        vec!["shared".to_owned()],
        "CONTROL: the view shows v2 waiting"
    );
    let mut doc = admin.doc.clone();
    set(&mut doc, "shared", b"v3");
    CUT_AFTER_STORE.with(|cell| cell.set(true));
    assert!(
        admin_publish(m22, &admin, &doc).is_err(),
        "CONTROL: the publish ends early"
    );
    log_entries::RACE_UNDEFERRED.with(|cell| cell.set(control));
    let before = convergence::ids(m22);
    vault_pass_at(m22, aterm_time::unix_millis().unwrap_or(0)).unwrap();
    let during = convergence::signed_since(m22, &before);
    sync(m22, None).expect("m22 syncs");
    let before = convergence::ids(m22);
    vault_pass_at(
        m22,
        aterm_time::unix_millis().unwrap_or(0) + log_entries::GAP_STEP_MS,
    )
    .unwrap();
    let after = convergence::signed_since(m22, &before);
    log_entries::RACE_UNDEFERRED.with(|cell| cell.set(false));
    convergence::carry(&lab.all, 0);
    InFlight { lab, during, after }
}

/// One-mesh W3 ([`in_flight`]): while a pending set of this Mac writes a name, the waiting write
/// it replaces is left as it is: the pass signs nothing while the change is in flight, and reads
/// the person's own newer edit as no change that overtook it. The sync that reads the stored
/// version back keeps its records and lands its set, the waiting write is settled, and every Mac
/// reads v3 with nothing recorded as overtaken.
///
/// NEGATIVE CONTROL: [`log_entries::RACE_UNDEFERRED`] (a pass signs or drops a waiting write
/// whose name a pending set writes, as before W3), run in the test: the pass while the change is
/// in flight signs v2, as the CONTROL assert says; with the switch set for the first run too, the
/// test fails "the pass while the change is in flight signs nothing".
#[test]
fn a_waiting_write_a_pending_set_rewrites_waits_for_that_set() {
    let run = in_flight(false);
    let m22 = &run.lab.all[0];
    assert_eq!(
        everyone_puts(&run.during),
        0,
        "the pass while the change is in flight signs nothing"
    );
    assert_eq!(everyone_puts(&run.after), 0, "the change signed itself");
    assert!(
        log_entries::read_overtaken(m22).unwrap().is_empty(),
        "the person's own newer edit is never read as overtaken"
    );
    assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
    for paths in &run.lab.all[..4] {
        assert_eq!(reads(paths, true, "shared"), Some(b"v3".to_vec()));
    }
    run.lab.macs.server.stop();

    let control = in_flight(true);
    assert_eq!(
        everyone_puts(&control.during),
        1,
        "CONTROL: an undeferred pass signs the write the change replaced"
    );
    control.lab.macs.server.stop();
}

/// One-mesh W3: a save that leaves a waiting settle of a conflict as its view showed it, made
/// once no rotation is due, signs that settle in its own records. m22 and m7 each put `shared`,
/// neither seeing the other's, and m22 holds the conflict and m7's `Remove` of m42, not its
/// rotation ([`held_remove`]). A person saves `shared` as the winner's own value on m22, and the
/// settle waits for the new key. The rotation arrives, no pass runs, and the person saves another
/// name from a view that still shows the settle waiting: the save's records sign the `Delete` of
/// the siblings and the `Put` of the winner themselves, so right after the publish, while a claim
/// keeps every pass from signing a waiting write, `shared` is no conflict on m22. A pass then
/// signs no `[everyone]` value: every Mac reads the winner with no conflict, nothing is recorded
/// as overtaken, and nothing waits.
///
/// NEGATIVE CONTROL: the `carried.resolves(name)` term of `draft_document_diff_with`'s `resolving`
/// cut by hand, leaving only `resolving(aud, name)` (restored with cp from a backup plus touch,
/// confirmed with cmp): the later save's records write nothing for `shared`, whose value is the
/// winner's already, and the test fails "the later save's own records settle the conflict".
#[test]
fn a_later_save_signs_the_settle_of_a_conflict_its_view_showed_waiting() {
    let lab = convergence::six_with(true);
    let (m22, m7) = (&lab.all[0], &lab.all[1]);
    let base = convergence::ids(m7);
    test_put_everyone(m22, "shared", b"c22").expect("m22 puts shared");
    test_put_everyone(m7, "shared", b"c7").expect("m7 puts shared");
    save_records(m22, &encode_records(&convergence::signed_since(m7, &base))).unwrap();
    convergence::carry(&lab.all, 0);
    assert!(
        conflicted(m22, "shared") && conflicted(m7, "shared"),
        "CONTROL: m22 and m7 hold a conflict on shared"
    );
    let chain = held_remove(&lab);

    let mut admin = admin_open(m22).expect("an admin");
    let winner = shows(&admin, "shared").expect("CONTROL: the view shows the conflict's winner");
    let mut doc = admin.doc.clone();
    set(&mut doc, "shared", &winner);
    admin.resolving(&Audience::Everyone, "shared");
    let first = admin_publish(m22, &admin, &doc)
        .expect("the settle")
        .waiting;
    assert_eq!(
        first,
        vec!["shared".to_owned()],
        "CONTROL: the settle waits for the new key"
    );

    save_records(m22, &encode_records(&chain)).unwrap();
    assert!(
        !log_of(m22)
            .rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone && due.rotation),
        "CONTROL: the rotation arrived"
    );
    let admin = admin_open(m22).expect("an admin");
    assert_eq!(
        admin.waiting(),
        vec!["shared".to_owned()],
        "CONTROL: the view still shows the settle waiting"
    );
    assert!(
        conflicted(m22, "shared"),
        "CONTROL: the conflict stands until the settle is signed"
    );
    let mut doc = admin.doc.clone();
    set(&mut doc, "other", b"x");
    let claim = log_entries::claim_draft(m22, None).expect("a claim");
    let later = admin_publish(m22, &admin, &doc)
        .expect("the later save")
        .waiting;
    assert!(
        later.is_empty(),
        "CONTROL: no rotation is due, so the later save waits for nothing"
    );
    assert!(
        !conflicted(m22, "shared"),
        "the later save's own records settle the conflict"
    );
    drop(claim);

    let before = convergence::ids(m22);
    vault_pass_at(m22, aterm_time::unix_millis().unwrap_or(0)).unwrap();
    assert_eq!(
        everyone_puts(&convergence::signed_since(m22, &before)),
        0,
        "a pass then signs no [everyone] value"
    );
    convergence::carry(&lab.all, 0);
    assert!(
        log_entries::read_overtaken(m22).unwrap().is_empty(),
        "nothing is recorded as overtaken"
    );
    assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
    for paths in &lab.all[..4] {
        assert_eq!(
            reads(paths, true, "shared"),
            Some(winner.clone()),
            "every Mac reads the winner"
        );
        assert!(!conflicted(paths, "shared"), "no conflict anywhere");
        assert_eq!(reads(paths, true, "other"), Some(b"x".to_vec()));
    }
    lab.macs.server.stop();
}
