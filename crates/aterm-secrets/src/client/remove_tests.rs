// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::log_entries_tests::{
    LabTemplate, TwoMacs, copy_private_tree, hand, join_host, member_key, off_this_thread,
    two_macs, two_macs_with,
};
use crate::commons::bootstrap::Bootstrap;
use crate::membership_log::encode_records;

#[path = "convergence_tests.rs"]
mod convergence;

#[path = "gap_heal_conformance_tests.rs"]
mod gap_heal;

#[path = "capacity_tests.rs"]
mod capacity;

/// One-mesh W3's tests of `log_entries/waiting.rs`, beside it, declared here for this lab.
#[path = "log_entries/waiting_tests.rs"]
mod waiting;

/// P3's binding (3) for waiting writes, beside them, declared here for this lab.
#[path = "log_entries/waiting_conformance_tests.rs"]
mod waiting_conformance;

/// Every clock in the log-derived envelope survives persistence, including maximum
/// timestamps: every held record an epoch (a due, a seen and a wait clock each), and half of
/// them `Remove`s instead (a removal clock each). The former 64 KiB reader refuses either file
/// and forgets every grace.
#[test]
fn tender_clocks_survive_the_full_record_bound() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let held = u64::try_from(crate::membership_log::MAX_HELD_RECORDS).unwrap();
    for epochs in [held, held / 2] {
        let mut mark = TendMark::default();
        for i in 0..epochs {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&i.to_le_bytes());
            mark.due.insert(id, u64::MAX);
            mark.seen.insert(id, (u64::MAX, u64::MAX));
            mark.wait.insert(id, (u64::MAX, u64::MAX));
        }
        for i in epochs..held {
            let mut target = [0xff; 48];
            target[..8].copy_from_slice(&i.to_le_bytes());
            mark.removal.insert(target, u64::MAX);
        }
        write_tend_mark(&paths, &mark).unwrap();
        let path = epoch_mark_file(&paths);
        assert!(fsx::read_private(&path, 64 * 1024).is_err());
        assert!(std::fs::metadata(&path).unwrap().len() <= u64::try_from(MAX_TEND_MARK).unwrap());
        let read = read_tend_mark(&paths);
        assert_eq!(read.due, mark.due);
        assert_eq!(read.seen, mark.seen);
        assert_eq!(read.wait, mark.wait);
        assert_eq!(read.removal, mark.removal);
        assert_eq!(
            read.removal.len(),
            usize::try_from(held - epochs).unwrap(),
            "CONTROL: the removal clocks fill what the epochs leave"
        );
    }
}

/// A Mac that asks `member` to join under `name` from its own home `dir`: [`newcomer`] with the
/// home apart from the name, so two Macs can ask under one name.
fn asker(
    root: &Path,
    member: &Paths,
    dir: &str,
    name: &str,
) -> (
    Paths,
    crate::commons::binding::Request,
    crate::members::Allowed,
) {
    let paths = Paths::for_home(&root.join(dir));
    machine_identity(&paths).expect("identity");
    let device = Device::open(&paths).expect("device").expect("an identity");
    let trust = Bootstrap::required(member).expect("member").trust();
    let request = device
        .request(trust.network, trust.authority)
        .expect("request");
    let allowed = Bootstrap::required(member)
        .expect("member")
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request.binding(), name, seed))
        .expect("allowed");
    (paths, request, allowed)
}

/// `m7` lets in a Mac asking under `name` by the log, and posts the records so `m22` can pull
/// them. Returns the Mac's home, actor and age recipient.
fn let_in(macs: &TwoMacs, dir: &str, name: &str) -> (Paths, MemberId, Recipient) {
    let admitted = let_in_by(macs, &macs.m7, dir, name);
    post_records_to_servers(&macs.m7).expect("m7 posts the admission");
    admitted
}

/// `member` lets in a Mac asking under `name` by the log, keeping the records to itself. Returns
/// the Mac's home, actor and age recipient.
fn let_in_by(
    macs: &TwoMacs,
    member: &Paths,
    dir: &str,
    name: &str,
) -> (Paths, MemberId, Recipient) {
    let (paths, request, allowed) = asker(macs.root.path(), member, dir, name);
    let (bundle, logged) = hand(member, &request, &allowed, true);
    assert!(logged, "CONTROL: the member hands the log path");
    write_from_log_bundle(&paths, &bundle).expect("the Mac writes from the log");
    (
        paths,
        request.binding().actor(),
        request.binding().recipient(),
    )
}

fn log_of(paths: &Paths) -> Log {
    load_records(paths)
        .expect("records")
        .log()
        .expect("a log")
        .clone()
}

/// The members' age recipients, as `paths`' log evaluates them.
fn active(paths: &Paths) -> BTreeSet<Recipient> {
    log_of(paths)
        .membership()
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .collect()
}

/// Every epoch of `audience` some `Wrap` in `log` hands to `recipient`, whether or not the log
/// still counts that `Wrap`: what a Mac holding `recipient`'s identity can open from the records,
/// once removed as before.
fn epochs_wrapped_to(log: &Log, recipient: &Recipient) -> BTreeSet<RecordId> {
    log.records()
        .filter_map(|r| match r.body() {
            Body::Wrap {
                epoch,
                recipient: to,
                ..
            } if to == recipient => Some(*epoch),
            _ => None,
        })
        .collect()
}

/// F16: m7, a plain member, removes m40 by its label, and the next value any member puts goes
/// under an epoch no `Wrap` ever handed m40: m40, holding every record and its own identity, has
/// no key for it, whatever the log's evaluation says of it. The removal names what m40 could read,
/// for rotation at the provider, rotates every audience m40 was in, and writes the cascade
/// (m40's roster row to the deny-lists) into the `[everyone]` reseal.
///
/// NEGATIVE CONTROL: in `rotate_after_removal`, skip the `[everyone]` epoch (`if false &&
/// opened.max_generation…`): the newest `[everyone]` epoch is still the one m7 wrapped to m40 on
/// admission, m7's put goes under it, and the `"no Wrap hands m40 the epoch"` assert fails.
#[test]
fn a_removed_mac_cannot_read_a_value_put_after_its_removal() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let (_m40, actor40, rec40) = let_in(&macs, "m40", "m40");
    let before = log_of(m7);
    let id7 = read_identity(m7).unwrap().unwrap();
    let opened = before.open_audience_entries(&id7).unwrap().unwrap();
    let old_newest = opened.maximal_epochs[&EpochAudience::Everyone].clone();
    assert!(
        old_newest
            .iter()
            .all(|id| epochs_wrapped_to(&before, &rec40).contains(id)),
        "CONTROL: m40 holds the newest [everyone] epoch before its removal"
    );

    let label = before.membership().get(&actor40).unwrap().label.clone();
    let removal = remove_member(m7, &label, false).expect("m7 removes m40");

    let after = log_of(m7);
    let membership = after.membership();
    let gone = membership.removal(&actor40).expect("the log removed m40");
    let (_, m7_actor, _) = member_key(m7);
    assert_eq!(gone.by, BTreeSet::from([m7_actor]));
    assert!(!gone.quorum);

    // m7 puts a new value under the newest `[everyone]` epoch it holds, as any writer does.
    let opened = after.open_audience_entries(&id7).unwrap().unwrap();
    let newest = opened.maximal_epochs[&EpochAudience::Everyone].clone();
    assert_eq!(newest.len(), 1, "one newest [everyone] epoch");
    let epoch = newest[0];
    let key: [u8; 32] = *opened.unwrapped_keys[&epoch];
    let (b7, a7, i7) = member_key(m7);
    let put = b7
        .device()
        .with_seed(|seed| {
            after
                .draft_put(
                    &a7,
                    aterm_time::unix_millis().unwrap_or(0),
                    epoch,
                    &key,
                    &Entry {
                        name: "after".to_owned(),
                        value: SecretBytes::from(b"put-after-the-removal".as_slice()),
                        audience: Audience::Everyone,
                        if_absent: false,
                        anchor: None,
                    },
                )
                .unwrap()
                .sign(seed, i7)
        })
        .unwrap();
    save_records(m7, &encode_records(std::slice::from_ref(&put))).unwrap();
    let held = log_of(m7);
    assert!(
        !epochs_wrapped_to(&held, &rec40).contains(&epoch),
        "no Wrap hands m40 the epoch the new value is put under"
    );
    let opened = held.open_audience_entries(&id7).unwrap().unwrap();
    assert_eq!(
        opened.by_audience[&Audience::Everyone]["after"]
            .value
            .as_slice(),
        b"put-after-the-removal",
        "CONTROL: the members read it"
    );
    assert_eq!(
        opened.by_audience[&Audience::Everyone]["shared"]
            .value
            .as_slice(),
        b"everyone-v1",
        "the reseal carried the value m40 could read"
    );
    assert!(removal.signed >= 3, "{removal:?}");
    assert!(
        removal.rotated.iter().any(|a| a == "everyone"),
        "{removal:?}"
    );
    assert!(
        removal.rotated.iter().any(|a| a == "commons"),
        "{removal:?}"
    );
    assert!(
        removal.could_read.iter().any(|n| n == "shared"),
        "{removal:?}"
    );
    assert!(removal.needs_you.is_none(), "a plain removal asks nobody");
    assert!(!removal.fleet_code_rotated);

    macs.server.stop();
}

/// F16: a removal names one Mac. Two Macs that asked under one host name both carry it (the log
/// gives the second the next free label), so a removal by that name is refused with each Mac's
/// label and join code, and one is removed by its code alone. A Mac does not remove itself, and a
/// Mac removed already is said to be.
///
/// NEGATIVE CONTROL: in `resolve`, take the first match (`[first, ..] => Ok(...)` before the
/// `many` arm): `remove_member(m7, "m40")` removes one of the two and the `"m40 names two Macs"`
/// expect_err fails.
#[test]
fn remove_by_label_refuses_a_label_naming_two_macs() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m7 = &macs.m7;
    let (_a, actor_a, rec_a) = let_in(&macs, "m40-a", "m40");
    let (_b, actor_b, _) = let_in(&macs, "m40-b", "m40");
    assert_ne!(actor_a, actor_b, "CONTROL: two Macs");

    let error = remove_member(m7, "m40", false).expect_err("m40 names two Macs");
    assert!(error.to_string().contains("names 2 Macs"), "{error}");
    let membership = log_of(m7).membership();
    assert!(membership.contains(&actor_a) && membership.contains(&actor_b));

    let error = remove_member(m7, "m7", false).expect_err("m7 does not remove itself");
    assert!(error.to_string().contains("itself"), "{error}");

    remove_member(m7, &join_code(&rec_a), false).expect("removed by its join code");
    let membership = log_of(m7).membership();
    assert!(membership.removal(&actor_a).is_some());
    assert!(membership.contains(&actor_b), "the other Mac stays in");

    let error = remove_member(m7, &join_code(&rec_a), false).expect_err("removed already");
    assert!(error.to_string().contains("removed already"), "{error}");

    macs.server.stop();
}

/// B12: a person's removal of the seat B2's resolver named (`remove_seat`) takes out that one
/// Mac where the word names two: m7 lets in two Macs labelled m40, the word `m40` is refused, the
/// seat of one is removed and the other stays in, a second removal of it is refused as removed
/// already and signs nothing, and m7's own seat is refused.
///
/// NEGATIVE CONTROL: in `named_target`, drop the `Target::Seat(_) if membership.removal(..)` arm:
/// the repeat is refused as no member, and the "removed already" assert fails.
#[test]
fn a_person_removes_the_seat_the_resolver_named_where_the_word_names_two_macs() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m7 = &macs.m7;
    let (_a, actor_a, _) = let_in(&macs, "m40-a", "m40");
    let (_b, actor_b, _) = let_in(&macs, "m40-b", "m40");
    assert_ne!(actor_a, actor_b, "CONTROL: two Macs");
    let error = remove_member(m7, "m40", false).expect_err("m40 names two Macs");
    assert!(error.to_string().contains("names 2 Macs"), "{error}");

    let removal = remove_seat(m7, actor_a, false).expect("m7 removes the seat it was given");
    assert!(removal.signed > 0, "{removal:?}");
    assert!(removal.label.starts_with("m40"), "{}", removal.label);
    let membership = log_of(m7).membership();
    assert!(membership.removal(&actor_a).is_some());
    assert!(membership.contains(&actor_b), "the other m40 stays in");

    let held = load_records(m7).expect("records").inventory();
    let error = remove_seat(m7, actor_a, false).expect_err("removed already");
    assert_eq!(
        error.to_string(),
        format!("{} is removed already", removal.label)
    );
    let (_, own, _) = member_key(m7);
    let error = remove_seat(m7, own, false).expect_err("m7 does not remove itself");
    assert!(error.to_string().contains("itself"), "{error}");
    assert_eq!(
        load_records(m7).expect("records").inventory(),
        held,
        "a refused seat signs nothing"
    );
    macs.server.stop();
}

/// B12: a person's removal cuts tables only for a seat it removes. The recording hook sees m40's
/// cut once, before its `Remove`, while m40 is still seated; it sees nothing for the repeat
/// (removed already) or for m7's own seat, both refused.
///
/// NEGATIVE CONTROL: in `cut_tables_first`, make the first arm
/// `Target::Actor(actor) | Target::Seat(actor) => actor`: the repeat's Share cut and m7's own
/// cut are recorded, and the assert fails.
#[test]
fn a_refused_seat_signs_no_table_cutoff() {
    on_table_cuts(record_table_cuts);
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m7 = &macs.m7;
    let (_a, actor_a, _) = let_in(&macs, "m40", "m40");
    let (_, own, _) = member_key(m7);
    let calls = || -> Vec<(MemberId, CutPass, bool)> {
        TABLE_CUT_CALLS
            .lock()
            .unwrap()
            .iter()
            .filter(|(dir, ..)| *dir == m7.dir)
            .map(|(_, member, pass, seated)| (*member, *pass, *seated))
            .collect()
    };
    assert!(calls().is_empty(), "CONTROL: nothing cut on m7 yet");
    remove_seat(m7, actor_a, false).expect("m7 removes m40's seat");
    remove_seat(m7, actor_a, false).expect_err("removed already");
    remove_seat(m7, own, false).expect_err("m7 does not remove itself");
    assert_eq!(
        calls(),
        vec![(actor_a, CutPass::BeforeRemove, true)],
        "only the removed seat is cut, once, before its Remove"
    );
    macs.server.stop();
}

/// B12: a seat's removal is a person's, so it takes out the Mac, every seat of its age recipient,
/// as the word's does: a Mac seated twice (a second device key under the same secrets identity,
/// let in on two sides of a split, since a desk refuses a key another seat binds) leaves the log
/// whole when a person removes one seat.
///
/// NEGATIVE CONTROL: in `draft_removal`, make `by_person` `matches!(target, Target::Word(_))`:
/// the twin's seat stays in, and the "every seat of the recipient" assert fails.
#[test]
fn a_seats_removal_takes_every_seat_of_its_recipient() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (a, actor_a, rec_a) = let_in_by(&macs, m7, "m40-a", "m40");
    let twin = Paths::for_home(&macs.root.path().join("m40-twin"));
    fsx::ensure_private_dir(&twin.dir).expect("twin dir");
    let key = fsx::read_private(&a.identity_file(), 16 << 10)
        .expect("read")
        .expect("m40's key");
    fsx::write_atomic(&twin.identity_file(), key.as_slice(), 0o600).expect("twin key");
    let (_, actor_twin, rec_twin) = let_in_by(&macs, m22, "m40-twin", "m41");
    save_records(m7, &load_records(m22).expect("m22's records").encode()).expect("the heal");
    assert_eq!(rec_a, rec_twin, "CONTROL: one secrets identity");
    assert_ne!(actor_a, actor_twin, "CONTROL: two seats");
    let membership = log_of(m7).membership();
    assert!(membership.contains(&actor_a) && membership.contains(&actor_twin));

    remove_seat(m7, actor_a, false).expect("m7 removes m40's seat");
    let membership = log_of(m7).membership();
    assert!(
        membership.removal(&actor_a).is_some() && membership.removal(&actor_twin).is_some(),
        "every seat of the recipient is removed"
    );
    macs.server.stop();
}

/// B12: a person's removal on a Mac seated twice refuses the Mac's other seat as itself: the
/// removal takes out every seat of the age recipient and revokes it, so m40's removal of its twin
/// (by seat or by the twin's label) would take m40 out too. Both are refused and sign nothing.
///
/// NEGATIVE CONTROL: in `named_target`, make the recipient check `false && …`: the seat's
/// removal signs, and the `expect_err` fails.
#[test]
fn a_person_does_not_remove_this_macs_twin_seat() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (a, actor_a, rec_a) = let_in_by(&macs, m7, "m40-a", "m40");
    let twin = Paths::for_home(&macs.root.path().join("m40-twin"));
    fsx::ensure_private_dir(&twin.dir).expect("twin dir");
    let key = fsx::read_private(&a.identity_file(), 16 << 10)
        .expect("read")
        .expect("m40's key");
    fsx::write_atomic(&twin.identity_file(), key.as_slice(), 0o600).expect("twin key");
    let (_, actor_twin, rec_twin) = let_in_by(&macs, m22, "m40-twin", "m41");
    save_records(m7, &load_records(m22).expect("m22's records").encode()).expect("the heal");
    save_records(&a, &load_records(m7).expect("m7's records").encode()).expect("m40 heals");
    assert_eq!(rec_a, rec_twin, "CONTROL: one secrets identity");
    let membership = log_of(&a).membership();
    assert!(
        membership.contains(&actor_a) && membership.contains(&actor_twin),
        "CONTROL: m40's log seats both"
    );
    let twin_label = membership
        .get(&actor_twin)
        .expect("twin seat")
        .label
        .clone();

    let held = load_records(&a).expect("records").inventory();
    let error = remove_seat(&a, actor_twin, false).expect_err("m40 does not remove its twin");
    assert!(error.to_string().contains("itself"), "{error}");
    let error = remove_member(&a, &twin_label, false).expect_err("nor by the twin's label");
    assert!(error.to_string().contains("itself"), "{error}");
    assert_eq!(
        load_records(&a).expect("records").inventory(),
        held,
        "a refused twin signs nothing"
    );
    macs.server.stop();
}

/// F16: on two sides of a split, m22 removes m40 and m7 removes m41; each mints its own next
/// `[everyone]` and Commons epoch. After the heal the vault pass's epoch tender on one Mac (the
/// first ranked of the two removers) mints one higher epoch of each covering both removals, the
/// other Mac waits its rank's step and then has nothing to do, and the newest epochs are wrapped
/// to neither removed Mac.
///
/// NEGATIVE CONTROL: make `coalesce_concurrent_commons_epochs` return `Ok(())` at once: neither
/// Mac signs, two `[everyone]` epochs stay in use, and the `"one Mac mints"` assert fails.
#[test]
fn concurrent_removals_mint_one_higher_epoch_covering_both() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_m40, actor40, rec40) = let_in(&macs, "m40", "m40");
    let (_m41, actor41, rec41) = let_in(&macs, "m41", "m41");
    sync_records(m22, None).expect("m22 pulls both admissions");
    assert!(active(m22).contains(&rec40) && active(m22).contains(&rec41));

    // The split: each side removes one Mac and keeps its records to itself.
    let split = log_of(m22).heads();
    remove_member(m22, &join_code(&rec40), false).expect("m22 removes m40");
    remove_member(m7, &join_code(&rec41), false).expect("m7 removes m41");
    assert_ne!(log_of(m22).heads(), split, "CONTROL: m22 signed");

    // The heal: each takes the other's records.
    let wire22 = load_records(m22).unwrap().encode();
    let wire7 = load_records(m7).unwrap().encode();
    save_records(m22, &wire7).unwrap();
    save_records(m7, &wire22).unwrap();
    let healed = log_of(m22);
    let membership = healed.membership();
    assert!(membership.removal(&actor40).is_some() && membership.removal(&actor41).is_some());
    let id22 = read_identity(m22).unwrap().unwrap();
    let opened = healed.open_audience_entries(&id22).unwrap().unwrap();
    let mut top = BTreeMap::new();
    for audience in [EpochAudience::Everyone, EpochAudience::Commons] {
        assert_eq!(
            opened.maximal_epochs[&audience].len(),
            2,
            "CONTROL: two newest {audience:?} epochs after the heal"
        );
        top.insert(audience.clone(), opened.max_generation[&audience]);
    }
    let active_now = active(m22);
    let (_, actor22, _) = member_key(m22);
    let (_, actor7, _) = member_key(m7);
    for due in healed.rekey_dues() {
        assert_eq!(
            due.removers,
            BTreeSet::from([actor22, actor7]),
            "CONTROL: both removers rank first for {:?}",
            due.audience
        );
    }
    assert!(!healed.rekey_dues().is_empty());

    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let first22 = tend_epochs_at(m22, t0).unwrap();
    let first7 = tend_epochs_at(m7, t0).unwrap();
    assert!(
        first22.is_empty() != first7.is_empty(),
        "one Mac mints at first sight, the other waits its rank's step: {} and {}",
        first22.len(),
        first7.len()
    );
    let (minter, waiter, signed) = if first22.is_empty() {
        (m7, m22, first7)
    } else {
        (m22, m7, first22)
    };
    save_records(waiter, &encode_records(&signed)).unwrap();
    let later = t0 + 40 * log_entries::GAP_STEP_MS;
    assert!(
        tend_epochs_at(waiter, later).unwrap().is_empty(),
        "the other Mac, past its step, finds the epochs covered"
    );
    assert!(tend_epochs_at(minter, later).unwrap().is_empty());

    let covered = log_of(waiter);
    let id = read_identity(waiter).unwrap().unwrap();
    let opened = covered.open_audience_entries(&id).unwrap().unwrap();
    for (audience, generation) in &top {
        let newest = &opened.maximal_epochs[audience];
        assert_eq!(newest.len(), 1, "one newest {audience:?} epoch");
        assert_eq!(
            opened.max_generation[audience],
            generation + 1,
            "one higher {audience:?} epoch"
        );
        let wrapped = &opened.wrapped_recipients[&newest[0]];
        assert!(!wrapped.contains(&rec40) && !wrapped.contains(&rec41));
        assert!(active_now.is_subset(wrapped), "every member holds it");
    }
    assert!(covered.rekey_dues().is_empty());

    macs.server.stop();
}

