// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::log_entries_tests::{
    TwoMacs, hand, join_host, member_key, two_macs, two_macs_with,
};
use crate::commons::bootstrap::Bootstrap;
use crate::membership_log::encode_records;

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
    let (paths, request, allowed) = asker(macs.root.path(), &macs.m7, dir, name);
    let (bundle, logged) = hand(&macs.m7, &request, &allowed, true);
    assert!(logged, "CONTROL: m7 hands the log path");
    write_from_log_bundle(&paths, &bundle).expect("the Mac writes from the log");
    post_records_to_servers(&macs.m7).expect("m7 posts the admission");
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
/// `[everyone]` and Commons epoch. After the heal, with no Commons handshake, the vault pass's
/// epoch tender on one Mac (the first ranked) mints one higher epoch of each covering both
/// removals, the other Mac waits its rank's step and then has nothing to do, and the newest epochs
/// are wrapped to neither removed Mac.
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
    assert!(healed.rotation_due(&id22.recipient(), &active_now));

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
    assert!(!covered.rotation_due(&id.recipient(), &active_now));

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
        reads_log: true,
        restore,
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
            reads_log: true,
            restore: true,
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
            reads_log: true,
            restore: false,
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
            reads_log: true,
            restore: true,
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
/// NEGATIVE CONTROL: make `Log::hosts_rotation_due` return an empty list: m22's passes mint
/// nothing for `[m22 m40]`, whose newest epoch still wraps m40, and the
/// `"m22 rotates [m22 m40]"` assert fails.
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

    // m22's pass runs the tender at the wall clock, which mints at once when m22 ranks first; a
    // pass past every rank's step mints it when m22 ranks second.
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
