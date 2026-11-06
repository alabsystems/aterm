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
/// its records. `control` names the negative control set for the edits, if any.
fn take_back(control: Option<&'static std::thread::LocalKey<Cell<bool>>>) -> TakeBack {
    let lab = convergence::six_with(true);
    let chain = held_remove(&lab);
    let m22 = &lab.all[0];
    if let Some(switch) = control {
        switch.with(|cell| cell.set(true));
    }
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
    if let Some(switch) = control {
        switch.with(|cell| cell.set(false));
    }

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
/// NEGATIVE CONTROLS, run in the test: [`log_entries::VIEW_WITHOUT_WAITING`] (the view shows the
/// log alone, as before W3): the view shows nothing waiting, the take-back is no change, and the
/// passes sign the change the person took back, so every Mac reads `shared` = v3; and
/// [`log_entries::NAMES_UNMERGED`] (the edit reviews only its own entries' names, not the view's
/// waiting writes and the ones that stand now merged in): `fresh`, which the take-back removes
/// and the log never held, is no name of the edit's entries, so no write takes it back and the
/// passes sign `fresh` = f1, as the CONTROL asserts say. With either switch set for the first run
/// too, the test fails.
#[test]
fn a_waiting_write_shows_in_the_view_and_a_save_changes_it_or_takes_it_back() {
    let run = take_back(None);
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

    let control = take_back(Some(&log_entries::VIEW_WITHOUT_WAITING));
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

    let control = take_back(Some(&log_entries::NAMES_UNMERGED));
    assert_eq!(
        control.after,
        vec!["fresh".to_owned()],
        "CONTROL: unmerged, the take-back of fresh writes nothing, so its write still waits"
    );
    assert_eq!(
        reads(&control.lab.all[1], true, "fresh"),
        Some(b"f1".to_vec()),
        "CONTROL: unmerged, the passes sign the fresh the person removed"
    );
    assert_eq!(
        reads(&control.lab.all[1], true, "shared"),
        Some(b"v1".to_vec()),
        "CONTROL: a name the edit's entries hold is taken back either way"
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
/// sync reads the stored version back, then its pass runs again. `race` sets
/// [`log_entries::RACE_UNDEFERRED`] for the passes, and `held` [`log_entries::HELD_UNDEFERRED`].
fn in_flight(race: bool, held: bool) -> InFlight {
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
    log_entries::RACE_UNDEFERRED.with(|cell| cell.set(race));
    log_entries::HELD_UNDEFERRED.with(|cell| cell.set(held));
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
    log_entries::HELD_UNDEFERRED.with(|cell| cell.set(false));
    convergence::carry(&lab.all, 0);
    InFlight { lab, during, after }
}

/// One-mesh W3 ([`in_flight`]): while a pending set of this Mac writes a name, the waiting write
/// it replaces is left as it is: the pass signs nothing while the change is in flight, and reads
/// the person's own newer edit as no change that overtook it. The sync that reads the stored
/// version back keeps its records and lands its set, the waiting write is settled, and every Mac
/// reads v3 with nothing recorded as overtaken.
///
/// The change's hold changes `shared` too, so two deferrals keep the pass off it: its pending
/// set's (W3) and its held draft's (P3 LaterKept). Each alone keeps it: with
/// [`log_entries::RACE_UNDEFERRED`] alone the pass still signs nothing. That switch alone is
/// caught by the waiting conformance's two waiting writes, whose sets are of waiting writes no
/// hold changes; [`log_entries::HELD_UNDEFERRED`] alone by its direct drafts, which keep no set.
///
/// NEGATIVE CONTROL: both switches (a pass defers no name a pending set writes or a held draft
/// changes, as before W3), run in the test: the pass while the change is in flight signs v2, as
/// the CONTROL assert says; with both set for the first run too, the test fails "the pass while
/// the change is in flight signs nothing".
#[test]
fn a_waiting_write_a_pending_set_rewrites_waits_for_that_set() {
    let run = in_flight(false, false);
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

    let race = in_flight(true, false);
    assert_eq!(
        everyone_puts(&race.during),
        0,
        "the change's hold keeps the pass off shared without its set"
    );
    race.lab.macs.server.stop();

    let control = in_flight(true, true);
    assert_eq!(
        everyone_puts(&control.during),
        1,
        "CONTROL: an undeferred pass signs the write the change replaced"
    );
    control.lab.macs.server.stop();
}

/// What [`later_settle`] saw.
struct LaterSettle {
    lab: convergence::Six,
    /// The winner of `shared`'s conflict, as m22's view showed it.
    winner: Vec<u8>,
    /// What the later save said waits.
    later: Vec<String>,
    /// Whether `shared` was still a conflict on m22 right after the later save.
    conflicted: bool,
}

/// m22 and m7 each put `shared`, neither seeing the other's, and m22 holds the conflict and m7's
/// `Remove` of m42, not its rotation ([`held_remove`]). A person saves `shared` as the winner's
/// own value on m22, and the settle waits for the new key. The rotation arrives, no pass runs, and
/// the person saves another name from a view that still shows the settle waiting, under a claim
/// that keeps every pass from signing a waiting write. `control` sets
/// [`log_entries::SETTLE_UNCARRIED`] for the saves.
fn later_settle(control: bool) -> LaterSettle {
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
    log_entries::SETTLE_UNCARRIED.with(|cell| cell.set(control));

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
    let conflicted = conflicted(m22, "shared");
    drop(claim);
    log_entries::SETTLE_UNCARRIED.with(|cell| cell.set(false));
    LaterSettle {
        lab,
        winner,
        later,
        conflicted,
    }
}

/// One-mesh W3 ([`later_settle`]): a save that leaves a waiting settle of a conflict as its view
/// showed it, made once no rotation is due, signs that settle in its own records: right after the
/// publish, while the claim keeps every pass from signing a waiting write, `shared` is no
/// conflict on m22. A pass then signs no `[everyone]` value: every Mac reads the winner with no
/// conflict, nothing is recorded as overtaken, and nothing waits.
///
/// NEGATIVE CONTROL: [`log_entries::SETTLE_UNCARRIED`] (the save signs the name without the
/// settle its view showed waiting, as before 24410be588), run in the test: the later save's
/// records write nothing for `shared`, whose value is the winner's already, and the conflict
/// stands, as the CONTROL assert says.
#[test]
fn a_later_save_signs_the_settle_of_a_conflict_its_view_showed_waiting() {
    let run = later_settle(false);
    let m22 = &run.lab.all[0];
    assert!(
        run.later.is_empty(),
        "CONTROL: no rotation is due, so the later save waits for nothing"
    );
    assert!(
        !run.conflicted,
        "the later save's own records settle the conflict"
    );

    let before = convergence::ids(m22);
    vault_pass_at(m22, aterm_time::unix_millis().unwrap_or(0)).unwrap();
    assert_eq!(
        everyone_puts(&convergence::signed_since(m22, &before)),
        0,
        "a pass then signs no [everyone] value"
    );
    convergence::carry(&run.lab.all, 0);
    assert!(
        log_entries::read_overtaken(m22).unwrap().is_empty(),
        "nothing is recorded as overtaken"
    );
    assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
    for paths in &run.lab.all[..4] {
        assert_eq!(
            reads(paths, true, "shared"),
            Some(run.winner.clone()),
            "every Mac reads the winner"
        );
        assert!(!conflicted(paths, "shared"), "no conflict anywhere");
        assert_eq!(reads(paths, true, "other"), Some(b"x".to_vec()));
    }
    run.lab.macs.server.stop();

    let control = later_settle(true);
    assert!(
        control.later.is_empty(),
        "CONTROL: the uncarried save waits for nothing either"
    );
    assert!(
        control.conflicted,
        "CONTROL: a save that drops the settle its view showed leaves the conflict"
    );
    control.lab.macs.server.stop();
}

/// What [`stored_unkept`] saw.
struct Unkept {
    lab: convergence::Six,
    s2: crate::server::Running,
    /// The sequence and envelope digest each server holds once both publishes answered: the
    /// first server's, then the second's.
    served: [(u64, [u8; 32]); 2],
    /// What the second publish said waits.
    waiting: Vec<String>,
    /// `shared` as m22's log reads it once the first publish kept its records, before any pass.
    kept: Option<Vec<u8>>,
    /// Whether the second publish's waiting write of `shared` may be signed over what m22's log
    /// reads there then ([`log_entries::waits_over`]).
    over: Option<bool>,
    /// What m22's passes signed once the rotation arrived.
    signed: Vec<Record>,
}

/// `[everyone]`'s `name` as the Mac at `paths` opens its log.
fn entry_at(paths: &Paths, name: &str) -> Option<Entry> {
    let identity = read_identity(paths).unwrap().unwrap();
    log_of(paths)
        .open_audience_entries(&identity)
        .unwrap()?
        .by_audience
        .get(&Audience::Everyone)?
        .get(name)
        .cloned()
}

/// The version `url` serves: its sequence and envelope digest.
fn served_at(url: &str) -> (u64, [u8; 32]) {
    let Fetched::Body(bytes) = transport::fetch(
        &Source::Url(url.to_owned()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("a read of the server") else {
        panic!("{url} serves no envelope");
    };
    let parsed = envelope::parse(&bytes).expect("an envelope");
    (parsed.seq, parsed.digest())
}

/// What the first publish of [`stored_unkept`] does to `shared`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Earlier {
    /// Sets it to v2.
    Put,
    /// Takes it out of `[everyone]`.
    Delete,
}

/// One person, two commands at once from one opened view, which shows `shared` = v1: m22 opens the
/// vault and publishes from it (P1) `shared` = v2, or with `shared` taken out (`earlier`); no
/// rotation is due, so P1's records sign that at once. The server stores P1, and before P1 keeps
/// its records (`AFTER_STORE`), m22 takes m7's `Remove` of m42 without its rotation, and a second
/// publish from the same opened view (P2) sets `shared` = v3, which now waits for the new key.
/// The server refuses P2 as not newer; a second server, which P1 never reached, stores it. Then
/// P1 keeps its records and ends, the rotation arrives, m22's passes run, and every Mac takes its
/// records. `control` sets [`log_entries::BASES_WITHOUT_HELD`] for both publishes.
fn stored_unkept(earlier: Earlier, control: bool) -> Unkept {
    let lab = convergence::six_with(true);
    let (m22, m7) = (lab.all[0].clone(), lab.all[1].clone());
    let s2 =
        crate::client::log_entries_tests::start_loopback_server(&lab.macs.root.path().join("s2"));
    let url2 = format!("http://{}/v1/alab", s2.addr);
    let base = convergence::ids(&m7);
    test_remove_unposted(&m7, &join_code(&lab.recipients[4])).expect("m7 removes m42");
    let chain = convergence::signed_since(&m7, &base);
    let opened = Rc::new(admin_open(&m22).expect("an admin"));
    assert_eq!(
        shows(&opened, "shared"),
        Some(b"v1".to_vec()),
        "CONTROL: the opened view shows v1"
    );
    let mut first = opened.doc.clone();
    match earlier {
        Earlier::Put => set(&mut first, "shared", b"v2"),
        Earlier::Delete => {
            let _ = first.unset_entry(&Audience::Everyone, "shared");
        }
    }
    let mut second = opened.doc.clone();
    set(&mut second, "shared", b"v3");

    let waiting = Rc::new(RefCell::new(None));
    let said = Rc::clone(&waiting);
    let (on, view, remove) = (m22.clone(), Rc::clone(&opened), encode_records(&chain[..1]));
    AFTER_STORE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            save_records(&on, &remove).unwrap();
            assert!(
                log_of(&on)
                    .rekey_dues()
                    .iter()
                    .any(|due| due.audience == EpochAudience::Everyone && due.rotation),
                "CONTROL: m22 holds the Remove, not its rotation"
            );
            let second = admin_publish_with_candidates(&on, &view, &second, &[url2])
                .expect("P2: the second server stores it");
            *said.borrow_mut() = Some(second.waiting);
        }));
    });
    log_entries::BASES_WITHOUT_HELD.with(|cell| cell.set(control));
    let first = admin_publish(&m22, &opened, &first);
    log_entries::BASES_WITHOUT_HELD.with(|cell| cell.set(false));
    first.expect("P1");
    let waiting = waiting
        .borrow_mut()
        .take()
        .expect("CONTROL: P2 ran in P1's window");
    let served = [
        served_at(&lab.macs.url),
        served_at(&format!("http://{}/v1/alab", s2.addr)),
    ];
    let kept = reads(&m22, true, "shared");
    let over = log_entries::waits_over(
        &m22,
        &read_identity(&m22).unwrap().unwrap(),
        "shared",
        entry_at(&m22, "shared").as_ref(),
    );

    save_records(&m22, &encode_records(&chain)).unwrap();
    let signed = two_passes(&m22);
    convergence::carry(&lab.all, 0);
    Unkept {
        lab,
        s2,
        served,
        waiting,
        kept,
        over,
        signed,
    }
}