/// m22 signs its share of the quorum `Remove` of `target` at the cutoff m7's plain `Remove` of it
/// named, and m7 keeps it: two of the voters, a quorum.
fn second_share(macs: &TwoMacs, target: MemberId) {
    let held = log_of(&macs.m7);
    let cutoff = held
        .records()
        .find_map(|r| match r.body() {
            Body::Remove { member, cutoff } if *member == target => Some(*cutoff),
            _ => None,
        })
        .expect("m7's Remove");
    let (b22, a22, i22) = member_key(&macs.m22);
    let share = b22
        .device()
        .with_seed(|seed| {
            held.draft(
                &a22,
                aterm_time::unix_millis().unwrap_or(0),
                Body::Remove {
                    member: target,
                    cutoff,
                },
            )
            .sign(seed, i22)
        })
        .unwrap();
    save_records(&macs.m7, &encode_records(std::slice::from_ref(&share))).unwrap();
}

/// F16: a Mac m7 removed that asks m7 again shows on m7's desk as removed, by whom and when, not
/// as a new Mac. A plain Allow is refused with the reason, "Allow again" gives a phrase, and its
/// run signs a restoring `Admit` under the Mac's old label. A Mac a quorum removed shows as
/// removed by a quorum, and neither Allow brings it back.
///
/// NEGATIVE CONTROL: in `Local::of`, leave `removed` empty (`client::RemovedNotes::default()`):
/// m40's row carries no note and reads as a new Mac asking to join (the ghost returns), and the
/// `"m40 shows as removed"` expect fails.
#[test]
fn a_removed_mac_asking_again_shows_as_removed_not_new() {
    use crate::members::desk::{Asking, Desk, Handing, Local};
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m7 = &macs.m7;
    let (m41, request41, allowed41) = asker(macs.root.path(), m7, "m41", "m41");
    let (bundle41, _) = hand(m7, &request41, &allowed41, true);
    write_from_log_bundle(&m41, &bundle41).unwrap();
    let (m40, request, allowed) = asker(macs.root.path(), m7, "m40", "m40");
    let (bundle, _) = hand(m7, &request, &allowed, true);
    write_from_log_bundle(&m40, &bundle).unwrap();
    let actor40 = request.binding().actor();
    let actor41 = request41.binding().actor();
    let label = log_of(m7).membership().get(&actor40).unwrap().label.clone();

    // m41 goes by a quorum (m7 and m22), then m40 by m7 alone.
    remove_member(m7, &join_code(&request41.binding().recipient()), false).unwrap();
    second_share(&macs, actor41);
    remove_member(m7, &label, false).unwrap();
    let membership = log_of(m7).membership();
    assert!(
        membership.removal(&actor41).is_some_and(|r| r.quorum),
        "CONTROL: a quorum removed m41"
    );
    assert!(membership.removal(&actor40).is_some_and(|r| !r.quorum));

    let mut desk = Desk::new(Local::of(m7).unwrap().expect("m7 is a member"));
    let now = Instant::now();
    for (paths, request, name) in [(&m40, &request, "m40"), (&m41, &request41, "m41")] {
        let device = Device::open(paths).unwrap().unwrap();
        let key = *desk.local().key();
        let signature = crate::members::joiner::ask_signature(
            &device,
            &key,
            &[0x40; 32],
            name,
            "Mac15,3",
            request.bytes(),
        )
        .unwrap();
        let asking = Asking {
            wire: request.bytes(),
            node: [0x40; 32],
            name,
            model: "Mac15,3",
            signature: &signature,
        };
        desk.hear(&asking, "192.0.2.40".parse().unwrap(), now)
            .expect("heard");
    }
    let rows = desk.rows(now);
    let row = rows.iter().find(|r| r.name == "m40").unwrap().clone();
    let note = row.removed.clone().expect("m40 shows as removed");
    assert_eq!(note.label, label);
    assert_eq!(note.by, vec!["m7".to_owned()]);
    assert!(!note.quorum);
    let title = note.title();
    assert!(
        title.starts_with(&format!("{label} was removed by m7 on "))
            && title.ends_with(". Allow again?"),
        "{title}"
    );
    let ghost = rows.iter().find(|r| r.name == "m41").unwrap().clone();
    assert!(
        ghost.removed.as_ref().is_some_and(|n| n.quorum),
        "m41 shows as removed by a quorum"
    );

    let error = desk
        .allow(row.id, now)
        .expect_err("a plain Allow of a removed Mac");
    assert!(error.to_string().contains("Allow again"), "{error}");
    let error = desk
        .allow_again(ghost.id, now)
        .expect_err("Allow again of a Mac a quorum removed");
    assert!(error.to_string().contains("quorum"), "{error}");
    desk.allow_again(row.id, now)
        .expect("Allow again gives a phrase");
    assert!(desk.rows(now).iter().any(|r| r.id == row.id && r.restore));

    // The run: without the person's "Allow again" the log keeps m40 out; with it, m7 signs a
    // restoring Admit under m40's old label. Nothing restores m41.
    let handing = |request, allowed, restore| Handing {
        request,
        node: [0x40; 32],
        name: "m40",
        allowed,
        reader: crate::members::bundle::Reader::LegacyLog,
        restore,
        node_proven: false,
        held: None,
    };
    let error = hand_over(m7, &handing(&request, &allowed, false))
        .err()
        .expect("kept out without Allow again");
    assert!(error.to_string().contains("Allow again"), "{error}");
    let error = hand_over(m7, &handing(&request41, &allowed41, true))
        .err()
        .expect("a quorum's removal keeps m41 out");
    assert!(error.to_string().contains("quorum"), "{error}");
    let handout = hand_over(m7, &handing(&request, &allowed, true)).expect("m7 restores m40");
    assert!(handout.logged);
    let membership = log_of(m7).membership();
    let back = membership.get(&actor40).expect("m40 is in again");
    assert_eq!(back.label, label, "under its old label");
    assert!(membership.removal(&actor41).is_some(), "m41 stays out");

    macs.server.stop();
}

#[test]
fn an_older_restore_cannot_undo_the_current_rosters_later_removal() {
    use crate::members::desk::Handing;

    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    admin_seed_swarm_roster(&macs.m22, &[]).unwrap();
    sync(&macs.m7, None).unwrap();
    let paths = &macs.m7;
    let (newcomer, request, allowed) = asker(macs.root.path(), paths, "m40", "m40");
    let (bundle, logged) = hand(paths, &request, &allowed, true);
    assert!(logged);
    write_from_log_bundle(&newcomer, &bundle).unwrap();
    let actor = request.binding().actor();
    let label = log_of(paths)
        .membership()
        .get(&actor)
        .unwrap()
        .label
        .clone();
    remove_member(paths, &label, false).unwrap();
    let restored = hand_over(
        paths,
        &Handing {
            request: &request,
            node: [0x40; 32],
            name: "m40",
            allowed: &allowed,
            reader: crate::members::bundle::Reader::LegacyLog,
            restore: true,
            node_proven: false,
            held: None,
        },
    )
    .unwrap();
    assert!(restored.logged);
    let before_second_removal = log_of(paths);
    assert!(
        roster::restored_by_log(&before_second_removal)
            .0
            .contains(&label)
    );
    let admitted = log_entries::local_swarm_roster_for_log(paths, &before_second_removal)
        .unwrap()
        .unwrap();
    assert!(
        admitted
            .member_by_label_or_alias(&label)
            .is_some_and(|member| !admitted.is_member_denied(member))
    );

    remove_member(paths, &label, false).unwrap();
    let current = swarm_roster_until(
        paths,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    .unwrap();
    assert!(current.is_label_denied(&label));
    assert!(
        current
            .member_by_label_or_alias(&label)
            .is_none_or(|member| current.is_member_denied(member))
    );
    let stale = log_entries::local_swarm_roster_for_log(paths, &before_second_removal);
    let stale_admits = stale.as_ref().is_ok_and(|roster| {
        roster.as_ref().is_some_and(|roster| {
            !roster.is_label_denied(&label)
                && roster
                    .member_by_label_or_alias(&label)
                    .is_some_and(|member| !roster.is_member_denied(member))
        })
    });
    macs.server.stop();
    assert!(
        !stale_admits,
        "an older restoration overrode the later removal"
    );
}

#[test]
fn a_signed_draft_extending_the_current_log_can_project_its_new_member() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let paths = &macs.m7;
    let current = log_of(paths);
    let heads = current.heads();
    let (bootstrap, actor, incarnation) = member_key(paths);
    let identity = read_identity(paths).unwrap().unwrap();
    let opened = current.open_audience_entries(&identity).unwrap().unwrap();
    let audiences = crate::membership_log::admission_audiences(&opened);
    let (_newcomer, request, _allowed) = asker(macs.root.path(), paths, "m40", "m40");
    let recipient = request.binding().recipient().to_bech32();
    let entrant = crate::membership_log::Entrant {
        request,
        node_key: [0x40; 32],
        name: "m40".to_owned(),
        label: "m40".to_owned(),
    };
    let authored_ms = current.records().map(Record::authored_ms).max().unwrap() + 1;
    let mut draft = current.clone();
    let minted = bootstrap
        .device()
        .with_seed(|seed| {
            draft.admit_with(
                &crate::membership_log::Signer {
                    actor: &actor,
                    seed,
                    incarnation,
                },
                entrant,
                crate::membership_log::Evidence::Phrase,
                &opened,
                &audiences,
                authored_ms,
            )
        })
        .unwrap();
    assert!(!minted.is_empty());
    assert_ne!(draft.heads(), heads);
    assert!(heads.iter().all(|head| draft.get(head).is_some()));
    let projected = log_entries::local_swarm_roster_for_log(paths, &draft)
        .unwrap()
        .unwrap();
    let member = projected.member_by_recipient(&recipient).unwrap();
    assert_eq!(member.label, "m40");
    assert!(!projected.is_member_denied(member));
    assert_eq!(log_of(paths).heads(), heads, "the draft was not committed");
    macs.server.stop();
}

/// F16 retires the admin-only revoke where a trusted log exists: a plain member, which holds no
/// admin key, removes by the log as the admin does, and `aterm secrets revoke` takes that path on
/// either.
///
/// NEGATIVE CONTROL: make `removes_by_log` also require the admin key (`admin_cached(paths)`):
/// the `"a plain member removes"` assert fails (proven 2026-10-29).
#[test]
fn a_vault_with_a_log_removes_by_any_member() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    assert!(admin_cached(m7).is_err(), "CONTROL: m7 holds no admin key");
    assert!(
        removes_by_log(m7),
        "a plain member removes where a log exists"
    );
    assert!(removes_by_log(m22));
    macs.server.stop();
}

/// A synthetic candidate for [`resolve`]: an actor from `tag`, a recipient from an identity of
/// its own, a label and a host name.
fn candidate(tag: u8, label: &str, name: &str) -> (MemberId, Recipient, String, String) {
    let identity = Identity::from_secret_bytes([tag; 32]);
    (
        [tag; 48],
        identity.recipient(),
        label.to_owned(),
        name.to_owned(),
    )
}

/// F16's review: a bare `m<N>` that names the Macs a clash suffixed (`m40-3fa2`, `m40-77b1`) and
/// is a third Mac's host name names all three, so the removal refuses it rather than take out
/// the Mac whose host name it happens to be. A word that names one Mac still resolves.
///
/// NEGATIVE CONTROL: in `resolve`, try the bare-label rule only when nothing else matched (the
/// old fallback): `m40` resolves to m41 alone and the `"names 3 Macs"` expect fails.
#[test]
fn a_bare_label_and_a_host_name_it_matches_name_every_mac() {
    let candidates = [
        candidate(1, "m40-3fa2", "studio"),
        candidate(2, "m40-77b1", "air"),
        candidate(3, "m41", "m40"),
    ];
    let error = resolve("m40", &candidates).expect_err("m40 names three Macs");
    assert!(error.to_string().contains("names 3 Macs"), "{error}");
    let (actor, _, label) = resolve("m41", &candidates).expect("m41 names one Mac");
    assert_eq!((actor, label.as_str()), ([3; 48], "m41"));
    let (actor, _, _) = resolve("m40-77b1", &candidates).expect("a suffixed label names one");
    assert_eq!(actor, [2; 48]);
}

/// One Mac of a hand-built log: an age identity of its own unless shared, a device seed and an
/// incarnation from `tag`.
struct Seat {
    identity: Identity,
    tag: u8,
}

impl Seat {
    fn request(&self) -> crate::commons::binding::Request {
        crate::commons::binding::Request::create(
            [2; 16],
            [3; 32],
            [self.tag; 16],
            [self.tag; 16],
            &self.identity,
            &[self.tag; 32],
        )
        .expect("request")
    }
    fn actor(&self) -> MemberId {
        self.request().binding().actor()
    }
    fn entrant(&self, label: &str) -> crate::membership_log::Entrant {
        crate::membership_log::Entrant {
            request: self.request(),
            node_key: [self.tag; 32],
            name: format!("mac-{}", self.tag),
            label: label.to_owned(),
        }
    }
    fn sign(&self, log: &mut Log, body: Body) -> Record {
        let record = log
            .draft(&self.actor(), 7, body)
            .sign(&[self.tag; 32], [self.tag; 16])
            .expect("sign");
        log.insert(record.clone()).expect("insert");
        record
    }
}

/// F16's review: m9 kept its machine key across a new device key, so its removed first seat and
/// the seat Allow again gave it share a join code, label and name. When the second seat removes
/// C, C's Undo row (which carries m9's join code) and `undo_removals(.., "m9")` name the one seat
/// with removals to undo, instead of refusing both as two Macs.
///
/// NEGATIVE CONTROL: in `undo_target`, return `resolve(who, &seated)` alone: the code names two
/// seats and the `"the second seat"` expect fails.
#[test]
fn an_undo_names_the_incarnation_with_removals_to_undo() {
    let seat = |tag: u8| Seat {
        identity: Identity::from_secret_bytes([tag; 32]),
        tag,
    };
    let (a, c, d) = (seat(1), seat(3), seat(4));
    let first = Seat {
        identity: Identity::from_secret_bytes([9; 32]),
        tag: 9,
    };
    let second = Seat {
        identity: Identity::from_secret_bytes([9; 32]),
        tag: 19,
    };
    let mut log = Log::new(
        Record::found(
            &[99; 32],
            vec![a.entrant("m1"), c.entrant("m3"), d.entrant("m4")],
            1,
        )
        .expect("genesis"),
    )
    .expect("log");
    let genesis = *log.network();
    let admitted = a.sign(
        &mut log,
        Body::Admit {
            entrant: first.entrant("m9"),
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    a.sign(
        &mut log,
        Body::Remove {
            member: first.actor(),
            cutoff: *admitted.id(),
        },
    );
    a.sign(
        &mut log,
        Body::Admit {
            entrant: second.entrant("m9"),
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    second.sign(
        &mut log,
        Body::Remove {
            member: c.actor(),
            cutoff: genesis,
        },
    );
    let code = join_code(&first.identity.recipient());
    let error = resolve(&code, &{
        let mut seated = member_candidates(&log);
        let (recipient, label, name) = seat_of(&log, &first.actor()).expect("first seat");
        seated.push((first.actor(), recipient, label, name));
        seated
    })
    .expect_err("CONTROL: the code names both seats");
    assert!(error.to_string().contains("names 2 Macs"), "{error}");
    let (remover, _, _) = undo_target(&log, &c.actor(), &code).expect("the second seat");
    assert_eq!(remover, second.actor());
    let hex = aterm_codec::hex::encode_lower(&first.actor());
    let (named, _, _) = undo_target(&log, &c.actor(), &hex).expect("an actor names its seat");
    assert_eq!(named, first.actor());
}

/// F16's review: a founder the genesis lists that has proved no device key (a 0.145 Mac after
/// F12b's Genesis v2), and a Mac only the vault's `[machines]` names, have no seat in the log, and
/// the envelope 0.145 Macs read still lists them (an admin's mirror wraps the log's keys to every
/// Mac the envelope lists). `aterm secrets revoke` sends each to the admin's revoke, which takes it
/// out of both, instead of the member path whose refusal pointed back at `revoke`; a member stays
/// on the member path, and the member path's refusal says where the admin's revoke runs.
/// The admin's revoke re-keys `[everyone]` and Commons over the current seats (one-mesh M1, M4):
/// m9 is out, the seat m22 holds the new keys, and the unproved m7, which no seat binds, drops
/// from the log's carry (it takes a seat through the desk to read the log again).
///
/// NEGATIVE CONTROL: make `revoke_by_log` return `removes_by_log(paths)` alone (the routing
/// before): the `"revoke sends an unproved founder to the admin"` assert fails. Restore the old
/// carry (keep a seatless recipient every newest head wraps) and the `"seatless m7 drops"`
/// assert fails.
#[test]
fn revoke_sends_a_mac_the_log_does_not_seat_to_the_admins_revoke() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let vault_file = root.path().join("alab.vault");
    let m22 = Paths::for_home(&root.path().join("m22"));
    let created = test_create_unbound(&m22, vault_file.to_str().expect("utf-8"), None, Some("m22"))
        .expect("vault");
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).expect("admin"));
    admin_commons_init(&m22).expect("commons init");
    let pinned = format!("{}#{}", vault_file.display(), created.signer);
    let (m7, m9) = (
        Paths::for_home(&root.path().join("m7")),
        Paths::for_home(&root.path().join("m9")),
    );
    let m7_rec = machine_identity(&m7).expect("m7").recipient();
    let m9_rec = machine_identity(&m9).expect("m9").recipient();
    let m22_rec = machine_identity(&m22).expect("m22").recipient();
    {
        let admin = admin_open(&m22).expect("admin");
        let mut doc = admin.doc.clone();
        doc.add_machine("m7", &m7_rec.to_bech32()).expect("m7");
        doc.add_machine("m9", &m9_rec.to_bech32()).expect("m9");
        doc.set_entry(&Audience::Everyone, "shared", b"v1")
            .expect("entry");
        admin_publish_approved(
            &m22,
            &admin,
            &doc,
            &[m7_rec.to_bech32(), m9_rec.to_bech32()],
        )
        .expect("publish");
    }
    join_host(&m7, &pinned, "m7");
    join_host(&m9, &pinned, "m9");
    seed_vault_genesis(&m22, &root.path().join("genesis.bin")).expect("genesis v2");
    let log = log_of(&m22);
    assert_eq!(
        log.membership().unproved().len(),
        2,
        "CONTROL: m7 and m9 unproved"
    );
    assert!(removes_by_log(&m22), "CONTROL: m22 removes by the log");
    assert!(
        !revoke_by_log(&m22, "m9"),
        "revoke sends an unproved founder to the admin"
    );
    assert!(
        !revoke_by_log(&m22, "p1"),
        "and a Mac the log does not seat"
    );
    assert!(
        revoke_by_log(&m22, "m22"),
        "a member stays on the member path"
    );

    let error = remove_member(&m22, "m9", false).expect_err("no member's removal names m9");
    assert!(
        error
            .to_string()
            .contains("on an admin's Mac, `aterm secrets revoke m9`"),
        "{error}"
    );
    let admin = admin_open(&m22).expect("admin");
    admin_revoke(&m22, &admin, "m9", false).expect("the admin's revoke removes m9");
    let log = log_of(&m22);
    for audience in [EpochAudience::Everyone, EpochAudience::Commons] {
        assert!(
            !log.wraps_maximal_audience_to(&audience, &m9_rec),
            "{audience:?}: the revoked m9 is out"
        );
        assert!(
            log.wraps_maximal_audience_to(&audience, &m22_rec),
            "{audience:?}: the seat m22 holds the re-keyed epoch"
        );
        assert!(
            !log.wraps_maximal_audience_to(&audience, &m7_rec),
            "{audience:?}: seatless m7 drops"
        );
    }
}

/// F16's review (reproduced from the reviewer's probe): a member's plain removal of m40, the
/// admin's steward pass that puts m40's label and node on the roster's deny rows, and a person's
/// Allow again that lets m40 back in. Every Mac's roster then lists m40 and denies it nothing, and
/// the steward's next pass lifts those rows from the entry 0.145 Macs read as well.
///
/// NEGATIVE CONTROL: make `restored_by_log` return two empty sets: the steward's rows stand, and
/// the `"m40 is denied nothing"` assert fails.
#[test]
fn a_mac_allowed_again_after_the_stewards_deny_rows_is_back_in_the_roster() {
    use crate::members::desk::Handing;
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        b"release_hosts = [\"m22\"]\n",
    )
    .expect("policy");
    admin_publish(m22, &admin, &doc).expect("publish");
    crate::client::admin_seed_swarm_roster(m22, &[]).expect("roster");
    sync(m7, None).expect("m7 syncs");

    let (paths40, request40, allowed40) = asker(macs.root.path(), m7, "m40", "m40");
    let (bundle, logged) = hand(m7, &request40, &allowed40, true);
    assert!(logged, "CONTROL: m7 hands the log path");
    write_from_log_bundle(&paths40, &bundle).expect("m40 writes");
    post_records_to_servers(m7).expect("m7 posts");
    let actor40 = request40.binding().actor();
    let label = log_of(m7)
        .membership()
        .get(&actor40)
        .expect("m40 is in")
        .label
        .clone();
    remove_member(m7, &label, false).expect("m7's plain removal of m40");

    let steward = || {
        sync(m22, None).expect("m22 syncs");
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
        let _ = steward_revoke_check(m22).expect("check");
        let _ = steward_finish_revokes(m22).expect("finish");
        envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        crate::swarm_roster::SwarmRoster::from_document(
            &admin_open(m22).expect("admin").published_doc,
        )
        .expect("roster")
        .expect("a roster")
    };
    let published = steward();
    assert!(
        published.is_label_denied(&label),
        "CONTROL: the steward denies {label}: {:?}",
        published.deny_labels
    );

    sync(m7, None).expect("m7 syncs");
    let handout = hand_over(
        m7,
        &Handing {
            request: &request40,
            node: [0x40; 32],
            name: "m40",
            allowed: &allowed40,
            reader: crate::members::bundle::Reader::LegacyLog,
            restore: true,
            node_proven: false,
            held: None,
        },
    )
    .expect("m7 restores m40");
    assert!(handout.logged);
    post_records_to_servers(m7).expect("m7 posts");
    assert!(
        log_of(m7).membership().get(&actor40).is_some(),
        "CONTROL: m40 is a member again"
    );
    sync(m22, None).expect("m22 syncs");
    for paths in [m22, m7] {
        let roster = crate::client::swarm_roster_until(
            paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("roster")
        .expect("a roster");
        assert!(
            !roster.is_label_denied(&label),
            "m40 is denied nothing: {:?}",
            roster.deny_labels
        );
        assert!(roster.members.iter().any(|m| m.label == label), "{label}");
    }
    let published = steward();
    assert!(
        !published.is_label_denied(&label),
        "the steward lifts {label}'s rows: {:?}",
        published.deny_labels
    );
    macs.server.stop();
}

#[test]
fn a_mac_allowed_again_under_a_new_device_key_clears_its_node_and_recipient_denials() {
    use crate::members::desk::Handing;
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        b"release_hosts = [\"m22\"]\n",
    )
    .expect("policy");
    admin_publish(m22, &admin, &doc).expect("publish");
    crate::client::admin_seed_swarm_roster(m22, &[]).expect("roster");
    sync(m7, None).expect("m7 syncs");

    let (paths40, request40, allowed40) = asker(macs.root.path(), m7, "m40", "m40");
    let handout1 = hand_over(
        m7,
        &Handing {
            request: &request40,
            node: [0x40; 32],
            name: "m40",
            allowed: &allowed40,
            reader: crate::members::bundle::Reader::LegacyLog,
            restore: false,
            node_proven: false,
            held: None,
        },
    )
    .expect("m7 admits m40 with node_key [0x40; 32]");
    assert!(handout1.logged);
    let bundle1 = crate::members::Bundle::decode(handout1.bundle.as_slice()).expect("bundle");
    write_from_log_bundle(&paths40, &bundle1).expect("m40 writes");
    post_records_to_servers(m7).expect("m7 posts");
    let actor40_v1 = request40.binding().actor();
    let rec40 = request40.binding().recipient();
    let label = log_of(m7)
        .membership()
        .get(&actor40_v1)
        .expect("m40 is in")
        .label
        .clone();
    remove_member(m7, &label, false).expect("m7 removes m40");
    assert!(
        log_of(m7).recipient_is_removed(&rec40),
        "CONTROL: m40's recipient is removed"
    );

    let steward = || {
        sync(m22, None).expect("m22 syncs");
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
        let _ = steward_revoke_check(m22).expect("check");
        let _ = steward_finish_revokes(m22).expect("finish");
        envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        crate::swarm_roster::SwarmRoster::from_document(
            &admin_open(m22).expect("admin").published_doc,
        )
        .expect("roster")
        .expect("a roster")
    };
    let node_hex = aterm_codec::hex::encode_lower(&[0x40; 32]);
    let published = steward();
    assert!(published.is_label_denied(&label));
    assert!(published.is_node_denied(&node_hex));

    // m40 keeps its machine identity (`identity.age`) in a fresh state dir (`m40-v2`) so it
    // generates a new `commons/device.age`.
    let paths40_v2 = Paths::for_home(&macs.root.path().join("m40-v2"));
    std::fs::create_dir_all(&paths40_v2.dir).expect("create m40-v2 dir");
    std::fs::copy(paths40.identity_file(), paths40_v2.identity_file()).expect("copy identity.age");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&paths40_v2.dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod dir");
        std::fs::set_permissions(
            paths40_v2.identity_file(),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("chmod identity");
    }
    let device_v2 = Device::open(&paths40_v2)
        .expect("device")
        .expect("identity");
    let trust = Bootstrap::required(m7).expect("m7").trust();
    let request40_v2 = device_v2
        .request(trust.network, trust.authority)
        .expect("request v2");
    assert_ne!(request40_v2.binding().actor(), actor40_v1);
    assert_eq!(request40_v2.binding().recipient(), rec40);
    let allowed40_v2 = Bootstrap::required(m7)
        .expect("m7")
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request40_v2.binding(), "m40", seed))
        .expect("allowed v2");

    sync(m7, None).expect("m7 syncs");
    let handout2 = hand_over(
        m7,
        &Handing {
            request: &request40_v2,
            node: [0x40; 32],
            name: "m40",
            allowed: &allowed40_v2,
            reader: crate::members::bundle::Reader::LegacyLog,
            restore: true,
            node_proven: false,
            held: None,
        },
    )
    .expect("m7 restores m40 under new device key");
    assert!(handout2.logged);
    post_records_to_servers(m7).expect("m7 posts");
    let bundle2 = crate::members::Bundle::decode(handout2.bundle.as_slice()).expect("bundle2");
    write_from_log_bundle(&paths40_v2, &bundle2).expect("write bundle2");
    crate::client::configure_log_member(
        &paths40_v2,
        &format!("{}#{}", macs.url, macs.signer),
        "m40",
    )
    .expect("configure v2");
    sync(&paths40_v2, None).expect("m40 syncs after restore");
    let cfg40 = config(&paths40_v2).expect("cfg").expect("some cfg");
    let verified = crate::client::entry::read_verified_entry(&paths40_v2, &cfg40, "shared")
        .expect("read_verified_entry")
        .expect("restored Mac reads verified entry before steward rewrites base roster");
    assert_eq!(verified.value().as_slice(), b"v1");

    let log7 = log_of(m7);
    let actor40_v2 = request40_v2.binding().actor();
    let membership7 = log7.membership();
    let member_v2 = membership7.get(&actor40_v2).expect("v2 is a member");
    assert_eq!(
        member_v2.label, label,
        "restored incarnation keeps bare label"
    );
    assert_eq!(
        membership7.node(&[0x40; 32]),
        crate::membership_log::NodeStanding::Member(actor40_v2),
        "restored Mac's node key is Member, not Removed"
    );
    assert!(!log7.recipient_is_removed(&rec40));
    assert!(
        removed_notes(m7)
            .expect("notes")
            .of(&rec40, request40_v2.binding().writer())
            .is_none(),
        "restored Mac is no longer in removed_notes"
    );
    let listed = member_list(m7).expect("list").expect("some");
    assert_eq!(
        listed.iter().filter(|m| m.label == label).count(),
        1,
        "restored Mac appears once in member_list: {listed:?}"
    );

    sync(m22, None).expect("m22 syncs");
    for paths in [m22, m7] {
        let roster = crate::client::swarm_roster_until(
            paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("roster")
        .expect("a roster");
        assert!(!roster.is_label_denied(&label), "{:?}", roster.deny_labels);
        assert!(!roster.is_node_denied(&node_hex), "{:?}", roster.deny_nodes);
        assert!(roster.members.iter().any(|m| m.label == label));
        let denied_nodes = crate::client::swarm_denied_nodes(paths)
            .expect("denied")
            .expect("some");
        assert!(!denied_nodes.contains(&node_hex), "{denied_nodes:?}");
    }
    let published = steward();
    assert!(!published.is_label_denied(&label));
    assert!(!published.is_node_denied(&node_hex));
    macs.server.stop();
}

