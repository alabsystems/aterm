// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::log_entries_tests::{
    TwoMacs, copy_private_tree, hand, join_host, member_key, two_macs, two_macs_with,
};
use crate::commons::bootstrap::Bootstrap;
use crate::membership_log::encode_records;

#[path = "convergence_tests.rs"]
mod convergence;

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
    let current = swarm_roster(paths).unwrap().unwrap();
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
///
/// NEGATIVE CONTROL: make `revoke_by_log` return `removes_by_log(paths)` alone (the routing
/// before): the `"revoke sends an unproved founder to the admin"` assert fails.
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
            "{audience:?}"
        );
        assert!(
            log.wraps_maximal_audience_to(&audience, &m7_rec),
            "{audience:?}"
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
        let roster = crate::client::swarm_roster(paths)
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
        let roster = crate::client::swarm_roster(paths)
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
            let _ = keyed_epochs_transition(&mut h, action);
        }
        h
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
            Log::admission_epochs(&opened22, &[EpochAudience::Everyone])
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
/// NEGATIVE CONTROLS (each restored with cp from a backup plus touch, and confirmed with cmp):
/// - no reseal (`reseal_ready` answers `false`, as before rule 5): fails `Reseal after ["Heal",
///   "CloseGap"] gave {"covered": 0, "edited": 0, "gap": 0, "healed": 1, "hx": 1, "partial": 0,
///   "sx": 1, "t": 0, "win": 2}`.
/// - rule 4's full reseal reduced to any counting epoch (`full` returns `true`): the partial mint
///   covers E, and the test fails `Reseal after ["Heal", "PartialMint", "CloseGap"] gave
///   {"covered": 1, "edited": 0, "gap": 0, "healed": 1, "hx": 0, "partial": 1, "sx": 1, "t": 0,
///   "win": 2}`.
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
    let observed = KeyedEpochsHarness::replay(&fixture, &root.path().join("s0"), &[]).observe();
    assert_eq!(observed, start, "state mismatch after []");
    let mut judged = BTreeSet::from([&start]);
    let mut run_id = 0usize;
    for (state, path) in &paths {
        for action in &model.actions {
            if !model.action_enabled(action.name, state) {
                continue;
            }
            run_id += 1;
            let mut h =
                KeyedEpochsHarness::replay(&fixture, &root.path().join(format!("t{run_id}")), path);
            let next_obs = keyed_epochs_transition(&mut h, action.name);
            let next_path = [path.as_slice(), &[action.name]].concat();
            match found_by.get(next_path.as_slice()) {
                Some(&next) => {
                    assert_eq!(&next_obs, next, "state mismatch after {next_path:?}");
                    judged.insert(next);
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