/// Which Mac's tender covers the fresh `[everyone]` epoch that [`stored_unkept`]'s earlier
/// publish minted for its `Delete` ([`covered`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Coverer {
    /// m7, the remover: first among the removers, it waits no grace, so its first pass once it
    /// holds that epoch coalesces it.
    Remover,
    /// m22 alone, the writer, with no pass of any other Mac: no remover, it waits the remover's
    /// grace ([`log_entries::REMOVER_GRACE_MS`]) and then its rank's steps among the members.
    Writer,
}

/// After [`stored_unkept`] with [`Earlier::Delete`]: the earlier publish's `Delete` minted its own
/// fresh `[everyone]` epoch (F15), wrapped to m42, since it was drafted before m22 held the
/// `Remove`. m7's rotation, concurrent with it, leaves it a newest epoch, so a rotation is due
/// over it in turn, and the passes of [`stored_unkept`] sign nothing. `by`'s tender then covers
/// both newest epochs with one, the coalesce of the ranked re-key
/// (`coalesce_concurrent_commons_epochs`): a pass of m7 at once, which every Mac takes, then
/// [`two_passes`] of m22; or one pass of m22 alone at its turn. Every Mac then takes m22's
/// records. Returns what m22 holds since, the coalesce among it.
fn covered(run: &Unkept, by: Coverer) -> Vec<Record> {
    let (m22, m7) = (&run.lab.all[0], &run.lab.all[1]);
    let before = convergence::ids(m22);
    let now = aterm_time::unix_millis().unwrap_or(0);
    match by {
        Coverer::Remover => {
            vault_pass_at(m7, now).unwrap();
            convergence::carry(&run.lab.all, 1);
            two_passes(m22);
        }
        Coverer::Writer => {
            let log = log_of(m22);
            let turn = log
                .rekey_dues()
                .into_iter()
                .find(|due| due.audience == EpochAudience::Everyone)
                .and_then(|due| due.turn(&log.membership(), &run.lab.actors[0]))
                .expect("CONTROL: m22 takes a turn in the due");
            assert!(
                turn.after_grace,
                "CONTROL: m22 is no remover, so its turn waits the remover's grace"
            );
            let rank = u64::try_from(turn.rank).unwrap();
            let at = now + log_entries::REMOVER_GRACE_MS + rank * log_entries::GAP_STEP_MS;
            vault_pass_at(m22, at).unwrap();
        }
    }
    convergence::carry(&run.lab.all, 0);
    convergence::signed_since(m22, &before)
}