/// F16's review: a removal by a member outside a host list the removed Mac was in cannot rotate
/// that list, so it names the list in what the Mac could read and in the `--stolen` line, and on
/// the next vault pass one of the list's own members mints its next epoch without the Mac. (Here
/// an admin is in the list, so its mirror also moves the list's entries to `[m22]`; a list with
/// no admin in it has only its members' pass.)
///
/// NEGATIVE CONTROL: make `Log::rekey_dues` skip host lists: m22's passes mint nothing for
/// `[m22 m40]`, whose newest epoch still wraps m40, and the `"m22 rotates [m22 m40]"` assert
/// fails.
#[test]
fn a_host_list_the_remover_cannot_open_is_named_and_rotated_by_its_members() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_m40, _actor40, rec40) = let_in(&macs, "m40", "m40");
    sync(m22, None).expect("m22 syncs m40's admission");
    let ring = Audience::Hosts(vec!["m22".to_owned(), "m40".to_owned()]);
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    assert!(
        doc.machines().iter().any(|m| m.name == "m40"),
        "CONTROL: the envelope names m40"
    );
    doc.set_entry(&ring, "ring-cred", b"ring-v1").expect("ring");
    admin_publish(m22, &admin, &doc).expect("publish the ring");
    let lists = EpochAudience::from(&ring);
    assert!(
        log_of(m22).wraps_maximal_audience_to(&lists, &rec40),
        "CONTROL: the ring's epoch wraps m40"
    );
    sync(m7, None).expect("m7 syncs");

    let removal = remove_member(m7, "m40", true).expect("m7 removes m40");
    assert!(
        !removal.rotated.contains(&ring.header()),
        "m7 cannot rotate it"
    );
    assert!(
        removal
            .could_read
            .iter()
            .any(|name| name.contains(&ring.header())),
        "{:?}",
        removal.could_read
    );
    let line = removal.needs_you.expect("a NEEDS YOU line");
    assert!(line.contains(&ring.header()), "{line}");

    // m22's pass runs the tender at the wall clock: m7, the remover, cannot carry the list, so
    // its one holder m22 ranks first with no grace and mints at once; a pass past every rank's
    // step finds nothing left.
    sync(m22, None).expect("m22 syncs the removal");
    let later = aterm_time::unix_millis().unwrap_or(0) + 40 * log_entries::GAP_STEP_MS;
    tend_epochs_at(m22, later).expect("tend");
    let log = log_of(m22);
    assert!(
        !log.wraps_maximal_audience_to(&lists, &rec40),
        "m22 rotates [m22 m40]"
    );
    let id22 = read_identity(m22).expect("identity").expect("m22");
    let opened = log
        .open_audience_entries(&id22)
        .expect("open")
        .expect("opens");
    let ring_values: Vec<Vec<u8>> = opened
        .by_audience
        .iter()
        .filter(|(audience, _)| matches!(audience, Audience::Hosts(_)))
        .filter_map(|(_, entries)| entries.get("ring-cred"))
        .map(|entry| entry.value.as_slice().to_vec())
        .collect();
    assert_eq!(
        ring_values,
        vec![b"ring-v1".to_vec()],
        "m22 still reads the ring (the admin's mirror moved it to [m22])"
    );
    macs.server.stop();
}

/// F16's review: a log `Remove` of another admin of the envelope 0.145 Macs read leaves it that
/// envelope's signing key (the admin's mirror drops it from `[admins]` with the key unchanged), so
/// `aterm secrets revoke` takes the admin path for it, which rotates the key or, as in this
/// Commons-bound vault, refuses as before F16; and a removal of it through the log says the key
/// stays.
///
/// NEGATIVE CONTROL: make `vault_admin_named` return `None`: `aterm secrets revoke m7` would sign
/// a log `Remove`, and the routing assert and the note assert fail.
#[test]
fn an_envelope_admin_is_revoked_with_the_admin_key_and_a_log_removal_says_the_key_stays() {
    let macs = two_macs_with(&[(Audience::Everyone, "shared", b"v1".as_slice())], true);
    let m22 = &macs.m22;
    assert!(revoke_by_log(m22, "m7"), "CONTROL: the log seats m7");
    assert_eq!(
        vault_admin_named(m22, "m7").as_deref(),
        Some("m7"),
        "revoke takes the admin path for m7"
    );
    assert_eq!(vault_admin_named(m22, "m22"), None, "never this Mac itself");
    let admin = admin_open(m22).expect("admin");
    let Err(refused) = admin_revoke(m22, &admin, "m7", false) else {
        panic!("the admin path keeps its guard for a Commons-bound admin");
    };
    assert!(
        refused.to_string().contains("cannot demote a vault admin"),
        "{refused}"
    );

    let removal = remove_member(m22, "m7", false).expect("m22 removes m7 through the log");
    assert!(
        removal
            .envelope_notes
            .iter()
            .any(|note| note.starts_with("m7 was an admin") && note.contains("signing key")),
        "{:?}",
        removal.envelope_notes
    );
    macs.server.stop();
}

/// B12: a person's removal of the seat B2's resolver named says, as the word's does, that the
/// envelope admin it removed keeps the envelope's signing key, finding that admin by the seat's
/// age recipient (a node id names no envelope machine).
///
/// NEGATIVE CONTROL: make `vault_admin_holding` return `None`: the note assert fails.
#[test]
fn a_seats_removal_of_an_envelope_admin_says_the_key_stays() {
    let macs = two_macs_with(&[(Audience::Everyone, "shared", b"v1".as_slice())], true);
    let (_, m7, _) = member_key(&macs.m7);
    let removal = remove_seat(&macs.m22, m7, false).expect("m22 removes m7's seat");
    assert!(
        removal
            .envelope_notes
            .iter()
            .any(|note| note.starts_with("m7 was an admin") && note.contains("signing key")),
        "{:?}",
        removal.envelope_notes
    );
    macs.server.stop();
}

#[test]
fn stolen_member_removal_denies_node_keys_and_expires_steward_lease() {
    let rec = Identity::from_secret_bytes([0x44; 32]).recipient();
    let roster_node_key = aterm_codec::hex::encode_lower(&[0x55; 32]);
    let log_node_key = aterm_codec::hex::encode_lower(&[0x66; 32]);
    let mut roster = crate::swarm_roster::SwarmRoster::default();
    roster.members.push(crate::swarm_roster::SwarmMember {
        label: "m40".to_owned(),
        aliases: Vec::new(),
        claimed_label: None,
        claimed_at_ms: None,
        vault_name: "m40".to_owned(),
        recipient: rec.to_bech32(),
        hint: None,
        hw_hint: None,
        admitted_ms: 0,
        node_id: None,
        node_key: Some(roster_node_key.clone()),
    });
    let lease = crate::swarm_roster::SwarmLease {
        holder: "m40".to_owned(),
        until: 9_999_999_999,
        epoch: 3,
    };
    let mut entries = BTreeMap::from([
        (
            crate::swarm_roster::ROSTER_ENTRY.to_owned(),
            Entry {
                name: crate::swarm_roster::ROSTER_ENTRY.to_owned(),
                value: SecretBytes::from(roster.to_toml()),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            },
        ),
        (
            crate::swarm_roster::LEASE_ENTRY.to_owned(),
            Entry {
                name: crate::swarm_roster::LEASE_ENTRY.to_owned(),
                value: SecretBytes::from(lease.to_toml()),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            },
        ),
    ]);
    super::retirement::cascade_member_removal(
        &mut entries,
        &rec,
        &BTreeSet::from(["m40".to_owned()]),
        &BTreeSet::from([log_node_key.clone()]),
        true,
    )
    .expect("cascade");
    let after_roster = crate::swarm_roster::SwarmRoster::from_toml(
        std::str::from_utf8(entries[crate::swarm_roster::ROSTER_ENTRY].value.as_slice()).unwrap(),
    )
    .expect("parse roster");
    assert!(after_roster.is_label_denied("m40"));
    assert!(after_roster.is_node_denied(&roster_node_key));
    assert!(after_roster.is_node_denied(&log_node_key));

    let after_lease = crate::swarm_roster::SwarmLease::from_toml(
        std::str::from_utf8(entries[crate::swarm_roster::LEASE_ENTRY].value.as_slice()).unwrap(),
    )
    .expect("parse lease");
    assert_eq!(after_lease.until, 0, "removed steward's lease must expire");
}