/// One-mesh P3 ([`stored_unkept`]): a waiting write drafted while a server holds an earlier
/// version of this Mac whose records are not kept yet is signed over that version's value. The
/// later publish reads the log without the earlier one's records, but its hold
/// (`records.drafts`), so the write's bases hold what the earlier publish leaves at the name, v2
/// or, where it took the name out, no value: once it keeps its records, the write stands over
/// what the log reads. With v2, once the rotation arrives, a pass signs v3, the person's later
/// save, nothing is recorded as overtaken, and every Mac reads v3. Where the earlier publish took
/// the name out, its `Delete`'s fresh epoch, wrapped to m42, keeps a rotation due after m7's
/// arrives, and the write still waits after those passes, neither signed nor overtaken. The
/// tender's coalesce covers that epoch ([`covered`]), whether m7, the remover, mints it at once
/// or m22 alone does at its turn after the grace; a pass of m22 then signs v3, as above, and
/// every Mac the log keeps reads v3. (What m42, the removed Mac, can open is P1's law, not this
/// test's.)
///
/// NEGATIVE CONTROL: [`log_entries::BASES_WITHOUT_HELD`] (a write's bases leave out what this
/// Mac's held drafts write, as before), run in the test: the write's one base is the log's v1, so
/// it stands over neither v2 nor no value, and the pass drops v3 as overtaken by m22's own v2,
/// which every Mac then reads, or, past the coalesce, by m22's own `Delete`, so no Mac reads
/// `shared`, as the CONTROL asserts say; with the switch set for the first run too, the test
/// fails "P2's write stands over what P1 leaves".
#[test]
fn a_waiting_write_drafted_before_its_own_earlier_version_kept_its_records_is_signed() {
    for (earlier, control, coverer) in [
        (Earlier::Put, false, None),
        (Earlier::Put, true, None),
        (Earlier::Delete, false, Some(Coverer::Remover)),
        (Earlier::Delete, false, Some(Coverer::Writer)),
        (Earlier::Delete, true, Some(Coverer::Remover)),
        (Earlier::Delete, true, Some(Coverer::Writer)),
    ] {
        let run = stored_unkept(earlier, control);
        let m22 = &run.lab.all[0];
        let earlier_value = (earlier == Earlier::Put).then(|| b"v2".to_vec());
        assert_eq!(
            run.served[0].0, run.served[1].0,
            "CONTROL: both publishes are of one sequence"
        );
        assert_ne!(
            run.served[0].1, run.served[1].1,
            "CONTROL: the first server holds P1, the second P2"
        );
        assert_eq!(
            run.waiting,
            vec!["shared".to_owned()],
            "CONTROL: P2's write waits"
        );
        assert_eq!(
            run.kept, earlier_value,
            "CONTROL: P1's records are kept before any pass"
        );
        if control {
            assert_eq!(
                run.over,
                Some(false),
                "CONTROL: with the old bases P2's write stands over the log's v1 alone"
            );
        } else {
            assert_eq!(
                run.over,
                Some(true),
                "P2's write stands over what P1 leaves"
            );
        }
        let signed = match coverer {
            None => run.signed.clone(),
            Some(by) => {
                let log = log_of(m22);
                assert_eq!(
                    log.newest_epochs(&EpochAudience::Everyone).len(),
                    2,
                    "CONTROL: two newest epochs, P1's fresh one and m7's rotation"
                );
                let minters: BTreeSet<MemberId> = log
                    .newest_epochs(&EpochAudience::Everyone)
                    .iter()
                    .filter_map(|epoch| log.get(epoch).map(|record| *record.actor()))
                    .collect();
                assert_eq!(
                    minters,
                    BTreeSet::from([run.lab.actors[0], run.lab.actors[1]]),
                    "CONTROL: P1's fresh epoch stands beside m7's rotation"
                );
                assert!(
                    log.rekey_dues()
                        .iter()
                        .any(|due| due.audience == EpochAudience::Everyone && due.rotation),
                    "CONTROL: P1's fresh epoch, wrapped to m42, keeps a rotation due"
                );
                assert_eq!(
                    everyone_puts(&run.signed),
                    0,
                    "CONTROL: the passes sign nothing while it is due"
                );
                assert!(
                    log_entries::waiting_file(m22).exists(),
                    "CONTROL: the write still waits"
                );
                assert!(
                    log_entries::read_overtaken(m22).unwrap().is_empty(),
                    "CONTROL: and is not overtaken"
                );
                let signed = covered(&run, by);
                let log = log_of(m22);
                assert_eq!(
                    log.newest_epochs(&EpochAudience::Everyone).len(),
                    1,
                    "{by:?}: one epoch covers both"
                );
                let coalescer = match by {
                    Coverer::Remover => run.lab.actors[1],
                    Coverer::Writer => run.lab.actors[0],
                };
                assert_eq!(
                    log.newest_epochs(&EpochAudience::Everyone)
                        .iter()
                        .find_map(|epoch| log.get(epoch).map(|record| *record.actor())),
                    Some(coalescer),
                    "{by:?}: that Mac's tender mints the coalesce"
                );
                assert!(
                    !log.rekey_dues()
                        .iter()
                        .any(|due| due.audience == EpochAudience::Everyone),
                    "{by:?}: nothing is due over it"
                );
                signed
            }
        };
        let journal = log_entries::read_overtaken(m22).unwrap();
        // Every Mac but m42, which the log removed.
        let readers = run
            .lab
            .all
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 4)
            .map(|(_, paths)| paths);
        if control {
            assert_eq!(
                everyone_puts(&signed),
                0,
                "CONTROL: with the old bases nothing is signed"
            );
            assert_eq!(
                journal
                    .iter()
                    .map(|record| record.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["shared"],
                "CONTROL: the later save is dropped as overtaken"
            );
            let by = journal[0].by.as_ref();
            assert_eq!(
                by.map(|by| by.label.as_str()),
                log_of(m22).membership().label(&run.lab.actors[0]),
                "CONTROL: by m22's own earlier write"
            );
            assert_eq!(
                by.map(|by| by.deleted),
                Some(earlier == Earlier::Delete),
                "CONTROL: its Put or its Delete"
            );
            for paths in readers {
                assert_eq!(
                    reads(paths, true, "shared"),
                    earlier_value,
                    "CONTROL: and every Mac reads what the earlier save left"
                );
            }
        } else {
            assert_eq!(
                everyone_puts(&signed),
                1,
                "a pass signs the person's later save"
            );
            assert!(journal.is_empty(), "nothing is recorded as overtaken");
            assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
            for paths in readers {
                assert_eq!(reads(paths, true, "shared"), Some(b"v3".to_vec()));
            }
        }
        run.s2.stop();
        run.lab.macs.server.stop();
    }
}

/// What [`kept_late`] saw.
struct KeptLate {
    lab: convergence::Six,
    /// How each later open stood to the log and what it showed at `shared`: P2a's, then P2b's.
    opens: Vec<(log_entries::LogBase, Option<Vec<u8>>)>,
    /// What P2b said waits.
    waiting: Vec<String>,
    /// `shared` as m22's log reads it once P1 ended, before any pass.
    ended: Option<Vec<u8>>,
    /// What m22's passes signed once the rotation arrived.
    signed: Vec<Record>,
}

/// m22 opens the vault and publishes `shared` = v2 from it (P1); no rotation is due, so P1's
/// records sign v2. The server stores P1, and before P1 keeps its records (`AFTER_STORE`), m22
/// opens the vault again (P2a): the server serves P1, whose records m22's log lacks, so the open
/// reads P1's text and P2a saves `other` = o1 from it, unstamped. P2a's version names P1's as its
/// parent, and its landing settles P1's hold by descent. m22 takes m7's `Remove` of m42 without
/// its rotation and opens P2a's version (P2b), whose text shows v2; the person sets `shared` = v3,
/// which waits for the new key, as P2a's `other` does, imported from that text. Then P1 goes on
/// to keep its records and ends, the rotation arrives, m22's passes run, and every Mac takes its
/// records. `control` sets [`log_entries::KEEPS_SUPERSEDED`] for P1.
fn kept_late(control: bool) -> KeptLate {
    let lab = convergence::six_with(true);
    let (m22, m7) = (lab.all[0].clone(), lab.all[1].clone());
    let base = convergence::ids(&m7);
    test_remove_unposted(&m7, &join_code(&lab.recipients[4])).expect("m7 removes m42");
    let chain = convergence::signed_since(&m7, &base);
    let opened = admin_open(&m22).expect("an admin");
    let mut first = opened.doc.clone();
    set(&mut first, "shared", b"v2");

    let seen = Rc::new(RefCell::new((Vec::new(), Vec::new())));
    let saw = Rc::clone(&seen);
    let (on, remove) = (m22.clone(), encode_records(&chain[..1]));
    AFTER_STORE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let p2a = admin_open(&on).expect("P2a opens");
            saw.borrow_mut()
                .0
                .push((p2a.log_base, shows(&p2a, "shared")));
            let mut doc = p2a.doc.clone();
            set(&mut doc, "other", b"o1");
            admin_publish(&on, &p2a, &doc).expect("P2a");
            save_records(&on, &remove).unwrap();
            let p2b = admin_open(&on).expect("P2b opens");
            saw.borrow_mut()
                .0
                .push((p2b.log_base, shows(&p2b, "shared")));
            let mut doc = p2b.doc.clone();
            set(&mut doc, "shared", b"v3");
            saw.borrow_mut().1 = admin_publish(&on, &p2b, &doc).expect("P2b").waiting;
        }));
    });
    log_entries::KEEPS_SUPERSEDED.with(|cell| cell.set(control));
    let first = admin_publish(&m22, &opened, &first);
    log_entries::KEEPS_SUPERSEDED.with(|cell| cell.set(false));
    first.expect("P1");
    let ended = reads(&m22, true, "shared");

    save_records(&m22, &encode_records(&chain)).unwrap();
    let signed = two_passes(&m22);
    convergence::carry(&lab.all, 0);
    let (opens, waiting) = seen.take();
    KeptLate {
        lab,
        opens,
        waiting,
        ended,
        signed,
    }
}