#[test]
fn removing_one_split_clashed_mac_does_not_deny_surviving_mac_or_mark_restored_mac_left_out() {
    let seat = |tag: u8| Seat {
        identity: Identity::from_secret_bytes([tag; 32]),
        tag,
    };
    let (a, b, clash1, clash2) = (seat(1), seat(2), seat(40), seat(41));
    let mut log_a = Log::new(
        Record::found(&[99; 32], vec![a.entrant("m1"), b.entrant("m2")], 1).expect("genesis"),
    )
    .expect("log");
    let mut log_b = log_a.clone();

    // Concurrent split admissions both claiming `m40`.
    let admit1 = a.sign(
        &mut log_a,
        Body::Admit {
            entrant: clash1.entrant("m40"),
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    let admit2 = b.sign(
        &mut log_b,
        Body::Admit {
            entrant: clash2.entrant("m40"),
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    let admit2_id = *admit2.id();
    let mut log = log_a;
    log.insert(admit2).expect("merge admit2");
    let mem_before = log.membership();
    let label1 = mem_before
        .get(&clash1.actor())
        .expect("clash1 in")
        .label
        .clone();
    let label2 = mem_before
        .get(&clash2.actor())
        .expect("clash2 in")
        .label
        .clone();
    assert!(
        label1.starts_with("m40-") && label2.starts_with("m40-"),
        "both split claims are suffixed: {label1}, {label2}"
    );

    // Remove clash2; clash1 remains active.
    a.sign(
        &mut log,
        Body::Remove {
            member: clash2.actor(),
            cutoff: admit2_id,
        },
    );
    let mem_after = log.membership();
    assert!(
        !mem_after.removed_labels().contains("m40"),
        "surviving split holder keeps bare m40 out of removed_labels: {:?}",
        mem_after.removed_labels()
    );
    let mut roster = crate::swarm_roster::SwarmRoster::default();
    super::roster::overlay_log_membership(&log, None, &mut roster);
    assert!(
        !roster.is_label_denied("m40") && !roster.is_label_denied(&label1),
        "surviving split holder must not be label-denied: {:?}",
        roster.deny_labels
    );
    let clash1_member = mem_after.get(&clash1.actor()).expect("clash1 still active");
    assert!(
        !log_entries::member_denied_by_roster(&roster, clash1_member),
        "surviving split holder must not be denied by roster"
    );

    // Also verify `audience_left_out_recipient` when a removed Mac is restored concurrently
    // with an epoch that causally follows its pre-removal wrap.
    let (ep1_draft, ep1_key) = log
        .draft_epoch(&a.actor(), 10, EpochAudience::Everyone, 1)
        .expect("ep1");
    let ep1 = ep1_draft.sign(&[a.tag; 32], [a.tag; 16]).expect("sign ep1");
    let ep1_id = *ep1.id();
    log.insert(ep1).expect("insert ep1");
    for rec in [
        a.identity.recipient(),
        b.identity.recipient(),
        clash1.identity.recipient(),
    ] {
        let w = log
            .draft_wrap(&a.actor(), 11, ep1_id, &ep1_key, rec)
            .expect("wrap")
            .sign(&[a.tag; 32], [a.tag; 16])
            .expect("sign wrap");
        log.insert(w).expect("insert wrap");
    }
    // Remove clash1 and rotate to ep2 without clash1.
    a.sign(
        &mut log,
        Body::Remove {
            member: clash1.actor(),
            cutoff: *admit1.id(),
        },
    );
    let (ep2_draft, ep2_key) = log
        .draft_epoch(&a.actor(), 12, EpochAudience::Everyone, 2)
        .expect("ep2");
    let ep2 = ep2_draft.sign(&[a.tag; 32], [a.tag; 16]).expect("sign ep2");
    let ep2_id = *ep2.id();
    log.insert(ep2).expect("insert ep2");
    for rec in [a.identity.recipient(), b.identity.recipient()] {
        let w = log
            .draft_wrap(&a.actor(), 13, ep2_id, &ep2_key, rec)
            .expect("wrap2")
            .sign(&[a.tag; 32], [a.tag; 16])
            .expect("sign wrap2");
        log.insert(w).expect("insert wrap2");
    }
    assert!(
        log.audience_left_out_recipient(&EpochAudience::Everyone, &clash1.identity.recipient()),
        "CONTROL: before restore, clash1 is left out of ep2"
    );

    // Now b restores clash1 under a second device key (same recipient) on top of ep2,
    // before wrapping ep2 to clash1.
    let clash1_v2 = Seat {
        identity: Identity::from_secret_bytes([40; 32]),
        tag: 140,
    };
    let admit_v2 = b.sign(
        &mut log,
        Body::Admit {
            entrant: clash1_v2.entrant("m42"),
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    assert!(
        !log.audience_left_out_recipient(&EpochAudience::Everyone, &clash1.identity.recipient()),
        "restored Mac whose Admit follows ep2 must not be marked left out by pre-removal wrap"
    );

    // If clash1_v2 is also removed, `removal_target` still reports `is removed already` even
    // though clash1's recipient now has two removed seats.
    a.sign(
        &mut log,
        Body::Remove {
            member: clash1_v2.actor(),
            cutoff: *admit_v2.id(),
        },
    );
    let err = removal_target(&log, &join_code(&clash1.identity.recipient()), &a.actor())
        .expect_err("both incarnations removed");
    assert!(
        err.to_string().contains("is removed already"),
        "expected removed already, got: {err}"
    );
}

#[test]
fn a_removed_mac_asking_again_with_fresh_secrets_key_and_same_node_key_is_restored() {
    use crate::members::desk::{Asking, Desk, Handing, Local};
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m7 = &macs.m7;
    let node_key = [0x44; 32];
    let (m40, request40, allowed40) = asker(macs.root.path(), m7, "m40", "m40");
    let handout = hand_over(
        m7,
        &Handing {
            request: &request40,
            node: node_key,
            name: "m40",
            allowed: &allowed40,
            reader: crate::members::bundle::Reader::LegacyLog,
            restore: false,
            node_proven: false,
            held: None,
        },
    )
    .expect("m7 admits m40 with node_key");
    assert!(handout.logged);
    let bundle40 = crate::members::Bundle::decode(handout.bundle.as_slice()).unwrap();
    write_from_log_bundle(&m40, &bundle40).unwrap();
    let actor40_old = request40.binding().actor();
    let label = log_of(m7)
        .membership()
        .get(&actor40_old)
        .unwrap()
        .label
        .clone();
    remove_member(m7, &label, false).expect("m7 removes m40");

    // m40 wipes its ~/.secrets (fresh machine + device keys) while keeping its fleet node_key.
    let (m40_fresh, request_fresh, allowed_fresh) = asker(macs.root.path(), m7, "m40-fresh", "m40");
    assert_ne!(
        request_fresh.binding().recipient(),
        request40.binding().recipient()
    );
    assert_ne!(
        request_fresh.binding().writer(),
        request40.binding().writer()
    );

    let mut desk = Desk::new(Local::of(m7).unwrap().expect("m7 is a member"));
    let now = Instant::now();
    let device_fresh = Device::open(&m40_fresh).unwrap().unwrap();
    let key = *desk.local().key();
    let signature = crate::members::joiner::ask_signature(
        &device_fresh,
        &key,
        &node_key,
        "m40",
        "Mac15,3",
        request_fresh.bytes(),
    )
    .unwrap();
    desk.hear(
        &Asking {
            wire: request_fresh.bytes(),
            node: node_key,
            name: "m40",
            model: "Mac15,3",
            signature: &signature,
        },
        "192.0.2.44".parse().unwrap(),
        now,
    )
    .expect("heard fresh request with old node_key");

    let rows = desk.rows(now);
    let row = rows.iter().find(|r| r.name == "m40").unwrap().clone();
    let note = row
        .removed
        .clone()
        .expect("m40 asking with fresh secrets key and same node_key shows as removed");
    assert_eq!(note.label, label);
    assert!(!note.quorum);

    let err = desk
        .allow(row.id, now)
        .expect_err("plain Allow must refuse removed node_key");
    assert!(err.to_string().contains("Allow again"), "{err}");
    desk.allow_again(row.id, now)
        .expect("Allow again gives a phrase for removed node_key");

    let restored_handout = hand_over(
        m7,
        &Handing {
            request: &request_fresh,
            node: node_key,
            name: "m40",
            allowed: &allowed_fresh,
            reader: crate::members::bundle::Reader::LegacyLog,
            restore: true,
            node_proven: false,
            held: None,
        },
    )
    .expect("m7 restores m40 under fresh secrets identity and same node_key");
    assert!(restored_handout.logged);

    let after_log = log_of(m7);
    let membership = after_log.membership();
    let actor_fresh = request_fresh.binding().actor();
    let back = membership.get(&actor_fresh).expect("fresh seat is in");
    assert_eq!(
        back.label, label,
        "restored under its old bare label without clash suffix"
    );
    assert_eq!(
        membership.node(&node_key),
        crate::membership_log::NodeStanding::Member(actor_fresh)
    );
    assert!(
        removed_notes(m7)
            .unwrap()
            .of_with_node(
                &request_fresh.binding().recipient(),
                request_fresh.binding().writer(),
                &node_key,
            )
            .is_none(),
        "restored node_key is no longer in removed_notes"
    );
    let listed = member_list(m7).unwrap().unwrap();
    assert_eq!(
        listed.iter().filter(|m| m.label == label).count(),
        1,
        "member_list shows restored Mac once as active and not in gone: {listed:?}"
    );
    assert!(
        listed
            .iter()
            .find(|m| m.label == label)
            .unwrap()
            .removed
            .is_none()
    );
    let (restored_labels, restored_nodes) = roster::restored_by_log(&after_log);
    assert!(restored_labels.contains(&label));
    assert!(restored_nodes.contains(&aterm_codec::hex::encode_lower(&node_key)));

    let snapshot = roster::denial_swarm_roster(&Reads::new(m7))
        .unwrap()
        .unwrap();
    assert!(!snapshot.is_label_denied(&label));
    assert!(
        snapshot
            .member_by_recipient(&request_fresh.binding().recipient().to_bech32())
            .is_some()
    );
    let lock = StateLock::observe(m7).unwrap().unwrap();
    let held = roster::swarm_roster_for_denial_held(m7, &lock)
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.to_toml(),
        held.to_toml(),
        "contention observations must retain the same restoration semantics"
    );

    macs.server.stop();
}

#[test]
fn a_secondary_admit_squatting_active_node_key_or_denied_label_does_not_hide_removal_or_lift_deny()
{
    let seat = |tag: u8| Seat {
        identity: Identity::from_secret_bytes([tag; 32]),
        tag,
    };
    let (a, b, y) = (seat(1), seat(2), seat(41));
    let mut log = Log::new(
        Record::found(&[99; 32], vec![a.entrant("m1"), b.entrant("m2")], 1).expect("genesis"),
    )
    .expect("log");

    // A admits Y as `m41` with node_key `[41; 32]`.
    let admit1 = a.sign(
        &mut log,
        Body::Admit {
            entrant: y.entrant("m41"),
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    let admit1_id = *admit1.id();

    // B signs a redundant secondary Admit for Y carrying A's active node_key `[1; 32]` and a
    // denied label `m99`, grinding `authored_ms` so its SHA-256 RecordId sorts below `admit1_id`.
    let mut forged_entrant = y.entrant("m99");
    forged_entrant.node_key = [a.tag; 32];
    let mut admit2 = None;
    for ts in 10..10_000 {
        let candidate = log
            .draft(
                &b.actor(),
                ts,
                Body::Admit {
                    entrant: forged_entrant.clone(),
                    evidence: crate::membership_log::Evidence::Manual,
                },
            )
            .sign(&[b.tag; 32], [b.tag; 16])
            .expect("sign candidate");
        if *candidate.id() < admit1_id {
            admit2 = Some(candidate);
            break;
        }
    }
    let admit2 = admit2.expect("found smaller RecordId");
    let admit2_id = *admit2.id();
    log.insert(admit2).expect("insert admit2");

    // A removes Y at `admit2_id`.
    a.sign(
        &mut log,
        Body::Remove {
            member: y.actor(),
            cutoff: admit2_id,
        },
    );

    // 1. A's node_key `[1; 32]` remains `Member(a.actor())`, and Y's node_key `[41; 32]` is `Removed`.
    let membership = log.membership();
    assert_eq!(
        membership.node(&[a.tag; 32]),
        crate::membership_log::NodeStanding::Member(a.actor())
    );
    assert_eq!(
        membership.node(&[y.tag; 32]),
        crate::membership_log::NodeStanding::Removed(y.actor())
    );

    // 2. `notes_of(&log)` still reports Y as removed under its earliest causal seat `m41`
    // (not hidden by A's active node_key `[1; 32]`, and not relabeled `m99`).
    let notes = notes_of(&log);
    let note = notes
        .of_with_node(
            &y.identity.recipient(),
            y.request().binding().writer(),
            &[y.tag; 32],
        )
        .expect("Y must still appear in RemovedNotes");
    assert_eq!(note.label, "m41");

    // 3. If Y is later restored under a new device key (`y_v2`), `restored_by_log` lifts `m41`
    // and `[41; 32]`, NOT `m99` from the redundant secondary Admit.
    let y_v2 = Seat {
        identity: Identity::from_secret_bytes([41; 32]),
        tag: 141,
    };
    let mut restored_entrant = y_v2.entrant("m41");
    restored_entrant.node_key = [y.tag; 32];
    a.sign(
        &mut log,
        Body::Admit {
            entrant: restored_entrant,
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    let (restored_labels, restored_nodes) = roster::restored_by_log(&log);
    assert!(restored_labels.contains("m41"));
    assert!(
        !restored_labels.contains("m99"),
        "secondary Admit's label must not be lifted by restore: {restored_labels:?}"
    );
    assert!(restored_nodes.contains(&aterm_codec::hex::encode_lower(&[y.tag; 32])));
}

#[test]
fn notes_of_and_kept_out_prefer_quorum_removal_and_newest_removed_seat_over_incarnation_sort() {
    let seat = |tag: u8| Seat {
        identity: Identity::from_secret_bytes([tag; 32]),
        tag,
    };
    let (a, b, c) = (seat(1), seat(2), seat(3));
    let mut log = Log::new(
        Record::found(
            &[99; 32],
            vec![a.entrant("m1"), b.entrant("m2"), c.entrant("m3")],
            1,
        )
        .expect("genesis"),
    )
    .expect("log");

    // 1. Quorum removal vs single-member removal across two incarnations sharing recipient/writer:
    // Seat 1 has incarnation `[0x01; 16]` (sorts FIRST in BTreeMap) and is removed by quorum (A + B).
    // Seat 2 has incarnation `[0xFF; 16]` (sorts SECOND in BTreeMap) and is removed by A alone.
    let target_id = Identity::from_secret_bytes([50; 32]);
    let target_seed = [50u8; 32];
    let req_quorum = crate::commons::binding::Request::create(
        [2; 16],
        [3; 32],
        [50; 16],
        [0x01; 16],
        &target_id,
        &target_seed,
    )
    .expect("req_quorum");
    let req_single = crate::commons::binding::Request::create(
        [2; 16],
        [3; 32],
        [50; 16],
        [0xFF; 16],
        &target_id,
        &target_seed,
    )
    .expect("req_single");
    assert!(
        req_quorum.binding().actor() < req_single.binding().actor(),
        "CONTROL: quorum seat sorts before single-member seat in BTreeMap"
    );

    let mut log_branch = log.clone();
    let admit_quorum = a.sign(
        &mut log,
        Body::Admit {
            entrant: crate::membership_log::Entrant {
                request: req_quorum.clone(),
                node_key: [0x50; 32],
                name: "m50".to_owned(),
                label: "m50".to_owned(),
            },
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    let admit_single = c.sign(
        &mut log_branch,
        Body::Admit {
            entrant: crate::membership_log::Entrant {
                request: req_single.clone(),
                node_key: [0x50; 32],
                name: "m50".to_owned(),
                label: "m50".to_owned(),
            },
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    let rem_single = c.sign(
        &mut log_branch,
        Body::Remove {
            member: req_single.binding().actor(),
            cutoff: *admit_single.id(),
        },
    );
    // Quorum of A and B removes `req_quorum`.
    a.sign(
        &mut log,
        Body::Remove {
            member: req_quorum.binding().actor(),
            cutoff: *admit_quorum.id(),
        },
    );
    b.sign(
        &mut log,
        Body::Remove {
            member: req_quorum.binding().actor(),
            cutoff: *admit_quorum.id(),
        },
    );
    log.insert(admit_single).expect("merge admit_single");
    log.insert(rem_single).expect("merge rem_single");

    let notes = notes_of(&log);
    let note = notes
        .of_with_node(
            &target_id.recipient(),
            req_quorum.binding().writer(),
            &[0x50; 32],
        )
        .expect("note present");
    assert!(
        note.quorum,
        "quorum removal must not be overwritten by higher-MemberId single removal: {note:?}"
    );
    // Even if the Mac asks again with a brand-new writer key (same recipient), `kept_out_with_node`
    // must still refuse `restore = true` because a quorum removed it.
    let req_new_writer = crate::commons::binding::Request::create(
        [2; 16],
        [3; 32],
        [51; 16],
        [0x02; 16],
        &target_id,
        &[51u8; 32],
    )
    .expect("req_new_writer");
    let why = super::admit::kept_out_with_node(&log, req_new_writer.binding(), [0; 32], true)
        .expect("quorum-removed recipient must stay kept out on restore");
    assert!(why.contains("quorum"), "{why}");

    // 2. Two plain removals in causal order where the first seat has incarnation `[0xFF; 16]`
    // (`m40`) and the restored second seat has incarnation `[0x01; 16]` (`m45`): `notes_of`
    // must report the newer seat's label `m45`, not `m40`.
    let plain_id = Identity::from_secret_bytes([60; 32]);
    let req_first = crate::commons::binding::Request::create(
        [2; 16],
        [3; 32],
        [60; 16],
        [0xFF; 16],
        &plain_id,
        &[60u8; 32],
    )
    .expect("req_first");
    let req_second = crate::commons::binding::Request::create(
        [2; 16],
        [3; 32],
        [60; 16],
        [0x01; 16],
        &plain_id,
        &[60u8; 32],
    )
    .expect("req_second");
    assert!(
        req_second.binding().actor() < req_first.binding().actor(),
        "CONTROL: newer seat sorts before older seat in BTreeMap"
    );
    let admit1 = a.sign(
        &mut log,
        Body::Admit {
            entrant: crate::membership_log::Entrant {
                request: req_first.clone(),
                node_key: [0x60; 32],
                name: "m40".to_owned(),
                label: "m40".to_owned(),
            },
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    a.sign(
        &mut log,
        Body::Remove {
            member: req_first.binding().actor(),
            cutoff: *admit1.id(),
        },
    );
    let admit2 = a.sign(
        &mut log,
        Body::Admit {
            entrant: crate::membership_log::Entrant {
                request: req_second.clone(),
                node_key: [0x60; 32],
                name: "m45".to_owned(),
                label: "m45".to_owned(),
            },
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    a.sign(
        &mut log,
        Body::Remove {
            member: req_second.binding().actor(),
            cutoff: *admit2.id(),
        },
    );
    let notes2 = notes_of(&log);
    let note2 = notes2
        .of_with_node(
            &plain_id.recipient(),
            req_second.binding().writer(),
            &[0x60; 32],
        )
        .expect("note2 present");
    assert_eq!(
        note2.label, "m45",
        "notes_of must return the causally newer seat's label, got: {note2:?}"
    );
}

#[test]
fn removed_labels_overlay_and_local_label_resolution_enforce_node_and_recipient_removal() {
    let seat = |tag: u8| Seat {
        identity: Identity::from_secret_bytes([tag; 32]),
        tag,
    };
    let (a, b, x, y) = (seat(1), seat(2), seat(40), seat(41));
    let mut log = Log::new(
        Record::found(&[99; 32], vec![a.entrant("m1"), b.entrant("m2")], 1).expect("genesis"),
    )
    .expect("log");

    // X (`m40`) and Y (`m41`) are two incarnations of the same device (sharing writer seed
    // `[40; 32]` and physical node_key `[0x40; 32]`); X is removed while Y is not explicitly
    // named in a Remove record.
    let mut ent_x = x.entrant("m40");
    ent_x.node_key = [0x40; 32];
    let admit_x = a.sign(
        &mut log,
        Body::Admit {
            entrant: ent_x,
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    let req_y = crate::commons::binding::Request::create(
        [2; 16],
        [3; 32],
        [41; 16],
        [0x41; 16],
        &y.identity,
        &[40u8; 32],
    )
    .expect("req_y");
    let ent_y = crate::membership_log::Entrant {
        request: req_y,
        node_key: [0x40; 32],
        name: "m41".to_owned(),
        label: "m41".to_owned(),
    };
    a.sign(
        &mut log,
        Body::Admit {
            entrant: ent_y,
            evidence: crate::membership_log::Evidence::Phrase,
        },
    );
    a.sign(
        &mut log,
        Body::Remove {
            member: x.actor(),
            cutoff: *admit_x.id(),
        },
    );

    // 1. Because `[0x40; 32]` is `NodeStanding::Removed`, Y's seat (`m41`) is also out and its
    // bare label `m41` must appear in `membership().removed_labels()`.
    let membership = log.membership();
    assert!(membership.removed_labels().contains("m40"));
    assert!(
        membership.removed_labels().contains("m41"),
        "seat out by shared removed node_key must contribute its label to removed_labels: {:?}",
        membership.removed_labels()
    );

    // 2. `overlay_log_membership` removes a `SwarmMember` row whose `node_key` is `[0x40; 32]`
    // even if that row has a rotated recipient (`z`) that never appeared in `log`.
    let z_rec = Identity::from_secret_bytes([42; 32])
        .recipient()
        .to_bech32();
    let node40_hex = aterm_codec::hex::encode_lower(&[0x40; 32]);
    let mut sw_roster = crate::swarm_roster::SwarmRoster {
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
        members: vec![
            crate::swarm_roster::SwarmMember {
                recipient: a.identity.recipient().to_bech32(),
                vault_name: "m1".to_owned(),
                label: "m1".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 0,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: Some(aterm_codec::hex::encode_lower(&[a.tag; 32])),
            },
            crate::swarm_roster::SwarmMember {
                recipient: z_rec,
                vault_name: "m42".to_owned(),
                label: "m42".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 0,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: Some(node40_hex.clone()),
            },
        ],
    };
    roster::overlay_log_membership(&log, None, &mut sw_roster);
    assert!(
        !sw_roster.members.iter().any(|m| m.label == "m42"),
        "row with log-removed node_key must be evicted from members: {:?}",
        sw_roster.members
    );
    assert!(sw_roster.is_label_denied("m42"));
    assert!(sw_roster.is_node_denied(&node40_hex));

    // 3. `resolve_local_swarm_label` returns `Ok(None)` when the local Mac is removed in the log
    // or denied in the roster, even if `source.toml` (`host = "m7"`) and `state.toml` (`name = "m7"`)
    // still name `"m7"`.
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    assert_eq!(
        crate::client::resolve_local_swarm_label(m7, &[]).expect("resolve before removal"),
        Some("m7".to_owned()),
        "CONTROL: m7 resolves its swarm label before removal"
    );
    remove_member(m22, "m7", false).expect("m22 removes m7");
    post_records_to_servers(m22).expect("m22 posts");
    sync(m7, None).expect("m7 syncs removal");
    assert_eq!(
        crate::client::resolve_local_swarm_label(m7, &["m7".to_owned()])
            .expect("resolve after removal"),
        None,
        "log-removed Mac must not resolve a swarm label from state.toml, source.toml, or hostnames"
    );
    macs.server.stop();
}

/// F21's split (the design's tests 4 to 6): m22 = A (the admin), m7 = Y, m40 = R, m42 and m44
/// other members, and m41 = X, whom m22 lets in on side 1 during the split; with `w`, also m43 =
/// W, whom m7 lets in on side 2 and who puts `w-entry` under m40's epoch.
pub(super) struct KeyedSplit {
    pub(super) macs: TwoMacs,
    pub(super) m40: Paths,
    pub(super) m41: Paths,
    pub(super) m43: Option<Paths>,
    pub(super) rec40: Recipient,
    pub(super) rec44: Recipient,
    /// The `[everyone]` epoch m40 minted on side 2 past its cutoff, under which m7 put
    /// `shared` = v2.
    pub(super) epoch: RecordId,
    /// m22's text before the split: `shared` = v1 and `other` = o1.
    pub(super) doc: Document,
}

/// Build [`KeyedSplit`] up to the heal. Side 1 (m22, m41): m22 removes m40 and m42 and lets in
/// m41. Side 2 (m40, m7): m40, not yet told, removes m42, which mints its epoch `E` wrapped to m22,
/// m7, m40 and m44; m7 pulls those records and puts `shared` = v2 under `E`, the one newest
/// `[everyone]` epoch it holds. A removal concurrent with its signer's own still holds (F16), so
/// m22 removes m42 too: after the heal no newest epoch is wrapped to a removed Mac, F16's
/// coverage rule has nothing to do, and only F21's reseal is due.
pub(super) fn keyed_split(w: bool) -> KeyedSplit {
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"v1".as_slice()),
        (Audience::Everyone, "other", b"o1".as_slice()),
    ]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, actor40, rec40) = let_in(&macs, "m40", "m40");
    let (_m42, _, rec42) = let_in(&macs, "m42", "m42");
    let (_m44, _, rec44) = let_in(&macs, "m44", "m44");
    sync_records(m22, None).expect("m22 pulls the admissions");
    sync_records(&m40, None).expect("m40 pulls the admissions");
    let doc = admin_open(m22).expect("m22 opens the vault").doc.clone();

    // Side 1.
    remove_member(m22, &join_code(&rec40), false).expect("m22 removes m40");
    remove_member(m22, &join_code(&rec42), false).expect("m22 removes m42");
    let (m41, _, _) = let_in_by(&macs, m22, "m41", "m41");

    // Side 2.
    remove_member(&m40, &join_code(&rec42), false).expect("m40, not yet told, removes m42");
    save_records(m7, &load_records(&m40).unwrap().encode()).expect("m7 pulls m40's records");
    let epoch = test_put_everyone(m7, "shared", b"v2").expect("m7 puts shared = v2");
    assert_eq!(
        log_of(m7).get(&epoch).map(|r| *r.actor()),
        Some(actor40),
        "CONTROL: m40 minted the epoch m7 wrote under"
    );
    let m43 = w.then(|| {
        let (m43, _, _) = let_in_by(&macs, m7, "m43", "m43");
        test_put_everyone(&m43, "w-entry", b"w1").expect("m43 puts w-entry");
        save_records(m7, &load_records(&m43).unwrap().encode()).expect("m7 pulls m43's put");
        m43
    });
    KeyedSplit {
        macs,
        m40,
        m41,
        m43,
        rec40,
        rec44,
        epoch,
        doc,
    }
}

impl KeyedSplit {
    /// Every Mac here, the removed one included.
    pub(super) fn all(&self) -> Vec<&Paths> {
        let mut all = vec![&self.macs.m22, &self.macs.m7, &self.m40, &self.m41];
        all.extend(self.m43.as_ref());
        all
    }

    /// The heal: every Mac takes every other's records.
    pub(super) fn heal(&self) {
        let wires: Vec<Vec<u8>> = self
            .all()
            .into_iter()
            .map(|paths| load_records(paths).unwrap().encode())
            .collect();
        for paths in self.all() {
            for wire in &wires {
                save_records(paths, wire).expect("the heal keeps every record");
            }
        }
    }

    /// What `paths` reads of the `[everyone]` entry `name` from its log.
    pub(super) fn read(paths: &Paths, name: &str) -> Option<Vec<u8>> {
        let id = read_identity(paths).unwrap().unwrap();
        log_of(paths)
            .open_audience_entries(&id)
            .unwrap()
            .unwrap()
            .by_audience
            .get(&Audience::Everyone)
            .and_then(|entries| entries.get(name))
            .map(|entry| entry.value.as_slice().to_vec())
    }

    /// Whether a reseal or the coverage rule is due on `paths` for `[everyone]` or Commons that it
    /// can act on ([`Log::rekey_dues`]): it carries a rotation or coverage there, or reseals.
    pub(super) fn due(paths: &Paths) -> bool {
        let (_, actor, _) = member_key(paths);
        log_of(paths).rekey_dues().iter().any(|due| {
            matches!(
                due.audience,
                EpochAudience::Everyone | EpochAudience::Commons
            ) && if due.rotation || due.coverage {
                due.carriers.contains(&actor)
            } else {
                due.resealers.contains(&actor)
            }
        })
    }

    /// Whether `paths` hides an epoch it cannot open.
    pub(super) fn hidden(paths: &Paths) -> bool {
        let id = read_identity(paths).unwrap().unwrap();
        log_of(paths)
            .open_audience_entries(&id)
            .unwrap()
            .unwrap()
            .hidden_epochs
    }
}

/// F21 rule 7 (the design's test 5): after the heal, m41, which holds no key of m40's epoch,
/// removes m44 (`rotate_after_removal`) and publishes its text with `other` dropped (a fresh
/// `[everyone]` epoch: `draft_document_diff`'s fresh-epoch branch). Neither re-puts what m41
/// reads of `shared` (v1) with m7's v2 in its past, so m22 and m7 still read v2 and m41 still
/// waits for a holder, which its status says (rule 7's waiting line). Then m7's vault pass
/// reseals, m41 reads v2 too and nothing waits, and m7 records what m40 could read while it was
/// out (rule 8): `shared`.
///
/// NEGATIVE CONTROL (each restored with cp from a backup plus touch, and confirmed with cmp):
/// - `rotate_after_removal` reseals every entry while hidden (`let whole = true;`): fails
///   "m22 reads shared" with left `Some([118, 49])`, right `Some([118, 50])`.
/// - the fresh-epoch branch re-puts every entry while hidden (`let whole = true;`): fails the
///   same assert, left `Some([118, 49])`, right `Some([118, 50])`.
/// - `reseal_audience` re-puts no winner (`if false && whole && …`): E is covered but carries
///   nothing, and the test fails "m41 reads shared after the reseal", left `Some([118, 49])`,
///   right `Some([118, 50])`.
/// - `reseal_waits` names nothing (`if true || keyed.covered …`): fails "m41's status says
///   writes wait for a holder".
/// - `tend_epochs_at` records nothing after a reseal (`if false && !uncovered.is_empty() …`):
///   fails "m7's reseal records what m40 could read".
#[test]
fn a_mac_that_cannot_open_a_keyed_epoch_never_reseals_over_it() {
    let split = keyed_split(false);
    split.heal();
    let (m22, m7, m41) = (&split.macs.m22, &split.macs.m7, &split.m41);
    for paths in [m22, m7] {
        assert_eq!(
            KeyedSplit::read(paths, "shared"),
            Some(b"v2".to_vec()),
            "CONTROL: a holder reads m7's write after the heal"
        );
    }
    assert!(
        log_of(m22)
            .keyed_epochs()
            .iter()
            .any(|k| k.epoch == split.epoch && !k.covered),
        "CONTROL: E is keyed and uncovered"
    );
    assert!(KeyedSplit::hidden(m41), "CONTROL: m41 holds no key of E");
    assert_eq!(KeyedSplit::read(m41, "shared"), Some(b"v1".to_vec()));

    remove_member(m41, &join_code(&split.rec44), false).expect("m41 removes m44");
    let mut doc = split.doc.clone();
    assert!(doc.unset_entry(&Audience::Everyone, "other").unwrap());
    let cfg = config(m41).unwrap().expect("m41's config");
    let drafted = log_entries::draft_document_diff(
        m41,
        &cfg,
        &cfg.signer.expect("m41 pins the vault's signer"),
        &[],
        &read_identity(m41).unwrap().unwrap(),
        &doc,
        log_entries::Base::default(),
    )
    .unwrap()
    .expect("m41 writes the log");
    save_records(m41, &encode_records(&drafted.records)).unwrap();
    split.heal();

    for (who, paths) in [("m22", m22), ("m7", m7)] {
        assert_eq!(
            KeyedSplit::read(paths, "shared"),
            Some(b"v2".to_vec()),
            "{who} reads shared"
        );
        assert_eq!(
            KeyedSplit::read(paths, "other"),
            None,
            "{who} reads m41's drop"
        );
    }
    assert!(
        KeyedSplit::hidden(m41),
        "m41 still waits for a holder's reseal"
    );
    assert!(
        !reseal_waits(m41).unwrap().is_empty(),
        "m41's status says writes wait for a holder"
    );
    assert!(reseal_waits(m7).unwrap().is_empty(), "m7 opens E");

    // m7's vault pass reseals, and m41 reads m7's write.
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    assert!(
        !tend_epochs_at(m7, t0).unwrap().is_empty(),
        "m7 reseals at first sight"
    );
    split.heal();
    for (who, paths) in [("m22", m22), ("m7", m7), ("m41", m41)] {
        assert_eq!(
            KeyedSplit::read(paths, "shared"),
            Some(b"v2".to_vec()),
            "{who} reads shared after the reseal"
        );
        assert_eq!(
            KeyedSplit::read(paths, "other"),
            None,
            "{who} keeps m41's drop"
        );
        assert!(
            !KeyedSplit::hidden(paths),
            "{who} hides nothing after the reseal"
        );
        assert!(
            reseal_waits(paths).unwrap().is_empty(),
            "nothing waits on {who}"
        );
    }
    // Rule 8: the resealer records what m40 could read while out.
    let lines = read_after_cutoff_lines(m7).unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("m40 ") && line.contains("[everyone] shared")),
        "m7's reseal records what m40 could read: {lines:?}"
    );

    split.macs.server.stop();
}

/// F21 rule 5 (the design's test 4): m7 also deletes `other` under m40's epoch E during the split.
/// After the heal, m7 and m22 read m7's split-time writes (`shared` = v2, `other` gone), a holder's
/// overlay writes v2 into its text while m41's, holding no key of E, leaves its text alone, and a
/// reseal is due on the holders but not on m41. The vault pass's tender on m7, ranked first as the
/// signer of E's dependents, attests E and mints one higher `[everyone]` epoch with m7's writes in
/// its past, wrapped to every member but m40, carrying v2 and issuing the `Delete` again; m22 waits
/// its rank's step and, once m7's records reach it, finds E covered and signs nothing. Then nothing
/// is due or hidden anywhere and m41 reads v2 and no `other`.
///
/// NEGATIVE CONTROLS (each restored with cp from a backup plus touch, and confirmed with cmp):
/// - `reseal_audience` signs no attestation (its loop over `attest` takes none): E stays uncovered,
///   so m22, past its step, reseals, and the test fails "m22, past its step, signs nothing".
/// - `reseal_audience` issues no `Delete` of a keyed `Delete` winner (its loop over
///   `keyed_deletes` takes none): fails "m41 reads no other", left `Some([111, 49])`, right
///   `None`.
#[test]
fn the_vault_pass_after_the_heal_reseals_an_epoch_kept_readable() {
    let split = keyed_split(false);
    let (m22, m7, m41) = (&split.macs.m22, &split.macs.m7, &split.m41);
    {
        let (b7, a7, i7) = member_key(m7);
        let log7 = log_of(m7);
        let id7 = read_identity(m7).unwrap().unwrap();
        let opened = log7.open_audience_entries(&id7).unwrap().unwrap();
        let key: [u8; 32] = *opened.unwrapped_keys[&split.epoch];
        let delete = b7
            .device()
            .with_seed(|seed| {
                log7.draft_delete(
                    &a7,
                    aterm_time::unix_millis().unwrap_or(0),
                    split.epoch,
                    &key,
                    "other",
                )
                .unwrap()
                .sign(seed, i7)
            })
            .unwrap();
        save_records(m7, &encode_records(&[delete])).unwrap();
    }
    split.heal();
    for paths in [m22, m7] {
        assert_eq!(KeyedSplit::read(paths, "shared"), Some(b"v2".to_vec()));
        assert_eq!(KeyedSplit::read(paths, "other"), None);
    }
    assert!(KeyedSplit::hidden(m41), "m41 holds no key of E");
    assert_eq!(KeyedSplit::read(m41, "shared"), Some(b"v1".to_vec()));
    assert_eq!(KeyedSplit::read(m41, "other"), Some(b"o1".to_vec()));
    let mut text = split.doc.clone();
    text.set_entry(&Audience::Everyone, "shared", b"local")
        .unwrap();
    let overlaid = |paths: &Paths| {
        let mut doc = text.clone();
        log_entries::overlay_log_onto_document(
            &log_of(paths),
            &read_identity(paths).unwrap().unwrap(),
            &mut doc,
            None,
        )
        .unwrap();
        doc.entries()
            .into_iter()
            .find(|e| e.audience == Audience::Everyone && e.name == "shared")
            .map(|e| e.value.as_slice().to_vec())
    };
    assert_eq!(
        overlaid(m7),
        Some(b"v2".to_vec()),
        "a holder's overlay writes v2"
    );
    assert_eq!(
        overlaid(m41),
        Some(b"local".to_vec()),
        "m41's overlay leaves its text alone"
    );
    assert!(
        KeyedSplit::due(m7) && KeyedSplit::due(m22),
        "a reseal is due on E's holders"
    );
    assert!(
        !KeyedSplit::due(m41),
        "m41 holds no key of E and is not due"
    );

    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let signed = tend_epochs_at(m7, t0).unwrap();
    assert!(
        tend_epochs_at(m22, t0).unwrap().is_empty(),
        "m22 waits its rank's step"
    );
    let reseal = signed
        .iter()
        .find(|r| {
            matches!(
                r.body(),
                Body::Epoch {
                    audience: EpochAudience::Everyone,
                    ..
                }
            )
        })
        .map(|r| *r.id())
        .expect("m7, ranked first, reseals [everyone] at first sight");
    assert!(
        !epochs_wrapped_to(&log_of(m7), &split.rec40).contains(&reseal),
        "no Wrap hands m40 the reseal's epoch"
    );
    split.heal();
    // m7's reseal covers E, so nothing is left due: m22 signs nothing at `later`, nor 40 steps on,
    // past any rank or grace it could hold.
    let later = t0 + 40 * log_entries::GAP_STEP_MS;
    for at in [later, later + 40 * log_entries::GAP_STEP_MS] {
        assert!(
            tend_epochs_at(m22, at).unwrap().is_empty(),
            "m22, past its step, signs nothing"
        );
    }
    assert!(
        log_of(m22).keyed_epochs().iter().all(|k| k.covered),
        "m22 finds E covered"
    );
    for (who, paths) in [("m22", m22), ("m7", m7), ("m41", m41)] {
        assert!(!KeyedSplit::due(paths), "nothing is due on {who}");
        assert!(!KeyedSplit::hidden(paths), "{who} hides nothing");
        assert_eq!(
            KeyedSplit::read(paths, "shared"),
            Some(b"v2".to_vec()),
            "{who} reads v2"
        );
    }
    assert_eq!(KeyedSplit::read(m41, "other"), None, "m41 reads no other");
    assert_eq!(
        overlaid(m41),
        Some(b"v2".to_vec()),
        "m41's overlay writes v2"
    );

    split.macs.server.stop();
}

/// The churn lab's heal shape (one-mesh §11 H1), on two sides: m22 (the admin) removes m40, whose
/// rotation `R` is the one newest `[everyone]` epoch and wraps no removed Mac, while m7, not yet
/// told, puts `shared` = v2 under the epoch before it, `E0`, which wraps m40. After the heal no
/// newest epoch wraps a removed Mac (no rotation is due, so no write waits for a key), but `E0`
/// wins `shared` and still wraps m40: a departure the log made, whose reseal is due. Returns the
/// two Macs, m22's text before the split and m40's recipient.
fn departure_heal() -> (TwoMacs, Document, Recipient) {
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"v1".as_slice()),
        (Audience::Everyone, "other", b"o1".as_slice()),
    ]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_m40, _, rec40) = let_in(&macs, "m40", "m40");
    sync_records(m22, None).expect("m22 pulls the admission");
    let doc = admin_open(m22).expect("m22 opens the vault").doc.clone();
    remove_member(m22, &join_code(&rec40), false).expect("m22 removes m40");
    test_put_everyone(m7, "shared", b"v2").expect("m7, not yet told, puts shared = v2");
    let wires = [
        load_records(m22).unwrap().encode(),
        load_records(m7).unwrap().encode(),
    ];
    for paths in [m22, m7] {
        for wire in &wires {
            save_records(paths, wire).expect("the heal keeps every record");
        }
    }
    let log = log_of(m22);
    let newest = log.newest_epochs(&EpochAudience::Everyone);
    let to_m40 = epochs_wrapped_to(&log, &rec40);
    assert!(
        !newest.is_empty() && newest.iter().all(|epoch| !to_m40.contains(epoch)),
        "CONTROL: no newest epoch wraps m40"
    );
    (macs, doc, rec40)
}

/// What m22's publish of `doc`, overlaid with its log so it changes no entry, signs into
/// `[everyone]`: the kinds of the `Epoch`s and `Put`s it drafts there.
fn unchanged_publish(m22: &Paths, doc: &Document) -> Vec<Kind> {
    let identity = read_identity(m22).unwrap().unwrap();
    let mut doc = doc.clone();
    log_entries::overlay_log_onto_document(&log_of(m22), &identity, &mut doc, None).unwrap();
    let cfg = config(m22).unwrap().expect("m22's config");
    let drafted = log_entries::draft_document_diff(
        m22,
        &cfg,
        &cfg.signer.expect("m22 pins the vault's signer"),
        &[],
        &identity,
        &doc,
        log_entries::Base::default(),
    )
    .unwrap()
    .expect("m22 writes the log");
    drafted
        .records
        .into_iter()
        .filter(|r| match r.body() {
            Body::Epoch { audience, .. } | Body::Put { audience, .. } => {
                *audience == EpochAudience::Everyone
            }
            _ => false,
        })
        .map(|r| r.kind())
        .collect()
}

/// H1 (one-mesh §11; the churn lab's bimodal heal): a departure the log made is the ranked
/// tender's to reseal. In [`departure_heal`]'s shape the admin m22's publish of a text that
/// changes no entry (what an envelope mirror of the heads a heal left diverged publishes) mints
/// nothing in `[everyone]`; the tender's reseal, on the vault pass's injected clock, then wraps
/// no removed Mac, and nothing is left due. In the churn lab that mirror's fresh epoch landed
/// over island A's prefix within the settle grace, unranked, before the gap rule ran.
///
/// NEGATIVE CONTROL: `log_entries::DEPARTURE_MINTS` (an unchanged edit mints for the log's
/// departure, as before) makes m22's publish mint a fresh `[everyone]` epoch and re-put entries.
#[test]
fn an_unchanged_publish_mints_nothing_for_a_departure_the_log_made() {
    let (macs, doc, _) = departure_heal();
    let (m22, m7) = (&macs.m22, &macs.m7);
    assert_eq!(
        unchanged_publish(m22, &doc),
        Vec::new(),
        "an unchanged publish signs nothing into [everyone]"
    );
    // Nothing else is due: m40 already held `E0` and the `Put` m7 sealed under it before it
    // heard of the removal, so a fresh epoch takes nothing from it. Each Mac's vault passes, a
    // step apart on an injected clock past every rank and the settle grace, sign nothing, and
    // both read the newest epoch and v2.
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let steps = 2 + log_entries::SETTLE_GRACE_MS / log_entries::GAP_STEP_MS;
    for k in 0..steps {
        for (who, paths) in [("m22", m22), ("m7", m7)] {
            assert_eq!(
                vault_pass_at(paths, t0 + k * log_entries::GAP_STEP_MS)
                    .unwrap()
                    .len(),
                0,
                "{who} signs nothing at step {k}"
            );
        }
    }
    for (who, paths) in [("m22", m22), ("m7", m7)] {
        let log = log_of(paths);
        let opened = log
            .open_audience_entries(&read_identity(paths).unwrap().unwrap())
            .unwrap()
            .unwrap();
        assert!(
            log.newest_epochs(&EpochAudience::Everyone)
                .iter()
                .all(|epoch| opened.unwrapped_keys.contains_key(epoch)),
            "{who} opens the newest epoch"
        );
        assert_eq!(
            KeyedSplit::read(paths, "shared"),
            Some(b"v2".to_vec()),
            "{who} reads v2"
        );
    }
    macs.server.stop();

    let (macs, doc, _) = departure_heal();
    log_entries::DEPARTURE_MINTS.with(|cell| cell.set(true));
    let minted = unchanged_publish(&macs.m22, &doc);
    log_entries::DEPARTURE_MINTS.with(|cell| cell.set(false));
    assert!(
        minted.contains(&Kind::Epoch) && minted.contains(&Kind::Put),
        "CONTROL: an unchanged publish mints a fresh epoch for the log's departure: {minted:?}"
    );
    macs.server.stop();
}

/// F21 rules 5 and 8 on the vault pass's tender after the heal: m7, which holds E and every
/// newest epoch, reseals at first sight (rank 0, as the signer of E's dependent), which covers E,
/// and records what m40 could read while out.
///
/// NEGATIVE CONTROL: `tend_epochs_at` never recording rule 8 (its `resealed` read as `false`):
/// fails "the tender records what m40 could read".
#[test]
fn the_ranked_tender_reseals_a_keyed_epoch_and_records_rule_8() {
    let split = keyed_split(false);
    split.heal();
    let m7 = &split.macs.m7;
    assert!(KeyedSplit::due(m7), "CONTROL: a reseal is due on m7");
    assert!(
        log_of(m7).keyed_epochs().iter().any(|k| !k.covered),
        "CONTROL: E is uncovered"
    );
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    assert!(
        !tend_epochs_at(m7, t0).unwrap().is_empty(),
        "m7's tender reseals at first sight"
    );
    assert!(log_of(m7).keyed_epochs().iter().all(|k| k.covered));
    assert!(
        !read_after_cutoff_lines(m7).unwrap().is_empty(),
        "the tender records what m40 could read"
    );
    split.macs.server.stop();
}

/// F21 rule 5's ranks: where only the reseal is due (no newest epoch is wrapped to a removed Mac
/// and none is concurrent), the Macs ranked are E's holders, the signer of its dependent first.
/// m41, which holds the newest `[everyone]` epoch but no key of E, takes no rank: it could not
/// reseal, and each rank ahead of the first holder delays the reseal by one step.
///
/// NEGATIVE CONTROL: `RekeyDue::turn` ranking the carriers for a reseal alone (every holder of a
/// newest epoch, whatever is due): fails "m41, which holds no key of E, takes no rank".
#[test]
fn a_mac_without_a_key_of_the_keyed_epoch_takes_no_reseal_rank() {
    let split = keyed_split(false);
    split.heal();
    let (m22, m7, m41) = (&split.macs.m22, &split.macs.m7, &split.m41);
    let log = log_of(m22);
    let membership = log.membership();
    let dues: Vec<RekeyDue> = log
        .rekey_dues()
        .into_iter()
        .filter(|due| {
            matches!(
                due.audience,
                EpochAudience::Everyone | EpochAudience::Commons
            )
        })
        .collect();
    assert!(!dues.is_empty(), "CONTROL: a reseal is due");
    let (_, a7, _) = member_key(m7);
    let (_, a22, _) = member_key(m22);
    let (_, a41, _) = member_key(m41);
    for due in &dues {
        assert!(
            due.reseal && !due.rotation && !due.coverage,
            "CONTROL: only the reseal is due"
        );
        assert_eq!(
            due.turn(&membership, &a7),
            Some(Turn {
                rank: 0,
                after_grace: false
            }),
            "CONTROL: m7 signed E's dependent"
        );
        assert!(
            due.turn(&membership, &a22).is_some(),
            "CONTROL: m22 holds E"
        );
        assert_eq!(
            due.turn(&membership, &a41),
            None,
            "m41, which holds no key of E, takes no rank"
        );
    }
    split.macs.server.stop();
}

/// F21 rule 4's attestation is signed only by a Mac that opens the whole audience, so only a
/// reseal that carries the values covers. m7 holds E and every newest epoch; were it left out of
/// `[everyone]` (as a host list's member outside `[everyone]` is, whom §3.3.7's host-list rule
/// still has rotate the list), its `reseal_audience` mints an epoch but carries no value, so it
/// attests nothing and E stays uncovered.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp):
/// `reseal_audience` attesting whether or not it opens the audience whole: fails "a Mac that does
/// not open the audience whole covers nothing".
#[test]
fn a_reseal_that_carries_no_value_covers_nothing() {
    let split = keyed_split(false);
    split.heal();
    let m7 = &split.macs.m7;
    let (b7, a7, i7) = member_key(m7);
    let mut log7 = log_of(m7);
    let id7 = read_identity(m7).unwrap().unwrap();
    let mut opened = log7.open_audience_entries(&id7).unwrap().unwrap();
    assert!(
        opened
            .fully_unwrapped_audiences
            .remove(&EpochAudience::Everyone),
        "CONTROL: m7 opens [everyone] whole"
    );
    let mut minted = Vec::new();
    b7.device()
        .with_seed(|seed| {
            let author = log_entries::AuthorCtx {
                actor: &a7,
                writer_seed: seed,
                incarnation: i7,
                authored_ms: aterm_time::unix_millis().unwrap_or(0),
            };
            log_entries::reseal_audience(
                &mut log7,
                &opened,
                &author,
                &EpochAudience::Everyone,
                &BTreeSet::from([id7.recipient()]),
                &mut minted,
            )
        })
        .unwrap();
    assert!(
        minted
            .iter()
            .any(|r| matches!(r.body(), Body::Epoch { .. })),
        "CONTROL: it mints an epoch"
    );
    assert!(
        log7.keyed_epochs().iter().all(|k| !k.covered),
        "a Mac that does not open the audience whole covers nothing"
    );
    split.macs.server.stop();
}

/// F21 rule 7 after the reseal (the hostile review's shape): m22 edits `shared` to v3 after the
/// heal, before m7's reseal reaches it, so the edit is concurrent with the reseal. On m22 the
/// reseal's v2 carries m7's write and v3 follows that write, so v3 wins; on m41, which never gets
/// E's key, the reseal's v2 is a write of its own, a tie-break away from v3. Once E is covered
/// m41 hides nothing, but it still lacks a keyed key of `[everyone]`, so its removal of m44
/// re-puts no entry it did not change, and v3 stands on m22.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp):
/// `OpenedAudienceEntries::reputs_whole` ignoring `keyless_keyed`: fails "m41's removal re-puts
/// no entry it did not change".
#[test]
fn a_mac_without_a_keyed_key_never_reputs_whole_after_the_reseal() {
    let split = keyed_split(false);
    split.heal();
    let (m22, m7, m41) = (&split.macs.m22, &split.macs.m7, &split.m41);
    test_put_everyone(m22, "shared", b"v3").expect("m22 edits shared after the heal");
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    assert!(
        !tend_epochs_at(m7, t0).unwrap().is_empty(),
        "CONTROL: m7 reseals without m22's edit"
    );
    split.heal();
    assert!(
        !KeyedSplit::hidden(m41),
        "CONTROL: m41 hides nothing once E is covered"
    );
    assert_eq!(
        KeyedSplit::read(m22, "shared"),
        Some(b"v3".to_vec()),
        "CONTROL: v3 on m22"
    );
    let (_, a41, _) = member_key(m41);
    let before: BTreeSet<RecordId> = log_of(m41).records().map(|r| *r.id()).collect();
    remove_member(m41, &join_code(&split.rec44), false).expect("m41 removes m44");
    let reput: Vec<RecordId> = log_of(m41)
        .records()
        .filter(|r| {
            !before.contains(r.id())
                && *r.actor() == a41
                && matches!(
                    r.body(),
                    Body::Put {
                        audience: EpochAudience::Everyone,
                        ..
                    }
                )
        })
        .map(|r| *r.id())
        .collect();
    assert!(
        reput.is_empty(),
        "m41's removal re-puts no entry it did not change"
    );
    split.heal();
    assert_eq!(
        KeyedSplit::read(m22, "shared"),
        Some(b"v3".to_vec()),
        "v3 stands on m22"
    );
    split.macs.server.stop();
}

/// F21 rule 7 for the Commons control descriptor: a Mac that does not open all of Commons keeps
/// its text's descriptor as it was, which can be older than the one every Mac reads, so it puts
/// the descriptor only when the person changed it. `old`, `new` and `edit` stand for three
/// descriptors.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp):
/// `commons_descriptor_due` comparing with the view alone wherever Commons is hidden (the reuse
/// branch's rule before the review): fails "a stale descriptor is not put while Commons is
/// hidden".
#[test]
fn a_mac_that_does_not_open_all_of_commons_puts_only_a_changed_descriptor() {
    let (old, new, edit): (&[u8], &[u8], &[u8]) = (b"old", b"new", b"edit");
    for fresh in [false, true] {
        assert!(
            !log_entries::commons_descriptor_due(false, fresh, Some(new), old, Some(Some(old))),
            "a stale descriptor is not put while Commons is hidden"
        );
        assert!(
            !log_entries::commons_descriptor_due(false, fresh, Some(new), old, None),
            "an imported text puts no descriptor while Commons is hidden"
        );
        assert!(
            log_entries::commons_descriptor_due(false, fresh, Some(new), edit, Some(Some(old))),
            "the person's change is put while Commons is hidden"
        );
    }
    assert!(log_entries::commons_descriptor_due(
        true,
        false,
        Some(new),
        old,
        Some(Some(old))
    ));
    assert!(!log_entries::commons_descriptor_due(
        true,
        false,
        Some(new),
        new,
        Some(Some(old))
    ));
    assert!(log_entries::commons_descriptor_due(
        true,
        true,
        Some(new),
        new,
        None
    ));
}

/// F21 rule 4 at the client (the design's test 6, the review's shape): m43, whom m7 let in on side
/// 2 and who put `w-entry` under m40's epoch E, lacks m22's side-1 epoch after the heal, so its
/// publish of an edit of `other` (against the text it started from: `draft_document_diff`'s
/// sealed-away path) mints an epoch holding only that change, with every dependent of E in its
/// past. That partial mint covers nothing: E stays uncovered, m41 stays hidden once the holders
/// close their wrap gaps, and m41's removal of m44 re-puts nothing over `shared`.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, and confirmed with cmp): rule 4's
/// full reseal reduced to any counting epoch (`full` returns `true`, so neither the head clause
/// nor the attestation clause holds it back): fails "E stays uncovered after m43's partial mint".
#[test]
fn a_partial_mint_never_retires_an_epoch_kept_readable() {
    let split = keyed_split(true);
    split.heal();
    let (m22, m7, m41) = (&split.macs.m22, &split.macs.m7, &split.m41);
    let m43 = split.m43.as_ref().expect("m43");
    assert!(
        KeyedSplit::hidden(m43),
        "CONTROL: m43 lacks m22's side-1 epoch"
    );
    assert_eq!(KeyedSplit::read(m22, "w-entry"), Some(b"w1".to_vec()));

    let mut doc = split.doc.clone();
    doc.set_entry(&Audience::Everyone, "other", b"o2").unwrap();
    let cfg = config(m43).unwrap().expect("m43's config");
    let drafted = log_entries::draft_document_diff(
        m43,
        &cfg,
        &cfg.signer.expect("m43 pins the vault's signer"),
        &[],
        &read_identity(m43).unwrap().unwrap(),
        &doc,
        log_entries::Base {
            doc: Some(&split.doc),
            resolve: None,
            readmit: None,
        },
    )
    .unwrap()
    .expect("m43 writes the log");
    assert!(
        drafted.records.iter().any(|r| matches!(
            r.body(),
            Body::Epoch {
                audience: EpochAudience::Everyone,
                ..
            }
        )),
        "CONTROL: m43's edit mints a sealed-away [everyone] epoch"
    );
    save_records(m43, &encode_records(&drafted.records)).unwrap();
    split.heal();
    let keyed = log_of(m22).keyed_epochs();
    assert!(
        keyed.iter().any(|k| k.epoch == split.epoch && !k.covered),
        "E stays uncovered after m43's partial mint: {keyed:?}"
    );

    let now = aterm_time::unix_millis().unwrap_or(0) + 40 * log_entries::GAP_STEP_MS;
    for paths in [m22, m7] {
        log_entries::close_wrap_gaps(paths, now).expect("a holder closes its wrap gaps");
    }
    split.heal();
    assert!(KeyedSplit::hidden(m41), "m41 stays hidden");
    assert_eq!(
        KeyedSplit::read(m22, "other"),
        Some(b"o2".to_vec()),
        "m43's edit stands"
    );

    remove_member(m41, &join_code(&split.rec44), false).expect("m41 removes m44");
    split.heal();
    for (who, paths) in [("m22", m22), ("m7", m7)] {
        assert_eq!(
            KeyedSplit::read(paths, "shared"),
            Some(b"v2".to_vec()),
            "{who} reads shared after m41's removal of m44"
        );
    }

    split.macs.server.stop();
}

/// The homes of [`KeyedSplit`] (`w` on) before the heal, kept as a template each
/// `KeyedEpochs` replay copies, with m40 = R's own write `T` put under its epoch on side 2.
struct KeyedEpochsFixture {
    split: KeyedSplit,
}

impl KeyedEpochsFixture {
    const HOMES: [&'static str; 5] = ["m22", "m7", "m40", "m41", "m43"];

    fn build() -> Self {
        let split = keyed_split(true);
        test_put_everyone(&split.m40, "T", b"t1").expect("m40 puts T under its epoch");
        Self { split }
    }
}

/// One replay of the `KeyedEpochs` model through the real client: m22 = A, m7 = Y, m40 = R,
/// m41 = X and m43 = W, each a copy of the fixture's home.
struct KeyedEpochsHarness<'a> {
    fixture: &'a KeyedEpochsFixture,
    m22: Paths,
    m7: Paths,
    m40: Paths,
    m41: Paths,
    m43: Paths,
    healed: bool,
    partial: bool,
    edited: bool,
}

impl<'a> KeyedEpochsHarness<'a> {
    fn replay(fixture: &'a KeyedEpochsFixture, run_dir: &Path, path: &[&str]) -> Self {
        let template = fixture.split.macs.root.path();
        for home in KeyedEpochsFixture::HOMES {
            copy_private_tree(&template.join(home), &run_dir.join(home));
        }
        let mut h = Self {
            fixture,
            m22: Paths::for_home(&run_dir.join("m22")),
            m7: Paths::for_home(&run_dir.join("m7")),
            m40: Paths::for_home(&run_dir.join("m40")),
            m41: Paths::for_home(&run_dir.join("m41")),
            m43: Paths::for_home(&run_dir.join("m43")),
            healed: false,
            partial: false,
            edited: false,
        };
        for action in path {
            h.apply(action);
        }
        h
    }

    /// A copy at `run_dir` of the Macs as `self` leaves them: a run that goes on from where this
    /// one stands, without this one seeing it.
    fn fork(&self, run_dir: &Path) -> Self {
        for (home, paths) in KeyedEpochsFixture::HOMES.into_iter().zip(self.all()) {
            copy_private_tree(&paths.home, &run_dir.join(home));
        }
        Self {
            fixture: self.fixture,
            m22: Paths::for_home(&run_dir.join("m22")),
            m7: Paths::for_home(&run_dir.join("m7")),
            m40: Paths::for_home(&run_dir.join("m40")),
            m41: Paths::for_home(&run_dir.join("m41")),
            m43: Paths::for_home(&run_dir.join("m43")),
            healed: self.healed,
            partial: self.partial,
            edited: self.edited,
        }
    }

    fn all(&self) -> [&Paths; 5] {
        [&self.m22, &self.m7, &self.m40, &self.m41, &self.m43]
    }

    /// Every Mac takes every other's records.
    fn exchange(&self) {
        let wires: Vec<Vec<u8>> = self
            .all()
            .into_iter()
            .map(|paths| load_records(paths).unwrap().encode())
            .collect();
        for paths in self.all() {
            for wire in &wires {
                save_records(paths, wire).expect("every Mac keeps every record");
            }
        }
    }

    fn apply(&mut self, action: &str) {
        let now = aterm_time::unix_millis().unwrap_or(0);
        // Past every rank's step: a pass at `now` notes first sight, one here acts at any rank.
        let later = now + 100 * log_entries::GAP_STEP_MS;
        match action {
            "Heal" => self.healed = true,
            "CloseGap" => {
                for at in [now, later] {
                    log_entries::close_wrap_gaps(&self.m7, at).expect("m7 closes its wrap gaps");
                }
            }
            "PartialMint" => {
                let base = &self.fixture.split.doc;
                // A new entry: `other`, whose winner A's epoch seals, stays there, so W still
                // lacks an epoch it reads until a holder closes the gap.
                let mut doc = base.clone();
                doc.set_entry(&Audience::Everyone, "w-edit", b"x1").unwrap();
                let cfg = config(&self.m43).unwrap().expect("m43's config");
                let drafted = log_entries::draft_document_diff(
                    &self.m43,
                    &cfg,
                    &cfg.signer.expect("m43 pins the vault's signer"),
                    &[],
                    &read_identity(&self.m43).unwrap().unwrap(),
                    &doc,
                    log_entries::Base {
                        doc: Some(base),
                        resolve: None,
                        readmit: None,
                    },
                )
                .unwrap()
                .expect("m43 writes the log");
                save_records(&self.m43, &encode_records(&drafted.records)).unwrap();
                self.partial = true;
            }
            "Reseal" => {
                for at in [now, later] {
                    tend_epochs_at(&self.m7, at).expect("m7's vault pass");
                }
            }
            "Edit" => {
                test_put_everyone(&self.m22, "shared", b"v3").expect("m22 puts shared = v3");
                self.edited = true;
            }
            "BlindReput" => {
                remove_member(&self.m41, &join_code(&self.fixture.split.rec44), false)
                    .expect("m41 removes m44");
            }
            other => panic!("unexpected KeyedEpochs action: {other}"),
        }
        self.exchange();
    }

    fn observe(&self) -> aterm_spec::interp::State {
        let value = |paths: &Paths| match KeyedSplit::read(paths, "shared").as_deref() {
            Some(b"v1") => 1,
            Some(b"v2") => 2,
            Some(b"v3") => 3,
            other => panic!("shared reads {other:?}"),
        };
        let win = value(&self.m7);
        if self.healed {
            assert_eq!(value(&self.m22), win, "the holders read one value");
        }
        let covered = log_of(&self.m7)
            .keyed_epochs()
            .iter()
            .any(|k| k.epoch == self.fixture.split.epoch && k.covered);
        let t = [&self.m22, &self.m7, &self.m41, &self.m43]
            .into_iter()
            .any(|paths| KeyedSplit::read(paths, "T").is_some());
        // W's gap: an `[everyone]` epoch A would hand a Mac it admits that W holds no key of.
        let gap = {
            let id22 = read_identity(&self.m22).unwrap().unwrap();
            let opened22 = log_of(&self.m22)
                .open_audience_entries(&id22)
                .unwrap()
                .unwrap();
            let id43 = read_identity(&self.m43).unwrap().unwrap();
            let opened43 = log_of(&self.m43)
                .open_audience_entries(&id43)
                .unwrap()
                .unwrap();
            Log::admission_epochs(&opened22, &[crate::membership_log::SeatAudience::Everyone])
                .iter()
                .any(|(_, epoch)| !opened43.unwrapped_keys.contains_key(epoch))
        };
        aterm_spec::interp::State::from([
            ("healed", i64::from(self.healed)),
            ("gap", i64::from(gap)),
            ("partial", i64::from(self.partial)),
            ("covered", i64::from(covered)),
            ("edited", i64::from(self.edited)),
            ("win", win),
            ("sx", value(&self.m41)),
            ("hx", i64::from(KeyedSplit::hidden(&self.m41))),
            ("t", i64::from(t)),
        ])
    }
}

/// The `KeyedEpochs` projection: `action` run through the real client on `h`'s Macs, then the
/// model's state read back from them.
fn keyed_epochs_transition(
    h: &mut KeyedEpochsHarness<'_>,
    action: &str,
) -> aterm_spec::interp::State {
    h.apply(action);
    h.observe()
}

/// The design's test 8: every reachable `KeyedEpochs` state, reached by its first path, is what
/// the real client gives after that path, and every enabled action from it gives a successor.
/// Heal is the exchange of every record; CloseGap is m7's `close_wrap_gaps`; PartialMint is m43's
/// publish of a new entry against the text it started from; Reseal is m7's
/// `tend_epochs_at`; Edit is m22's put of `shared` = v3; BlindReput is m41's removal of m44. Each
/// pass that ranks its Macs runs twice, at first sight and past every rank's step.
///
/// Negative control in the test: after PartialMint, the state a coverage rule without its head
/// clause gives (X stops hiding, `hx = 0`) is no successor and breaks `ReadersAgree`.
///
/// Each state's run goes on from a copy of the run that found it, so a path runs once however many
/// actions leave its states. THE GAP: an action that goes wrong only on some rerun of a path, with
/// its fresh random bytes or a later clock, had a sample per edge and has one.
///
/// NEGATIVE CONTROLS (run by hand, each alone):
/// - no reseal (`reseal_ready` answers `false`, as before rule 5): fails `state mismatch after
///   ["Heal", "Reseal"]`, the run leaving `"covered": 0, "gap": 1, "hx": 1, "sx": 1` where the model
///   has `"covered": 1, "gap": 0, "hx": 0, "sx": 2`.
/// - rule 4's full reseal reduced to any counting epoch (`full` returns `true`): the replaced
///   reading's oracle fails first, `the keyed epochs disagree with the replaced reading`.
#[test]
fn keyed_epochs_model_conforms_and_catches_mutants() {
    let model = aterm_models_swarm::keyed_epochs_model();
    aterm_spec::interp::prove_and_catch(&model);
    let fired = aterm_spec::interp::fired_actions(&model);
    assert_eq!(fired.len(), 6, "fired actions: {fired:?}");

    let mut paths: std::collections::BTreeMap<aterm_spec::interp::State, Vec<&'static str>> =
        std::collections::BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([model.init_state()]);
    paths.insert(model.init_state(), Vec::new());
    while let Some(state) = queue.pop_front() {
        for action in &model.actions {
            for next in model.successors(action.name, &state) {
                if !paths.contains_key(&next) {
                    let mut path = paths[&state].clone();
                    path.push(action.name);
                    paths.insert(next.clone(), path);
                    queue.push_back(next);
                }
            }
        }
    }
    assert_eq!(
        paths.len(),
        aterm_spec::interp::reachable_states(&model).len()
    );
    let found_by: std::collections::BTreeMap<&[&str], &aterm_spec::interp::State> = paths
        .iter()
        .map(|(state, path)| (path.as_slice(), state))
        .collect();
    assert_eq!(
        found_by.len(),
        paths.len(),
        "every state has a path of its own"
    );

    let fixture = KeyedEpochsFixture::build();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let start = model.init_state();
    let first = KeyedEpochsHarness::replay(&fixture, &root.path().join("s0"), &[]);
    assert_eq!(first.observe(), start, "state mismatch after []");
    let mut judged = BTreeSet::from([&start]);
    let mut run_id = 0usize;
    // Shortest path first, so a state's own run stands kept before any action leaves it: its path
    // is its finder's and one action, and each action runs on a copy of it.
    let mut by_length: Vec<_> = paths.iter().collect();
    by_length.sort_by_key(|(_, path)| path.len());
    let mut kept: std::collections::BTreeMap<&[&str], KeyedEpochsHarness<'_>> =
        std::collections::BTreeMap::from([(&[][..], first)]);
    for (state, path) in by_length {
        let parent = kept
            .remove(path.as_slice())
            .expect("the state's finder kept its run");
        for action in &model.actions {
            if !model.action_enabled(action.name, state) {
                continue;
            }
            run_id += 1;
            let mut h = parent.fork(&root.path().join(format!("t{run_id}")));
            let next_obs = keyed_epochs_transition(&mut h, action.name);
            let next_path = [path.as_slice(), &[action.name]].concat();
            match found_by.get_key_value(next_path.as_slice()) {
                Some((&found, &next)) => {
                    assert_eq!(&next_obs, next, "state mismatch after {next_path:?}");
                    judged.insert(next);
                    kept.insert(found, h);
                }
                None => assert!(
                    model.successors(action.name, state).contains(&next_obs),
                    "{} after {path:?} gave {next_obs:?}",
                    action.name
                ),
            }
        }
    }
    assert_eq!(
        judged.len(),
        paths.len(),
        "every state was replayed and judged"
    );

    let healed = KeyedEpochsHarness::replay(&fixture, &root.path().join("neg-h"), &["Heal"]);
    let before = healed.observe();
    let mut bad =
        KeyedEpochsHarness::replay(&fixture, &root.path().join("neg"), &["Heal", "PartialMint"])
            .observe();
    assert_eq!(
        bad["hx"], 1,
        "CONTROL: X still hides after the partial mint"
    );
    bad.insert("hx", 0);
    assert!(
        !model.successors("PartialMint", &before).contains(&bad),
        "X unhidden after a partial mint is no successor"
    );
    let broken: Vec<&str> = model
        .invariants
        .iter()
        .filter(|inv| !model.check_invariant(inv.name, &bad))
        .map(|inv| inv.name)
        .collect();
    assert!(
        broken.contains(&"ReadersAgree"),
        "expected ReadersAgree in {broken:?}"
    );

    fixture.split.macs.server.stop();
}

// One-mesh §8 and M4 (A3): the 8,500 stop holds only an automatic batch no writer waits on, a
// removal reserves the room of the re-keys it makes due, and a writer that holds a `Remove`
// keeps its `[everyone]` writes unsigned until the new key arrives.

/// Record budgets whose keyed one is `keyed`, the weighed one as in production.
fn keyed_budget(keyed: usize) -> crate::membership_log::RecordBudgets {
    crate::membership_log::RecordBudgets {
        weighed: crate::membership_log::MAX_WEIGHED_RECORDS,
        keyed,
        held: crate::membership_log::MAX_WEIGHED_RECORDS + keyed,
    }
}

/// The record budgets in which `keyed` held records fill the log as `of` would fill
/// [`crate::membership_log::MAX_KEYED_RECORDS`]: one of one-mesh §8's fills, scaled to a lab's
/// log.
fn scaled_budget(keyed: usize, of: usize) -> crate::membership_log::RecordBudgets {
    keyed_budget(keyed * crate::membership_log::MAX_KEYED_RECORDS / of)
}

/// Each Mac of `all` runs its vault pass ([`vault_pass_at`]) at each of `steps`
/// [`log_entries::GAP_STEP_MS`] steps from `t0`, what one signs carried to every other Mac of
/// `all` at once. Returns what the passes signed.
fn passes(all: &[Paths], t0: u64, steps: u64) -> Vec<Record> {
    let mut signed = Vec::new();
    for k in 0..steps {
        for i in 0..all.len() {
            let mine = vault_pass_at(&all[i], t0 + k * log_entries::GAP_STEP_MS).unwrap();
            if !mine.is_empty() {
                convergence::carry(all, i);
                signed.extend(mine);
            }
        }
    }
    signed
}

/// The stop's lab, before any pass: of six Macs, m7 lets m44 in, which every Mac but m22 hears
/// of; then m22 re-keys `[everyone]` to the six seats it knows ([`convergence::rekey`]), and m7,
/// concurrently, to all seven and S, a key no seat binds ([`convergence::rekey_to`]); every Mac,
/// m44 too, takes both batches. The log holds no `Remove`: the two heads differ (S), so
/// `[everyone]` is due a coalesce no writer waits on, and the heal owes m44 one `Wrap` of m22's
/// head (m44's `Admit` is not in its past), after which S still sets the heads apart. Returns
/// the lab, its seven Macs (m44 last), m44's recipient, the log's keyed records and the tender's
/// bound on what the coalesce signs ([`log_entries::rekey_bound`]), the same before the
/// heal and after.
fn stop_lab() -> (convergence::Six, Vec<Paths>, Recipient, usize, usize) {
    let lab = convergence::six();
    let (m44, _, rec44) = let_in_by(&lab.macs, &lab.macs.m7, "m44", "m44");
    let wire = load_records(&lab.all[1]).unwrap().encode();
    for paths in &lab.all[2..] {
        save_records(paths, &wire).unwrap();
    }
    convergence::rekey(&lab.all[0], &EpochAudience::Everyone, |_| true);
    let mut to: BTreeSet<Recipient> = active(&lab.all[1]);
    assert!(to.contains(&rec44), "CONTROL: m7 re-keys to m44");
    to.insert(Identity::generate().unwrap().recipient());
    convergence::rekey_to(&lab.all[1], &EpochAudience::Everyone, &to);
    let mut all = lab.all.clone();
    all.push(m44);
    convergence::carry(&all, 0);
    convergence::carry(&all, 1);
    let log = log_of(&lab.all[0]);
    assert!(
        log.removed_recipients().is_empty(),
        "CONTROL: the log holds no Remove"
    );
    assert!(
        log.rekey_dues().iter().any(|due| {
            due.audience == EpochAudience::Everyone && due.coverage && !due.rotation
        }),
        "CONTROL: [everyone] is due a coalesce no removal made: {:?}",
        log.rekey_dues()
    );
    let identity = read_identity(&lab.all[0]).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let (_, me, _) = member_key(&lab.all[0]);
    let flight = log_entries::Flight::of_batch(&log, &opened, &me);
    let bound = log_entries::rekey_bound(
        &log,
        &opened,
        &flight,
        &EpochAudience::Everyone,
        &log.current_recipients(&EpochAudience::Everyone),
        &log.removed_recipients(),
        None,
    )
    .keyed;
    let keyed = log.counts().keyed;
    (lab, all, rec44, keyed, bound)
}

/// One-mesh §8, "The 8,500 stop": the ranked tender's coalesce of two `[everyone]` heads no
/// `Remove` set apart ([`stop_lab`]) is an automatic batch no writer waits on. In the largest
/// keyed budget whose scaled stop ([`crate::membership_log::RecordBudgets::automatic_keyed_stop`])
/// the log's keyed records and the coalesce's planned ones pass, the seven Macs' passes over 12
/// steps sign the heal's one `Wrap` (of m22's head, to m44, at the same fill) and no epoch, and
/// `[everyone]` stays due. In the smallest budget whose stop takes them, after the heal's `Wrap`
/// too, the same passes sign one coalesce, within the planned bound, which wraps m44, and nothing
/// is due after. There the heal's `Wrap` is signed where the gap closer's turn comes before the
/// tender's, and is not needed where the coalesce came first: which comes first follows the
/// ranks the lab's random keys draw, so the test counts a heal's `Wrap` (of an epoch the passes
/// did not mint) as at most one.
///
/// NEGATIVE CONTROLS:
/// - the contrast, run in the test: the smallest budget that fits the coalesce lies one to a few
///   keyed records above the one that holds it, and there the coalesce is signed, while past the
///   stop the heal is signed alone, so the stop holds the re-key and not the heal;
/// - `within_automatic_stop` returning `ready` at once (no stop; restored with cp from a backup
///   plus touch, confirmed with cmp): the test fails "the coalesce waits past the stop: 36 keyed +
///   10 planned > 45", left `{Everyone: 1}`, right `{}`.
#[test]
fn past_the_automatic_stop_a_coalesce_no_removal_made_waits_and_a_heal_does_not() {
    let stop_of = |keyed: usize| keyed_budget(keyed).automatic_keyed_stop();
    // A heal's `Wrap`: to m44, of an epoch the passes did not mint (a coalesce wraps m44 its own).
    let heal_of = |signed: &[Record], rec44: &Recipient| {
        let minted: BTreeSet<RecordId> = signed
            .iter()
            .filter(|record| record.kind() == Kind::Epoch)
            .map(|record| *record.id())
            .collect();
        signed
            .iter()
            .filter(|record| {
                matches!(record.body(), Body::Wrap { epoch, recipient, .. }
                    if recipient == rec44 && !minted.contains(epoch))
            })
            .count()
    };

    let (lab, all, rec44, keyed, bound) = stop_lab();
    let past = (keyed..)
        .take_while(|&k| stop_of(k) < keyed + bound)
        .last()
        .expect("a budget whose stop the coalesce passes");
    assert!(
        past > keyed + 1,
        "CONTROL: the log takes the heal under the hard bound"
    );
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let signed = keyed_budget(past).within(|| passes(&all, t0, 12));
    assert_eq!(
        convergence::epochs_by_audience(&signed),
        BTreeMap::new(),
        "the coalesce waits past the stop: {} keyed + {bound} planned > {}",
        keyed,
        stop_of(past)
    );
    assert_eq!(
        (signed.len(), heal_of(&signed, &rec44)),
        (1, 1),
        "the heal's Wrap is signed at the same fill"
    );
    let held = log_of(&lab.all[0]);
    assert!(
        held.rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone && due.coverage),
        "the coalesce stays due"
    );
    lab.macs.server.stop();

    let (lab, all, rec44, keyed, bound) = stop_lab();
    let below = (keyed..)
        .find(|&k| stop_of(k) >= keyed + 1 + bound)
        .expect("a budget whose stop takes the coalesce");
    assert!(
        below > past && below - past <= 3,
        "CONTROL: the contrast lies just above the stop ({past} then {below})"
    );
    let signed = keyed_budget(below).within(|| passes(&all, t0, 12));
    assert_eq!(
        convergence::epochs_by_audience(&signed),
        BTreeMap::from([(EpochAudience::Everyone, 1)]),
        "CONTROL: below the stop the coalesce is signed"
    );
    let heal = heal_of(&signed, &rec44);
    assert!(heal <= 1, "at most the one heal: {heal}");
    assert!(
        signed.len() - heal <= bound,
        "the bound is never below what the coalesce signs: {} > {bound}",
        signed.len() - heal
    );
    let after = log_of(&lab.all[0]);
    assert!(after.rekey_dues().is_empty());
    assert!(
        after.wraps_maximal_audience_to(&EpochAudience::Everyone, &rec44),
        "the coalesce wraps m44"
    );
    lab.macs.server.stop();
}

/// The numbers of a refusal's text, in order.
fn numbers_of(text: &str) -> Vec<usize> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|word| !word.is_empty())
        .map(|word| word.parse().unwrap())
        .collect()
}

/// One-mesh §8: `aterm members remove` (`fleet remove`) refuses up front where the log's keyed
/// records, its own batch and the worst case of the re-keys it makes due (here a coalesce of
/// `[everyone]` and of Commons, the audiences m40 is in, which m7 rotates itself) would pass the
/// keyed budget: in a budget the log's records fill exactly, and in one a record short of the
/// three, it signs nothing and prints the fill (records held, the batch, the worst case, the
/// limit), and the error names no value. In the budget that takes all three it removes m40, and
/// signs no more keyed records than its batch's bound.
///
/// NEGATIVE CONTROLS:
/// - the contrast, run in the test: one keyed record more of budget than the second refusal had
///   and the removal is signed, so the refusal is the fill's; the budget the own batch alone fits
///   (`keyed + batch`) still refuses, so the worst case is reserved;
/// - `draft_removal` with no fill check (`if false && fill.passes()`; restored with cp from a
///   backup plus touch, confirmed with cmp): the first removal signs into the log's hard bound and
///   fails with the log's own "membership log holds its limit of keyed records (Epoch, Wrap, Put,
///   Delete, Node, Name)", no fill and no "the membership log is full", so the test's first
///   refusal assert fails.
#[test]
fn a_removal_whose_worst_case_passes_the_keyed_budget_refuses_and_prints_the_fill() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"never-printed".as_slice())]);
    let m7 = &macs.m7;
    let (_m40, actor40, rec40) = let_in(&macs, "m40", "m40");
    let before = log_of(m7);
    let keyed = before.counts().keyed;
    let held: BTreeSet<RecordId> = before.records().map(|r| *r.id()).collect();

    let refused = |limit: usize| {
        let error = keyed_budget(limit)
            .within(|| remove_member(m7, &join_code(&rec40), false))
            .expect_err("the removal refuses");
        let text = error.to_string();
        assert!(text.contains("the membership log is full"), "{text}");
        assert!(!text.contains("never-printed"), "no value: {text}");
        let after: BTreeSet<RecordId> = log_of(m7).records().map(|r| *r.id()).collect();
        assert_eq!(after, held, "nothing is signed");
        numbers_of(&text)
    };
    let fill = refused(keyed);
    let [held_keyed, batch, worst, limit] = fill.as_slice() else {
        panic!("the fill's four numbers: {fill:?}");
    };
    let (batch, worst) = (*batch, *worst);
    assert_eq!((*held_keyed, *limit), (keyed, keyed));
    assert!(batch > 0 && worst > 0, "{fill:?}");
    assert_eq!(
        refused(keyed + batch + worst - 1),
        vec![keyed, batch, worst, keyed + batch + worst - 1]
    );
    assert_eq!(
        refused(keyed + batch),
        vec![keyed, batch, worst, keyed + batch],
        "CONTROL: the own batch fits, the worst case does not"
    );

    keyed_budget(keyed + batch + worst)
        .within(|| remove_member(m7, &join_code(&rec40), false))
        .expect("CONTROL: with room for the worst case m7 removes m40");
    let after = log_of(m7);
    assert!(after.membership().removal(&actor40).is_some());
    assert!(
        after.counts().keyed - keyed <= batch,
        "the batch's bound is never below what it signs: {} > {batch}",
        after.counts().keyed - keyed
    );
    macs.server.stop();
}

/// What [`writer_over_removal`] saw: the lab, the records the publish signed, what each of the
/// three passes signed, m7's batch, and whether m22's waiting file was there after the publish
/// and is after the passes.
struct WriterOverRemoval {
    lab: convergence::Six,
    published: Vec<Record>,
    pass: [Vec<Record>; 3],
    chain: Vec<Record>,
    waited: bool,
    waits_after: bool,
}

/// The shape of a writer that holds a `Remove` whose rotation it lacks: of six Macs (m22 and m7
/// admins), m7 removes m42 and m22 holds only its `Remove`, a prefix of m7's batch, when it
/// publishes `shared` = v2, `dont_wait` setting `log_entries::WRITERS_DONT_WAIT` for the publish.
/// Then m22's pass runs at `t0` (the rotation is still due: m22 waits the remover's grace); it
/// takes m7's whole batch, its pass runs a step later and once more a step after that, and every
/// other Mac takes its records.
fn writer_over_removal(dont_wait: bool) -> WriterOverRemoval {
    let lab = convergence::six_with(true);
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

    let before = convergence::ids(m22);
    log_entries::WRITERS_DONT_WAIT.with(|cell| cell.set(dont_wait));
    let admin = admin_open(m22).expect("m22 is an admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"v2")
        .expect("edit");
    let published = admin_publish(m22, &admin, &doc);
    log_entries::WRITERS_DONT_WAIT.with(|cell| cell.set(false));
    published.expect("the person's publish does not fail");
    let published = convergence::signed_since(m22, &before);
    let waited = log_entries::waiting_file(m22).exists();

    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let first = vault_pass_at(m22, t0).unwrap();
    save_records(m22, &encode_records(&chain)).unwrap();
    let second = vault_pass_at(m22, t0 + log_entries::GAP_STEP_MS).unwrap();
    let third = vault_pass_at(m22, t0 + 2 * log_entries::GAP_STEP_MS).unwrap();
    convergence::carry(&lab.all, 0);
    let waits_after = log_entries::waiting_file(m22).exists();
    WriterOverRemoval {
        lab,
        published,
        pass: [first, second, third],
        chain,
        waited,
        waits_after,
    }
}

/// One-mesh M4, "Writers wait for the new key" ([`writer_over_removal`]): m22, which holds m7's
/// `Remove` of m42 and not its rotation, publishes `shared` = v2. The publish succeeds and signs
/// nothing: the newest `[everyone]` epoch m22 holds still wraps m42, and the write waits in
/// m22's `everyone.waiting`. Its pass while the rotation is due signs nothing either. Once m7's
/// batch arrives, m22's next pass signs the one `Put`, under m7's new epoch, which no `Wrap`
/// hands m42; the pass after signs nothing, nothing waits, and the other Macs read v2.
///
/// NEGATIVE CONTROLS:
/// - `log_entries::WRITERS_DONT_WAIT` (the writer writes at once, as before), run in the test as
///   a CONTROL, and once with it set for the first run too: m22's publish signs a fresh
///   `[everyone]` epoch with its `Put` and a fresh Commons epoch with the descriptor while the
///   newest epochs it holds still wrap m42, and the test fails "the publish signs nothing while
///   the newest epoch wraps m42: [Epoch, Wrap, Wrap, Wrap, Wrap, Wrap, Put, Epoch, Wrap, Wrap,
///   Wrap, Wrap, Wrap, Put]";
/// - the writer's Commons block minting for the departure alone again (`true || !commons_waits`;
///   restored with cp from a backup plus touch, confirmed with cmp): the test fails "the publish
///   signs nothing while the newest epoch wraps m42: [Epoch, Wrap, Wrap, Wrap, Wrap, Wrap, Put]",
///   a Commons epoch beside the rotation due.
#[test]
fn a_writer_that_holds_a_remove_keeps_its_everyone_put_unsigned_until_the_new_key() {
    let run = writer_over_removal(false);
    let r = &run.lab.recipients;
    assert!(
        run.published.is_empty(),
        "the publish signs nothing while the newest epoch wraps m42: {:?}",
        run.published.iter().map(Record::kind).collect::<Vec<_>>()
    );
    assert!(run.waited, "the write waits in everyone.waiting");
    assert!(
        run.pass[0].is_empty(),
        "nothing is signed while the rotation is due"
    );
    let puts: Vec<RecordId> = run.pass[1]
        .iter()
        .filter_map(|record| match record.body() {
            Body::Put { epoch, .. } => Some(*epoch),
            _ => None,
        })
        .collect();
    assert_eq!(
        (run.pass[1].len(), puts.len()),
        (1, 1),
        "the pass after the rotation signs the one Put"
    );
    let rotation = run
        .chain
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
        .expect("m7's batch rotates [everyone]");
    assert_eq!(puts[0], rotation, "the Put goes under m7's new epoch");
    // The key boundary: the epoch's key is m7's, its minter's, and each `Wrap` of it is sealed to
    // the recipient it names. (A reader's view says nothing here: the log reads nothing for a
    // recipient it removed, whatever the keys.)
    let log = log_of(&run.lab.all[0]);
    assert_eq!(
        log.get(&rotation).map(|record| *record.actor()),
        Some(run.lab.actors[1]),
        "m7 minted it"
    );
    assert!(
        !epochs_wrapped_to(&log, &r[4]).contains(&rotation),
        "no Wrap hands m42 the epoch the write is sealed under"
    );
    assert!(run.pass[2].is_empty(), "signed exactly once");
    assert!(!run.waits_after, "nothing waits after");
    for i in [1, 2, 3] {
        assert_eq!(
            convergence::shared(&run.lab.all[i]).as_deref(),
            Some(b"v2".as_slice()),
            "every member reads the edit"
        );
    }
    run.lab.macs.server.stop();

    let control = writer_over_removal(true);
    let kinds: Vec<Kind> = control.published.iter().map(Record::kind).collect();
    assert!(
        kinds.contains(&Kind::Put) && kinds.contains(&Kind::Epoch),
        "CONTROL: the writer that does not wait signs at once: {kinds:?}"
    );
    assert!(!control.waited, "CONTROL: nothing waits");
    control.lab.macs.server.stop();
}

/// The epoch the op that wins `name` in `[everyone]` is sealed under, as `paths` opens its log.
fn winning_epoch(paths: &Paths, name: &str) -> Option<RecordId> {
    let identity = read_identity(paths).unwrap().unwrap();
    let log = log_of(paths);
    let opened = log.open_audience_entries(&identity).unwrap()?;
    let winner = opened
        .winning_ops
        .get(&(Audience::Everyone, name.to_owned()))?;
    match log.get(winner)?.body() {
        Body::Put { epoch, .. } | Body::Delete { epoch, .. } => Some(*epoch),
        _ => None,
    }
}

/// One-mesh M4 beside F15: a writer that holds m7's `Remove` of m42 and not its rotation, and
/// deletes `shared` from `[everyone]`, which also holds `kept`, writes at once rather than wait:
/// a fresh `[everyone]` epoch no `Wrap` hands m42, the `Delete` and `kept` resealed under it, and
/// nothing waits or is pending. m44, which m22 lets in next, is handed the fresh epoch and no
/// `Wrap` of the epoch `shared` = v1 is sealed under, so it cannot open the deleted value; it
/// reads `kept` from the reseal (a carry, which the register names by the op it carries).
///
/// NEGATIVE CONTROL: an edit that leaves `[everyone]` waiting as any other while the rotation is
/// due (`let waits = rotation_waits;` in `draft_document_diff_with`; restored with cp from a
/// backup plus touch, confirmed with cmp): the publish signs nothing, the admission hands m44
/// the old epoch, and the test fails "m44 is handed no Wrap of the epoch shared = v1 is sealed
/// under".
#[test]
fn a_delete_from_everyone_while_a_rotation_is_due_writes_at_once_and_reseals_what_stays() {
    let lab = convergence::six_with(true);
    let (m22, m7) = (&lab.all[0], &lab.all[1]);
    let r = &lab.recipients;
    let admin = admin_open(m22).expect("m22 is an admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "kept", b"k1")
        .expect("edit");
    admin_publish(m22, &admin, &doc).expect("m22 writes kept");
    convergence::carry(&lab.all, 0);
    let old = winning_epoch(m22, "shared").expect("shared = v1 is sealed under an epoch");
    assert_eq!(
        winning_epoch(m22, "kept"),
        Some(old),
        "CONTROL: kept is sealed under the epoch shared = v1 is"
    );

    let base = convergence::ids(m7);
    test_remove_unposted(m7, &join_code(&r[4])).expect("m7 removes m42");
    let chain = convergence::signed_since(m7, &base);
    save_records(m22, &encode_records(&chain[..1])).unwrap();
    assert!(
        log_of(m22)
            .rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone && due.rotation),
        "CONTROL: m22 holds the Remove, not its rotation"
    );

    let before = convergence::ids(m22);
    let admin = admin_open(m22).expect("m22 is an admin");
    let mut doc = admin.doc.clone();
    assert!(
        doc.unset_entry(&Audience::Everyone, "shared")
            .expect("unset"),
        "CONTROL: shared is in [everyone]"
    );
    admin_publish(m22, &admin, &doc).expect("the person's publish does not fail");
    let published = convergence::signed_since(m22, &before);
    let waits = log_entries::waiting_file(m22).exists() || log_entries::waiting_pending(m22);
    let (m44, _, r44) = let_in_by(&lab.macs, m22, "m44", "m44");
    let handed = epochs_wrapped_to(&log_of(m22), &r44);
    assert!(
        !handed.contains(&old),
        "m44 is handed no Wrap of the epoch shared = v1 is sealed under"
    );

    let fresh = published
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
        .unwrap_or_else(|| {
            panic!(
                "the delete is written at once, under a fresh [everyone] epoch: {:?}",
                published.iter().map(Record::kind).collect::<Vec<_>>()
            )
        });
    assert!(handed.contains(&fresh), "m44 is handed the fresh epoch");
    assert!(
        !epochs_wrapped_to(&log_of(m22), &r[4]).contains(&fresh),
        "no Wrap hands m42 the fresh epoch"
    );
    assert!(
        published.iter().any(|record| matches!(
            record.body(),
            Body::Delete { epoch, .. } if *epoch == fresh
        )),
        "the Delete goes under the fresh epoch"
    );
    assert!(
        published.iter().any(|record| matches!(
            record.body(),
            Body::Put { epoch, .. } if *epoch == fresh
        )),
        "kept is resealed under it"
    );
    assert_eq!(convergence::shared(m22), None, "shared is deleted");
    let identity44 = read_identity(&m44).unwrap().unwrap();
    let read44 = log_of(&m44)
        .open_audience_entries(&identity44)
        .unwrap()
        .expect("m44 reads the log");
    let everyone44 = &read44.by_audience[&Audience::Everyone];
    assert_eq!(
        everyone44.get("kept").map(|entry| entry.value.as_slice()),
        Some(b"k1".as_slice()),
        "m44 reads kept from its reseal"
    );
    assert!(!everyone44.contains_key("shared"), "and not shared");
    assert!(!waits, "nothing waits or is pending");
    lab.macs.server.stop();
}

/// How [`cut_publish`] ends m22's publish early.
#[derive(Clone, Copy)]
enum Cut {
    /// Before any server took the version (`CUT_BEFORE_STORE`).
    BeforeStore,
    /// Once the servers stored it, before the publish kept anything (`CUT_AFTER_STORE`).
    AfterStore,
    /// As [`Cut::AfterStore`], and m22 then publishes `shared` = v3 from the text it had opened
    /// before, which the server refuses as not newer.
    AfterStoreThenStale,
}

/// What [`cut_publish`] saw: whether m22's write was pending and whether it waited after the
/// publish, whether it waited after m22's sync, and what m22's passes signed once m7's batch
/// arrived.
struct CutPublish {
    lab: convergence::Six,
    pending: bool,
    waiting: bool,
    landed: bool,
    signed: Vec<Record>,
}

/// [`writer_over_removal`]'s shape with the publish cut short by `cut`: m22, which holds m7's
/// `Remove` of m42 and not its rotation, publishes `shared` = v2 and the publish ends with an
/// error. m22 then syncs, takes m7's whole batch, and its pass runs twice, a step apart.
fn cut_publish(cut: Cut) -> CutPublish {
    let lab = convergence::six_with(true);
    let (m22, m7) = (&lab.all[0], &lab.all[1]);
    let base = convergence::ids(m7);
    test_remove_unposted(m7, &join_code(&lab.recipients[4])).expect("m7 removes m42");
    let chain = convergence::signed_since(m7, &base);
    save_records(m22, &encode_records(&chain[..1])).unwrap();

    let admin = admin_open(m22).expect("m22 is an admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"v2")
        .expect("edit");
    match cut {
        Cut::BeforeStore => CUT_BEFORE_STORE.with(|cell| cell.set(true)),
        Cut::AfterStore | Cut::AfterStoreThenStale => {
            CUT_AFTER_STORE.with(|cell| cell.set(true));
        }
    }
    let published = admin_publish(m22, &admin, &doc);
    assert!(published.is_err(), "CONTROL: the publish ends early");
    let pending = log_entries::waiting_pending(m22);
    let waiting = log_entries::waiting_file(m22).exists();
    if let Cut::AfterStoreThenStale = cut {
        let mut later = admin.doc.clone();
        later
            .set_entry(&Audience::Everyone, "shared", b"v3")
            .expect("edit");
        let refused = admin_publish(m22, &admin, &later).expect_err("CONTROL: a stale publish");
        assert!(
            refused.to_string().contains("not newer"),
            "CONTROL: the server holds the cut publish's sequence: {refused}"
        );
    }
    sync(m22, None).expect("m22 syncs");
    let landed = log_entries::waiting_file(m22).exists();

    save_records(m22, &encode_records(&chain)).unwrap();
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let mut signed = vault_pass_at(m22, t0).unwrap();
    signed.extend(vault_pass_at(m22, t0 + log_entries::GAP_STEP_MS).unwrap());
    CutPublish {
        lab,
        pending,
        waiting,
        landed,
        signed,
    }
}

/// The `[everyone]` `Put`s among `records` (a `Put`'s name is sealed; m22 writes only `shared`).
fn everyone_puts(records: &[Record]) -> usize {
    records
        .iter()
        .filter(|record| {
            matches!(
                record.body(),
                Body::Put {
                    audience: EpochAudience::Everyone,
                    ..
                }
            )
        })
        .count()
}

/// One-mesh M4 ([`cut_publish`]): a write that waits for the new key is pending on the version
/// that carries it and waits only once a server took that version. A publish that ended before
/// any server took it leaves the write pending: m22's sync reads its old version, and once m7's
/// rotation arrives its passes sign no `Put` of `shared`, so the edit the person saw fail never
/// lands. A publish that ended once the servers stored its version leaves the write pending too,
/// m22's sync reads that version and the write waits, and the pass after the rotation signs the
/// one `Put`.
///
/// NEGATIVE CONTROL: a publish's waiting writes kept straight in `everyone.waiting`, as before
/// (`wait_for_key` landing its pending set at once, `land_waiting_held` after the write;
/// restored with cp from a backup plus touch, confirmed with cmp): the publish no server took
/// signs its `Put` once the rotation arrives, and the test fails "a publish no server took never
/// signs its write".
#[test]
fn a_waiting_write_is_signed_only_once_a_server_took_its_version() {
    let never = cut_publish(Cut::BeforeStore);
    assert_eq!(
        everyone_puts(&never.signed),
        0,
        "a publish no server took never signs its write"
    );
    assert!(
        never.pending && !never.waiting,
        "the write is pending, not waiting"
    );
    assert!(!never.landed, "nothing waits after the sync");
    assert!(
        log_entries::waiting_pending(&never.lab.all[0]),
        "the write stays pending on a version no server holds"
    );
    never.lab.macs.server.stop();

    let stored = cut_publish(Cut::AfterStore);
    assert!(
        stored.pending && !stored.waiting,
        "the write is pending, not waiting"
    );
    assert!(stored.landed, "the sync that reads the version lands it");
    assert!(
        !log_entries::waiting_pending(&stored.lab.all[0]),
        "nothing is pending after"
    );
    assert_eq!(
        everyone_puts(&stored.signed),
        1,
        "the pass after the rotation signs the one Put"
    );
    stored.lab.macs.server.stop();
}

/// One-mesh M4 ([`cut_publish`]): a publish whose version the server stored ends before it hears
/// so, and m22 publishes again, `shared` = v3, from the text it had opened before; the server
/// holds the first version's sequence and refuses the second as not newer. The first publish's
/// write stays pending beside the second's, which the refusal drops: m22's sync reads the stored
/// version and lands it, and the pass after the rotation signs `shared` = v2, which the stored
/// version's text carries.
///
/// NEGATIVE CONTROL: `wait_for_key` replacing the pending sets with its own, as before
/// (`sets.clear();` before its push; restored with cp from a backup plus touch, confirmed with
/// cmp): the refusal drops the only set, m22's sync lands nothing, and the test fails "the sync
/// that reads the stored version lands its write".
#[test]
fn a_waiting_write_on_a_stored_version_outlasts_a_later_publish_refused_as_not_newer() {
    let run = cut_publish(Cut::AfterStoreThenStale);
    assert!(
        run.pending && !run.waiting,
        "the first write is pending, not waiting"
    );
    assert!(
        run.landed,
        "the sync that reads the stored version lands its write"
    );
    assert!(
        !log_entries::waiting_pending(&run.lab.all[0]),
        "nothing is pending after"
    );
    assert_eq!(
        everyone_puts(&run.signed),
        1,
        "the pass after the rotation signs the one Put"
    );
    assert_eq!(
        convergence::shared(&run.lab.all[0]).as_deref(),
        Some(b"v2".as_slice()),
        "m22 reads the stored version's edit"
    );
    run.lab.macs.server.stop();
}

/// The audience a `Epoch`, `Put` or `Delete` record names, for a failure's message.
fn audience_of(record: &Record) -> Option<EpochAudience> {
    match record.body() {
        Body::Epoch { audience, .. }
        | Body::Put { audience, .. }
        | Body::Delete { audience, .. } => Some(audience.clone()),
        _ => None,
    }
}

/// What [`concurrent_removals_and_a_write`] saw: the lab, the scaled stop and limit, the keyed
/// records before the publish, what the publish and then the passes signed, and whether a write
/// still waits on m7 after them.
struct ConcurrentRemovals {
    lab: convergence::Six,
    stop: usize,
    limit: usize,
    keyed: usize,
    published: Vec<Record>,
    signed: Vec<Record>,
    waits_after: bool,
}

/// Two concurrent removals and a held write: of six Macs (m22 and m7 admins), m22 removes m42
/// and m40 removes m43, each on its own side, and every Mac takes both batches: each newest
/// `[everyone]` and Commons epoch wraps a removed Mac, so both are due a coalesce a held `Remove`
/// made due, the removers' to sign first. The keyed budget is scaled so the log's records fill it
/// as 8,600 fill 12,288, past the stop. At that fill m7, an admin and no remover (its own pass
/// waits the removers' grace), publishes `shared` = v2, and then the four remaining Macs' passes
/// run 16 steps, `stop_holds` setting `remove::STOP_HOLDS_REMOVALS` for all of it.
fn concurrent_removals_and_a_write(stop_holds: bool) -> ConcurrentRemovals {
    let lab = convergence::six_with(true);
    let (m22, m7, m40) = (&lab.all[0], &lab.all[1], &lab.all[2]);
    test_remove_unposted(m22, &join_code(&lab.recipients[4])).expect("m22 removes m42");
    test_remove_unposted(m40, &join_code(&lab.recipients[5])).expect("m40 removes m43");
    convergence::carry(&lab.all, 0);
    convergence::carry(&lab.all, 2);
    let log = log_of(m7);
    for audience in [EpochAudience::Everyone, EpochAudience::Commons] {
        assert!(
            log.rekey_dues()
                .iter()
                .any(|due| due.audience == audience && due.rotation),
            "CONTROL: {audience:?} is due a coalesce the removals made"
        );
    }
    let keyed = log.counts().keyed;
    let budgets = scaled_budget(keyed, 8_600);
    let stop = budgets.automatic_keyed_stop();
    assert!(
        keyed > stop,
        "CONTROL: the fill is past the stop: {keyed} > {stop}"
    );

    let before = convergence::ids(m7);
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    remove::STOP_HOLDS_REMOVALS.with(|cell| cell.set(stop_holds));
    let (published, signed) = budgets.within(|| {
        let admin = admin_open(m7).expect("m7 is an admin");
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"v2")
            .expect("edit");
        admin_publish(m7, &admin, &doc).expect("the person's publish does not fail");
        let published = convergence::signed_since(m7, &before);
        (published, passes(&lab.all[..4], t0, 16))
    });
    remove::STOP_HOLDS_REMOVALS.with(|cell| cell.set(false));
    let waits_after = log_entries::waiting_file(m7).exists();
    ConcurrentRemovals {
        stop,
        limit: budgets.keyed,
        keyed,
        published,
        signed,
        waits_after,
        lab,
    }
}

/// One-mesh §8 and M4 together ([`concurrent_removals_and_a_write`]): at a fill of 8,600 keyed
/// records scaled, past the stop, the coalesce of `[everyone]` and of Commons that the two
/// concurrent removals made due is signed, one epoch of each, because writers wait on it; m7's
/// publish signed nothing, and its `Put` is signed after the coalesce, under the new
/// `[everyone]` epoch, which wraps neither removed Mac. The log then holds more than the stop and
/// no more than its limit, and nothing waits.
///
/// NEGATIVE CONTROL: `remove::STOP_HOLDS_REMOVALS` (the stop holds the removals' coalesce too),
/// run in the test as a CONTROL, and once with it set for the first run too: the same passes sign
/// nothing, m7's write still waits, and the test fails "the coalesce is signed past the stop",
/// left `{}`, right `{Everyone: 1, Commons: 1}`.
#[test]
fn two_concurrent_removals_past_the_stop_coalesce_and_the_held_put_is_then_signed() {
    let run = concurrent_removals_and_a_write(false);
    let r = &run.lab.recipients;
    assert!(
        run.published.is_empty(),
        "the publish signs nothing: {:?}",
        run.published
            .iter()
            .map(|record| (record.kind(), audience_of(record)))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        convergence::epochs_by_audience(&run.signed),
        BTreeMap::from([(EpochAudience::Everyone, 1), (EpochAudience::Commons, 1)]),
        "the coalesce is signed past the stop"
    );
    let m7 = &run.lab.all[1];
    let log = log_of(m7);
    let identity = read_identity(m7).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let newest = opened.maximal_epochs[&EpochAudience::Everyone].clone();
    assert_eq!(newest.len(), 1, "one newest [everyone] epoch");
    let wrapped = &opened.wrapped_recipients[&newest[0]];
    assert!(!wrapped.contains(&r[4]) && !wrapped.contains(&r[5]));
    let (_, actor7, _) = member_key(m7);
    let winner = opened.winning_ops[&(Audience::Everyone, "shared".to_owned())];
    let put = log.get(&winner).expect("the winner");
    assert_eq!(*put.actor(), actor7, "m7's write is the winner");
    assert!(
        matches!(put.body(), Body::Put { epoch, .. } if *epoch == newest[0]),
        "m7's Put goes under the coalesced epoch"
    );
    assert!(
        run.signed.iter().any(|record| record.id() == &winner),
        "signed by the passes, after the publish"
    );
    for paths in &run.lab.all[..4] {
        assert_eq!(
            convergence::shared(paths).as_deref(),
            Some(b"v2".as_slice()),
            "every member reads the edit"
        );
    }
    assert!(!run.waits_after, "nothing waits");
    let keyed_after = log.counts().keyed;
    assert!(
        run.stop < run.keyed && keyed_after <= run.limit,
        "past the stop ({} < {}), within the limit ({keyed_after} <= {})",
        run.stop,
        run.keyed,
        run.limit
    );
    run.lab.macs.server.stop();

    let control = concurrent_removals_and_a_write(true);
    assert_eq!(
        convergence::epochs_by_audience(&control.signed),
        BTreeMap::new(),
        "CONTROL: a stop that holds the removals' coalesce signs nothing"
    );
    assert!(control.waits_after, "CONTROL: m7's write still waits");
    control.lab.macs.server.stop();
}

/// One-mesh §8 for `aterm secrets revoke`: its log half (`log_entries::draft_retire_in_log_with`)
/// is a person's removal too, and reserves the room of the re-keys it makes due as `members
/// remove` does. m22 revokes m40: in a budget a keyed record short of the log's records, the
/// batch and the worst case it prints, it refuses with the fill, signs nothing and m40 stays a
/// member; in the budget that takes all three it removes m40.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp):
/// `draft_retire_in_log_with` with no fill check (`if false && fill.passes()`): the revoke at the
/// log's own keyed count signs into the log's hard bound and fails "removal cannot fit: at least
/// 13 complete records … (membership log holds its limit of keyed records (Epoch, Wrap, Put,
/// Delete, Node, Name))", with no fill, and the first refusal assert fails.
#[test]
fn a_revoke_whose_worst_case_passes_the_keyed_budget_refuses_and_prints_the_fill() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"never-printed".as_slice())]);
    let m22 = &macs.m22;
    let (_m40, actor40, _) = let_in(&macs, "m40", "m40");
    sync(m22, None).expect("m22 syncs m40's admission");
    let admin = admin_open(m22).expect("admin");
    assert!(
        admin.doc.machines().iter().any(|m| m.name == "m40"),
        "CONTROL: the envelope names m40"
    );
    let before = log_of(m22);
    let keyed = before.counts().keyed;
    let held: BTreeSet<RecordId> = before.records().map(|r| *r.id()).collect();
    let refused = |limit: usize| {
        let Err(error) = keyed_budget(limit).within(|| admin_revoke(m22, &admin, "m40", false))
        else {
            panic!("the revoke refuses at {limit}");
        };
        let text = error.to_string();
        assert!(text.contains("the membership log is full"), "{text}");
        assert!(!text.contains("never-printed"), "no value: {text}");
        let after = log_of(m22);
        assert_eq!(
            after.records().map(|r| *r.id()).collect::<BTreeSet<_>>(),
            held,
            "nothing is signed"
        );
        assert!(after.membership().contains(&actor40), "m40 stays a member");
        numbers_of(&text)
    };
    let fill = refused(keyed);
    let [_, batch, worst, _] = fill.as_slice() else {
        panic!("the fill's four numbers: {fill:?}");
    };
    let room = keyed + batch + worst;
    assert!(*batch > 0 && *worst > 0, "{fill:?}");
    assert_eq!(
        refused(room - 1),
        vec![keyed, *batch, *worst, room - 1],
        "a record short"
    );
    keyed_budget(room)
        .within(|| admin_revoke(m22, &admin, "m40", false))
        .expect("CONTROL: with room for the worst case m22 revokes m40");
    assert!(log_of(m22).membership().removal(&actor40).is_some());
    macs.server.stop();
}

/// The worst case of a removal ([`RemovalFill`]) counts two re-keys of an audience the remover's
/// batch does not mint (a ranked holder's rotation, then a coalesce of the heads a concurrent
/// removal leaves) and one of each it does. m22 puts `ring-cred` in `[m22 m40]`, which m7 is not
/// in; m7's removal of m40 mints `[everyone]` and Commons, so its fill's worst case passes its
/// batch by exactly twice the bound of the ring's re-key, which is in no batch.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp):
/// `RemovalFill::of` with one re-key for every audience (`let rekeys = 1`): fails "two re-keys of
/// the ring: [14, 8, 11, 14], the ring's bound 3", left 3, right 6.
#[test]
fn a_removal_reserves_two_rekeys_of_an_audience_it_does_not_mint() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_m40, _, rec40) = let_in(&macs, "m40", "m40");
    sync(m22, None).expect("m22 syncs m40's admission");
    let ring = Audience::Hosts(vec!["m22".to_owned(), "m40".to_owned()]);
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&ring, "ring-cred", b"ring-v1").expect("ring");
    admin_publish(m22, &admin, &doc).expect("publish the ring");
    sync(m7, None).expect("m7 syncs");
    let lists = EpochAudience::from(&ring);
    let log = log_of(m7);
    assert!(
        log.wraps_maximal_audience_to(&lists, &rec40),
        "CONTROL: the ring's epoch wraps m40"
    );
    let identity = read_identity(m7).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    assert!(
        !log_entries::opens_newest(&opened, &lists)
            && log_entries::opens_newest(&opened, &EpochAudience::Everyone)
            && log_entries::opens_newest(&opened, &EpochAudience::Commons),
        "CONTROL: m7 mints [everyone] and Commons, not the ring"
    );
    let (_, me, _) = member_key(m7);
    let flight = log_entries::Flight::of_batch(&log, &opened, &me);
    let mut gone = log.removed_recipients();
    gone.insert(rec40);
    let ring_bound = log_entries::rekey_bound(
        &log,
        &opened,
        &flight,
        &lists,
        &BTreeSet::new(),
        &gone,
        None,
    )
    .keyed;
    assert!(ring_bound > 0);

    let keyed = log.counts().keyed;
    let error = keyed_budget(keyed)
        .within(|| remove_member(m7, &join_code(&rec40), false))
        .expect_err("the removal refuses");
    let fill = numbers_of(&error.to_string());
    let [_, batch, worst, _] = fill.as_slice() else {
        panic!("the fill's four numbers: {fill:?}");
    };
    assert_eq!(
        worst - batch,
        2 * ring_bound,
        "two re-keys of the ring: {fill:?}, the ring's bound {ring_bound}"
    );
    macs.server.stop();
}