/// One-mesh P3 ([`kept_late`]): a publish that keeps its records after a later head of this Mac
/// settled its hold by descent keeps none, so the person's later edit, drafted from that head's
/// text while the log lacked them, stands. P1's records stay where P2a's landing left them, as a
/// publish cut short after its PUT leaves them: m22's log keeps v1 until the rotation arrives,
/// and a pass then signs P2b's v3 and P2a's o1, nothing is recorded as overtaken, nothing waits,
/// and every Mac reads v3 and o1.
///
/// NEGATIVE CONTROL: [`log_entries::KEEPS_SUPERSEDED`] (P1 keeps its records after the descent, as
/// before), run in the test: v2 enters the log after P2b's draft, whose one base is the log's v1,
/// so the pass drops v3 as overtaken by m22's own v2, which every Mac then reads, as the CONTROL
/// asserts say; with the switch set for the first run too, the test fails "P1's records stay
/// where the descent left them".
#[test]
fn a_publish_whose_own_later_head_settled_its_hold_leaves_the_later_edit_standing() {
    for control in [false, true] {
        let run = kept_late(control);
        let m22 = &run.lab.all[0];
        let v2 = Some(b"v2".to_vec());
        assert_eq!(
            run.opens,
            vec![
                (log_entries::LogBase::Behind, v2.clone()),
                (log_entries::LogBase::Unstamped, v2.clone()),
            ],
            "CONTROL: P2a reads P1's text with the log behind it, P2b P2a's unstamped text"
        );
        assert_eq!(
            run.waiting,
            vec!["other".to_owned(), "shared".to_owned()],
            "CONTROL: P2b's writes wait"
        );
        let journal = log_entries::read_overtaken(m22).unwrap();
        if control {
            assert_eq!(
                run.ended, v2,
                "CONTROL: kept after the descent, v2 enters the log"
            );
            assert_eq!(
                journal
                    .iter()
                    .map(|record| record.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["shared"],
                "CONTROL: the later save is dropped as overtaken"
            );
            assert_eq!(
                journal[0].by.as_ref().map(|by| by.label.as_str()),
                log_of(m22).membership().label(&run.lab.actors[0]),
                "CONTROL: by m22's own earlier write"
            );
            for paths in &run.lab.all[..4] {
                assert_eq!(
                    reads(paths, true, "shared"),
                    v2,
                    "CONTROL: and every Mac reads the earlier save"
                );
            }
        } else {
            assert_eq!(
                run.ended,
                Some(b"v1".to_vec()),
                "P1's records stay where the descent left them"
            );
            assert_eq!(
                everyone_puts(&run.signed),
                2,
                "a pass signs the later save and the imported o1"
            );
            assert!(journal.is_empty(), "nothing is recorded as overtaken");
            assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
            for paths in &run.lab.all[..4] {
                assert_eq!(reads(paths, true, "shared"), Some(b"v3".to_vec()));
                assert_eq!(reads(paths, true, "other"), Some(b"o1".to_vec()));
            }
        }
        run.lab.macs.server.stop();
    }
}

/// What the second publish of [`drafted_in_turn`] did first, while the first was between its
/// draft and its hold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    /// It found the first's turn taken and waited for it ([`log_entries::TURN_WAITS`]).
    Waits,
    /// It drafted (`AFTER_DRAFT`).
    Drafted,
}

/// What [`drafted_in_turn`] saw.
struct InTurn {
    lab: convergence::Six,
    s2: crate::server::Running,
    /// What P2 did first while P1 was between its draft and its hold.
    first: Option<Step>,
    /// The sequence and envelope digest each server holds once both publishes answered: the
    /// first server's, then the second's.
    served: [(u64, [u8; 32]); 2],
    /// What P2 said waits.
    waiting: Vec<String>,
    /// `shared` as m22's log reads it once P1 ended, before any pass.
    kept: Option<Vec<u8>>,
    /// What m22's passes signed once the rotation arrived.
    signed: Vec<Record>,
}

/// One person, two commands at once: m22 opens the vault and publishes `shared` = v2 from it (P1);
/// no rotation is due, so P1's records sign v2. Once P1 drafted, before it holds its records
/// (`AFTER_DRAFT`), m22 takes m7's `Remove` of m42 without its rotation, and a second command on
/// another thread opens the vault, which still serves v1, and publishes `shared` = v3 to a second
/// server (P2), which waits for the new key: it takes its turn, which P1 holds, and drafts once P1
/// held its records and wrote its pending set, or, where `control` sets
/// [`log_entries::DRAFTS_AT_ONCE`] for it, drafts at once. P2 stores its version only once the
/// first server stored P1's (`AFTER_STORE`). Then P1 keeps its records and ends, the rotation
/// arrives, m22's passes run, and every Mac takes its records.
fn drafted_in_turn(control: bool) -> InTurn {
    let lab = convergence::six_with(true);
    let (m22, m7) = (lab.all[0].clone(), lab.all[1].clone());
    let s2 =
        crate::client::log_entries_tests::start_loopback_server(&lab.macs.root.path().join("s2"));
    let url2 = format!("http://{}/v1/alab", s2.addr);
    let base = convergence::ids(&m7);
    test_remove_unposted(&m7, &join_code(&lab.recipients[4])).expect("m7 removes m42");
    let chain = convergence::signed_since(&m7, &base);
    let opened = admin_open(&m22).expect("an admin");
    let mut first = opened.doc.clone();
    set(&mut first, "shared", b"v2");

    let (step_tx, step_rx) = std::sync::mpsc::channel::<Step>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let p2: Rc<RefCell<Option<std::thread::JoinHandle<Vec<String>>>>> = Rc::new(RefCell::new(None));
    let started = Rc::clone(&p2);
    let first_step = Rc::new(Cell::new(None));
    let saw = Rc::clone(&first_step);
    let (on, remove) = (m22.clone(), encode_records(&chain[..1]));
    AFTER_DRAFT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            save_records(&on, &remove).unwrap();
            let handle = std::thread::spawn(move || {
                let view = admin_open(&on).expect("P2 opens");
                assert_eq!(
                    (view.log_base, shows(&view, "shared")),
                    (log_entries::LogBase::Covers, Some(b"v1".to_vec())),
                    "CONTROL: P2 opens the version P1 was drafted from"
                );
                let mut second = view.doc.clone();
                set(&mut second, "shared", b"v3");
                let waits = step_tx.clone();
                log_entries::TURN_WAITS.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        let _ = waits.send(Step::Waits);
                    }));
                });
                AFTER_DRAFT.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        let _ = step_tx.send(Step::Drafted);
                        go_rx
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .expect("P1 stored its version");
                    }));
                });
                log_entries::DRAFTS_AT_ONCE.with(|cell| cell.set(control));
                admin_publish_with_candidates(&on, &view, &second, &[url2])
                    .expect("P2: the second server stores it")
                    .waiting
            });
            saw.set(Some(
                step_rx
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .expect("P2 tried to draft"),
            ));
            *started.borrow_mut() = Some(handle);
        }));
    });
    let joined = Rc::clone(&p2);
    let waiting = Rc::new(RefCell::new(None));
    let said = Rc::clone(&waiting);
    AFTER_STORE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            go_tx.send(()).unwrap();
            let handle = joined.borrow_mut().take().expect("P2 started");
            *said.borrow_mut() = Some(handle.join().expect("P2's thread"));
        }));
    });
    admin_publish(&m22, &opened, &first).expect("P1");
    let waiting = waiting
        .borrow_mut()
        .take()
        .expect("CONTROL: P2 ended in P1's window");
    let served = [
        served_at(&lab.macs.url),
        served_at(&format!("http://{}/v1/alab", s2.addr)),
    ];
    let kept = reads(&m22, true, "shared");

    save_records(&m22, &encode_records(&chain)).unwrap();
    let signed = two_passes(&m22);
    convergence::carry(&lab.all, 0);
    InTurn {
        lab,
        s2,
        first: first_step.get(),
        served,
        waiting,
        kept,
        signed,
    }
}