/// F21 and the planned cost ([`log_entries::rekey_bound`]): for an audience a Mac does not
/// open whole, the bound counts the names it reads, its keyed tombstones, and each `Put` and
/// `Delete` sealed under an epoch whose key it lacks, not every write the log ever held there. On
/// m41 of the keyed split ([`keyed_split`]), which lacks the key of the uncovered keyed epoch E,
/// after m22 writes `other` sixteen times: the bound lies below the count of `[everyone]` writes
/// the log holds (which the old bound added its epoch and `Wrap`s to), and never below what m7's
/// reseal of `[everyone]` signs.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): the bound
/// counting every `Put` and `Delete` of the audience (its old arm): fails "the bound counts no
/// superseded write: 31, of 25 writes".
#[test]
fn the_planned_cost_of_an_audience_not_opened_whole_counts_no_superseded_write() {
    let split = keyed_split(false);
    split.heal();
    let (m22, m7, m41) = (&split.macs.m22, &split.macs.m7, &split.m41);
    for i in 0..16 {
        test_put_everyone(m22, "other", format!("o-{i}").as_bytes()).expect("m22 writes other");
    }
    split.heal();
    let log = log_of(m41);
    let identity = read_identity(m41).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    assert!(
        !opened
            .fully_unwrapped_audiences
            .contains(&EpochAudience::Everyone),
        "CONTROL: m41 does not open [everyone] whole"
    );
    let (_, a41, _) = member_key(m41);
    let flight = log_entries::Flight::of_batch(&log, &opened, &a41);
    let seats = log.current_recipients(&EpochAudience::Everyone);
    let removed = log.removed_recipients();
    let bound = log_entries::rekey_bound(
        &log,
        &opened,
        &flight,
        &EpochAudience::Everyone,
        &seats,
        &removed,
        None,
    )
    .keyed;
    let writes = log
        .records()
        .filter(|record| {
            matches!(record.body(),
                Body::Put { audience, .. } | Body::Delete { audience, .. }
                    if *audience == EpochAudience::Everyone)
        })
        .count();
    assert!(
        bound < writes,
        "the bound counts no superseded write: {bound}, of {writes} writes"
    );

    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let resealed: Vec<Record> = tend_epochs_at(m7, t0).expect("m7 tends");
    let epoch = resealed
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
        .expect("CONTROL: m7 reseals [everyone]");
    let signed = resealed
        .iter()
        .filter(|record| match record.body() {
            Body::Epoch { audience, .. }
            | Body::Put { audience, .. }
            | Body::Delete { audience, .. } => *audience == EpochAudience::Everyone,
            Body::Wrap { epoch: of, .. } => *of == epoch || *of == split.epoch,
            _ => false,
        })
        .count();
    assert!(
        signed <= bound,
        "the bound is never below what the reseal signs: {signed} > {bound}"
    );
    split.macs.server.stop();
}

/// What the recording table-cuts hook saw: each call's vault, target and pass, and whether the
/// target was still a member then.
static TABLE_CUT_CALLS: std::sync::Mutex<Vec<(std::path::PathBuf, MemberId, CutPass, bool)>> =
    std::sync::Mutex::new(Vec::new());

fn record_table_cuts(paths: &Paths, member: &MemberId, pass: CutPass) -> usize {
    // Installed for the whole test binary, so it never fails another test's removal.
    let seated = load_records(paths)
        .ok()
        .and_then(|store| store.log().map(|log| log.membership().contains(member)))
        .unwrap_or(false);
    TABLE_CUT_CALLS
        .lock()
        .unwrap()
        .push((paths.dir.clone(), *member, pass, seated));
    0
}

/// `aterm secrets revoke` signs its table cutoffs too (one-mesh §4.10, B11): the vault
/// revocation runs the installed table-cuts hook on the revoked seat, before its `Remove`, as the
/// first remover. CONTROL: no call names m40 before the revoke.
#[test]
fn a_vault_revocation_cuts_the_revoked_seats_tables_before_its_remove() {
    on_table_cuts(record_table_cuts);
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m22 = &macs.m22;
    let (_m40, actor40, _) = let_in(&macs, "m40", "m40");
    sync(m22, None).expect("m22 syncs m40's admission");
    let calls = || -> Vec<(CutPass, bool)> {
        TABLE_CUT_CALLS
            .lock()
            .unwrap()
            .iter()
            .filter(|(dir, member, ..)| *dir == m22.dir && *member == actor40)
            .map(|(_, _, pass, seated)| (*pass, *seated))
            .collect()
    };
    assert!(calls().is_empty(), "CONTROL: nothing cut m40 yet");
    let admin = admin_open(m22).expect("admin");
    admin_revoke(m22, &admin, "m40", false).expect("m22 revokes m40");
    assert!(log_of(m22).membership().removal(&actor40).is_some());
    assert_eq!(
        calls(),
        vec![(CutPass::BeforeRemove, true)],
        "one first remover's cut, while m40 was still seated"
    );
    macs.server.stop();
}