/// One-mesh P3 ([`drafted_in_turn`]): a publish drafts in its turn, so a waiting write drafted
/// while another publish of this Mac is between its draft and its hold, whose records are then in
/// neither the log nor a hold, is not drafted until that hold is written. P2 tries to draft while
/// P1 holds its turn, waits for it, then reads P1's hold, so its write's bases hold v2; once P1
/// keeps its records and the rotation arrives, a pass signs v3, the person's later save, nothing
/// is recorded as overtaken, and every Mac reads v3.
///
/// NEGATIVE CONTROL: [`log_entries::DRAFTS_AT_ONCE`] (a publish takes no turn, as before), run in
/// the test: P2 drafts before P1's hold is written, its write's one base is the log's v1, and the
/// pass drops v3 as overtaken by m22's own v2, which every Mac then reads, as the CONTROL asserts
/// say; with the switch set for the first run too, the test fails "P2 waits for P1's turn".
#[test]
fn a_waiting_write_drafted_while_its_own_earlier_publish_drafts_is_signed() {
    for control in [false, true] {
        let run = drafted_in_turn(control);
        let m22 = &run.lab.all[0];
        assert_eq!(
            run.served[0].0, run.served[1].0,
            "CONTROL: both publishes are of one sequence"
        );
        assert_ne!(
            run.served[0].1, run.served[1].1,
            "CONTROL: the first server holds P1, the second P2"
        );
        assert_eq!(
            run.waiting,
            vec!["shared".to_owned()],
            "CONTROL: P2's write waits"
        );
        assert_eq!(
            run.kept,
            Some(b"v2".to_vec()),
            "CONTROL: P1's records are kept before any pass"
        );
        let journal = log_entries::read_overtaken(m22).unwrap();
        if control {
            assert_eq!(
                run.first,
                Some(Step::Drafted),
                "CONTROL: P2 drafts before P1 holds its records"
            );
            assert_eq!(
                everyone_puts(&run.signed),
                0,
                "CONTROL: drafted at once, nothing is signed"
            );
            assert_eq!(
                journal
                    .iter()
                    .map(|record| record.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["shared"],
                "CONTROL: the later save is dropped as overtaken"
            );
            assert_eq!(
                journal[0].by.as_ref().map(|by| by.label.as_str()),
                log_of(m22).membership().label(&run.lab.actors[0]),
                "CONTROL: by m22's own earlier write"
            );
            for paths in &run.lab.all[..4] {
                assert_eq!(
                    reads(paths, true, "shared"),
                    Some(b"v2".to_vec()),
                    "CONTROL: and every Mac reads the earlier save"
                );
            }
        } else {
            assert_eq!(run.first, Some(Step::Waits), "P2 waits for P1's turn");
            assert_eq!(
                everyone_puts(&run.signed),
                1,
                "a pass signs the person's later save"
            );
            assert!(journal.is_empty(), "nothing is recorded as overtaken");
            assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
            for paths in &run.lab.all[..4] {
                assert_eq!(reads(paths, true, "shared"), Some(b"v3".to_vec()));
            }
        }
        run.s2.stop();
        run.lab.macs.server.stop();
    }
}

/// One-mesh P3's keep in [`log_entries::SINGLE_SLOT_HOLD`]'s mode, the negative control of
/// per-version holds: its one hold goes at any other publish's hold or answer, so a publish whose
/// hold another replaced keeps its records once a server took its version, as before
/// per-version holds, not as a hold settled by descent. m22 publishes `shared` = v2 (P1); the
/// server stores it, and before P1 keeps its records (`AFTER_STORE`) a second publish from the
/// same opened view sets `other` = o1: its hold replaces P1's, and the server refuses it as not
/// newer. P1 then keeps its records: m22's log reads v2.
#[test]
fn the_single_slot_control_keeps_a_version_whose_hold_another_publish_replaced() {
    let lab = convergence::six_with(true);
    let m22 = lab.all[0].clone();
    let opened = Rc::new(admin_open(&m22).expect("an admin"));
    let mut first = opened.doc.clone();
    set(&mut first, "shared", b"v2");
    let mut second = opened.doc.clone();
    set(&mut second, "other", b"o1");
    let holds = |paths: &Paths| {
        std::fs::read_dir(paths.dir.join("records.drafts"))
            .map(|dir| dir.count())
            .unwrap_or(0)
    };

    let left = Rc::new(Cell::new(None));
    let saw = Rc::clone(&left);
    let (on, view) = (m22.clone(), Rc::clone(&opened));
    AFTER_STORE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            assert_eq!(holds(&on), 1, "CONTROL: P1's hold is there");
            let refused = admin_publish(&on, &view, &second).expect_err("P2 is not newer than P1");
            assert_eq!(refused.kind(), crate::ErrorKind::NotStored, "{refused}");
            saw.set(Some(holds(&on)));
        }));
    });
    log_entries::SINGLE_SLOT_HOLD.with(|cell| cell.set(true));
    let first = admin_publish(&m22, &opened, &first);
    log_entries::SINGLE_SLOT_HOLD.with(|cell| cell.set(false));
    first.expect("P1");
    assert_eq!(
        left.get(),
        Some(0),
        "CONTROL: P2's hold replaced P1's and its refusal forgot it"
    );
    assert_eq!(
        reads(&m22, true, "shared"),
        Some(b"v2".to_vec()),
        "P1 keeps its records with its one hold gone"
    );
    lab.macs.server.stop();
}

/// One-mesh P3 under W3's rule: what this Mac cannot read is never read as none. A held draft
/// that does not read leaves values nobody can tell (`held_values`), so a save
/// whose `[everyone]` writes wait refuses over it, as over unread waiting writes, rather than
/// sign them over the log's value alone.
///
/// NEGATIVE CONTROL: [`log_entries::UNREAD_AS_NONE`] (the hold reads as leaving nothing), run in
/// the test: the save goes on and its write waits, as the CONTROL assert says.
#[test]
fn a_waiting_write_is_never_drafted_over_a_hold_that_does_not_read() {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let lab = convergence::six_with(true);
    let m22 = lab.all[0].clone();
    let _chain = held_remove(&lab);
    let dir = m22.dir.join("records.drafts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join(format!("{:020}-{}", 1, "ab".repeat(32))))
        .unwrap()
        .write_all(b"no held draft")
        .unwrap();
    let admin = admin_open(&m22).expect("an admin");
    let mut doc = admin.doc.clone();
    set(&mut doc, "shared", b"v3");

    let refused = admin_publish(&m22, &admin, &doc).expect_err("the save refuses");
    assert!(
        refused
            .to_string()
            .contains("no [everyone] write waits over this Mac's held drafts until they read"),
        "{refused}"
    );
    assert!(!log_entries::waiting_file(&m22).exists(), "nothing waits");

    log_entries::UNREAD_AS_NONE.with(|cell| cell.set(true));
    let control = admin_publish(&m22, &admin, &doc);
    log_entries::UNREAD_AS_NONE.with(|cell| cell.set(false));
    assert_eq!(
        control
            .expect("CONTROL: read as none, the save goes on")
            .waiting,
        vec!["shared".to_owned()],
        "CONTROL: and its write waits"
    );
    lab.macs.server.stop();
}
