// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Member writes (`docs/DESIGN-claude-setup-token-2026-11-05.md` §3.4–§3.6, S2 and S3; part of
//! one-mesh B13): a seated Mac without the admin key writes a shared name into the vault's log
//! and removes one under a fresh `[everyone]` epoch, what it refuses signs nothing, and a write
//! that cannot be signed now waits for the vault pass. The tests of `log_entries/member.rs` that
//! need the removal lab ([`convergence::six`]), declared beside the removal tests for its
//! fixtures.

use super::waiting::{held_remove, reads, two_passes};
use super::*;

/// A member's plain `set` of `name` to `value`.
fn entry(name: &str, value: &[u8]) -> MemberEntry {
    MemberEntry {
        name: name.to_owned(),
        value: SecretBytes::from(value.to_vec()),
        if_absent: false,
        anchor: Anchor::Keep,
        fleet_reads_anchors: false,
    }
}

/// A person's patience: the hang detector, never a latency budget.
fn refuse() -> OnBusy {
    OnBusy::Refuse(std::time::Instant::now() + std::time::Duration::from_secs(60))
}

/// The `[everyone]` `Put`s among `records`.
fn puts(records: &[Record]) -> usize {
    everyone_puts(records)
}

/// A seated Mac with no admin key (m7) sets a shared name with no admin: the write is posted,
/// and another Mac (m22) reads it from the vault's log after a pass, as its file.
#[test]
fn a_seated_mac_without_the_admin_key_sets_a_shared_name_another_mac_reads() {
    let macs = two_macs(&[(
        Audience::Everyone,
        "gh_access_token",
        b"token-v1".as_slice(),
    )]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    assert!(
        matches!(can_administer(m7), Ok(false)),
        "CONTROL: m7 holds no admin key"
    );
    let soon = std::time::Instant::now() + std::time::Duration::from_secs(60);
    assert_eq!(write_route(m7, soon).unwrap(), WriteRoute::Seated);
    let written = member_set(m7, &entry("gh_access_token", b"token-v2"), refuse()).unwrap();
    assert_eq!(written, MemberWritten::Posted);
    sync(m22, None).expect("m22 syncs");
    assert_eq!(
        std::fs::read(m22.home.join(".secrets/gh_access_token")).unwrap(),
        b"token-v2",
        "m22 writes m7's value"
    );
    assert!(!log_entries::waiting_file(m7).exists(), "nothing waits");
    // The same value again signs nothing more and still reads as posted.
    let before = convergence::ids(m7);
    let again = member_set(m7, &entry("gh_access_token", b"token-v2"), refuse()).unwrap();
    assert_eq!(again, MemberWritten::Posted);
    assert_eq!(
        convergence::ids(m7),
        before,
        "an unchanged value signs nothing"
    );
}

/// Each class a member may not write is refused: it signs nothing and writes no waiting file. A
/// control record, release material, fleet configuration, git's config, a home path aterm does
/// not name, a case variant of release material, a name that folds onto one the vault holds, a
/// CLI login and the Claude Code token class. CONTROL: a shared name, set the same way, signs.
#[test]
fn each_name_a_member_may_not_write_signs_nothing_and_waits_nowhere() {
    const TOKEN: &str = "aterm-claude-token-ac-0123456789abcdef";
    let macs = two_macs(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    test_put_everyone(m22, "shared-pair", b"p1").expect("m22 puts shared-pair");
    sync(m7, None).expect("m7 syncs");
    for name in [
        crate::entry_class::FLEET_CODE,
        crate::SWARM_ROSTER_ENTRY,
        crate::entry_class::RELEASE_GH_TOKEN,
        crate::entry_class::FLEET_ACCOUNTS,
        crate::entry_class::GIT_CONFIG,
        "~/.zshrc",
        "GH_ACCESS_TOKEN_ALABSYSTEMS",
        "Shared-Pair",
        "~/.codex/auth.json",
        TOKEN,
    ] {
        let before = convergence::ids(m7);
        let written = member_set(m7, &entry(name, b"from a member"), refuse()).unwrap();
        let MemberWritten::Refused(why) = &written else {
            panic!("{name}: {written:?}");
        };
        assert!(!why.is_empty(), "{name}");
        assert_eq!(convergence::ids(m7), before, "{name} signs nothing");
        assert!(
            !log_entries::waiting_file(m7).exists(),
            "{name} waits nowhere"
        );
    }
    let folded = member_set(m7, &entry("Shared-Pair", b"x"), refuse()).unwrap();
    assert_eq!(
        folded,
        MemberWritten::Refused(crate::entry_class::folded_pair_refusal(
            "Shared-Pair",
            "shared-pair"
        ))
    );
    let before = convergence::ids(m7);
    assert_eq!(
        member_set(m7, &entry("shared-control", b"ok"), refuse()).unwrap(),
        MemberWritten::Posted,
        "CONTROL"
    );
    assert_eq!(puts(&convergence::signed_since(m7, &before)), 1, "CONTROL");
}

/// While a removal's rotation has not reached this Mac (m22 holds m7's `Remove` of m42 and not
/// its new key), a member write waits in `everyone.waiting`, signing nothing, and reads "not yet
/// in vault"; a second write of the name replaces it there. Once the rotation arrives, the next
/// vault pass signs the later write alone under the new key, and journals nothing overtaken.
#[test]
fn a_write_with_a_rotation_due_waits_and_the_next_pass_signs_it() {
    let lab = convergence::six();
    let chain = held_remove(&lab);
    let m22 = &lab.all[0];
    let before = convergence::ids(m22);
    let written = member_set(m22, &entry("shared", b"v2"), refuse()).unwrap();
    assert_eq!(written, MemberWritten::Waiting(WaitFor::RotationDue));
    assert_eq!(
        convergence::ids(m22),
        before,
        "nothing signed while it waits"
    );
    assert!(log_entries::waiting_file(m22).exists(), "it waits");
    assert_eq!(reads(m22, true, "shared"), Some(b"v1".to_vec()));
    let again = member_set(m22, &entry("shared", b"v3"), refuse()).unwrap();
    assert_eq!(again, MemberWritten::Waiting(WaitFor::PendingOfThisMac));
    assert_eq!(convergence::ids(m22), before, "still nothing signed");

    save_records(m22, &encode_records(&chain)).unwrap();
    let signed = two_passes(m22);
    assert_eq!(puts(&signed), 1, "the pass signs the later write alone");
    assert_eq!(reads(m22, true, "shared"), Some(b"v3".to_vec()));
    assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
    assert!(
        log_entries::read_overtaken(m22).unwrap().is_empty(),
        "the earlier write is replaced, not overtaken"
    );
    lab.macs.server.stop();
}

/// A rotation that becomes due between the draft and its commit (m7's `Remove` of m42 reaches m22
/// there) makes the draft's re-run wait: the store the checks judge is never stale, and the `Put`
/// the first draft signed is never committed. CONTROL: with no such arrival, the same write signs.
#[test]
fn a_rotation_that_lands_before_the_commit_makes_the_re_run_wait() {
    let lab = convergence::six();
    let (m22, m7) = (lab.all[0].clone(), lab.all[1].clone());
    let base = convergence::ids(&m7);
    test_remove_unposted(&m7, &join_code(&lab.recipients[4])).expect("m7 removes m42");
    let remove = encode_records(&convergence::signed_since(&m7, &base)[..1]);
    let arrived = std::rc::Rc::new(std::cell::Cell::new(false));
    let hook = {
        let (m22, arrived) = (m22.clone(), arrived.clone());
        log_entries::author_batch_tests::install_hook(move || {
            if !arrived.replace(true) {
                save_records(&m22, &remove).unwrap();
            }
        })
    };
    let before = convergence::ids(&m22);
    let written = member_set(&m22, &entry("shared", b"v2"), refuse()).unwrap();
    drop(hook);
    assert!(arrived.get(), "the Remove arrived before the commit");
    assert_eq!(written, MemberWritten::Waiting(WaitFor::RotationDue));
    assert_eq!(
        puts(&convergence::signed_since(&m22, &before)),
        0,
        "no Put is committed"
    );
    assert_eq!(reads(&m22, true, "shared"), Some(b"v1".to_vec()));

    let m40 = &lab.all[2];
    assert_eq!(
        member_set(m40, &entry("other", b"o1"), refuse()).unwrap(),
        MemberWritten::Posted,
        "CONTROL: a Mac with no rotation due signs"
    );
    lab.macs.server.stop();
}

/// With no server answering, a member write is signed into this Mac's log alone: `SignedHere`,
/// which reads "not yet in vault", and this Mac's log holds the value.
#[test]
fn a_write_no_server_takes_is_signed_here() {
    let macs = two_macs(&[]);
    let m7 = &macs.m7;
    macs.server.stop();
    let written = member_set(m7, &entry("offline", b"o1"), refuse()).unwrap();
    assert_eq!(written, MemberWritten::SignedHere);
    assert_eq!(reads(m7, true, "offline"), Some(b"o1".to_vec()));
}

/// A waiting write that another Mac's later write overtakes before the outcome is read reports
/// `Overtaken`, naming that Mac: m22's `shared` = v2 waits for the rotation; m7, which holds it,
/// sets `shared` = v3; the pass on m22 then finds the log's value none of its write's bases.
#[test]
fn an_overtaken_write_reports_overtaken() {
    let lab = convergence::six();
    let _ = held_remove(&lab);
    let (m22, m7) = (lab.all[0].clone(), lab.all[1].clone());
    let hook = {
        let (m22, m7) = (m22.clone(), m7.clone());
        move || {
            test_put_everyone(&m7, "shared", b"v3").expect("m7 sets shared = v3");
            save_records(&m22, &load_records(&m7).unwrap().encode()).unwrap();
            let _ = two_passes(&m22);
        }
    };
    log_entries::BEFORE_OUTCOME.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    let written = member_set(&m22, &entry("shared", b"v2"), refuse());
    log_entries::BEFORE_OUTCOME.with(|slot| *slot.borrow_mut() = None);
    let MemberWritten::Overtaken(Some(by)) = written.unwrap() else {
        panic!("an overtaken write");
    };
    assert_eq!(by.label, "m7");
    assert_eq!(reads(&m22, true, "shared"), Some(b"v3".to_vec()));
    lab.macs.server.stop();
}

/// A vault pass that holds this Mac's state past the deadline makes a person's write `Busy`,
/// never the admin route's fallback, and nothing waits. With `Wait`, a draft claim a pass holds
/// past the deadline lands the write in `everyone.waiting` instead (`LockDeadline`), and the next
/// pass signs it.
#[test]
fn a_busy_vault_refuses_a_person_and_keeps_a_waiting_write() {
    let macs = two_macs(&[]);
    let m7 = &macs.m7;
    {
        let _held = test_hold_state_lock(m7).expect("hold the state");
        let by = std::time::Instant::now() + std::time::Duration::from_millis(50);
        let written = member_set(m7, &entry("busy", b"b1"), OnBusy::Refuse(by)).unwrap();
        assert_eq!(written, MemberWritten::Busy);
    }
    assert!(!log_entries::waiting_file(m7).exists(), "nothing waits");

    let claim = log_entries::pass_claim(m7)
        .unwrap()
        .expect("a pass's claim");
    let written = member_set(
        m7,
        &entry("busy", b"b2"),
        OnBusy::Wait(std::time::Instant::now()),
    )
    .unwrap();
    drop(claim);
    assert_eq!(written, MemberWritten::Waiting(WaitFor::LockDeadline));
    assert!(log_entries::waiting_file(m7).exists(), "it waits");
    let _ = vault_pass_at(m7, aterm_time::unix_millis().unwrap_or(0)).unwrap();
    assert_eq!(reads(m7, true, "busy"), Some(b"b2".to_vec()));
}

/// A log vault that seats no device key of this Mac's writes nothing: `NotSeated`, signing
/// nothing, waiting nowhere.
#[test]
fn a_seatless_mac_is_not_seated() {
    let macs = two_macs(&[]);
    let m7 = &macs.m7;
    let device = m7.dir.join("commons").join("device.age");
    std::fs::rename(&device, device.with_extension("set-aside")).unwrap();
    let soon = std::time::Instant::now() + std::time::Duration::from_secs(60);
    assert_eq!(write_route(m7, soon).unwrap(), WriteRoute::NotSeated);
    let before = convergence::ids(m7);
    let written = member_set(m7, &entry("seatless", b"s1"), refuse()).unwrap();
    assert_eq!(written, MemberWritten::NotSeated);
    assert_eq!(convergence::ids(m7), before);
    assert!(!log_entries::waiting_file(m7).exists());
}

/// A host Mac that opens a Hosts audience holding a name refuses a member write of it, naming the
/// hosts, and counts the copy that shadows `[everyone]`; a Mac outside that audience sees no such
/// copy, and its write of the name is posted.
#[test]
fn a_host_mac_refuses_a_name_its_hosts_hold_and_counts_the_shadow() {
    let macs = two_macs_with(
        &[
            (Audience::Everyone, "hosted", b"e1".as_slice()),
            (Audience::Hosts(vec!["m7".to_owned()]), "hosted", b"h1"),
        ],
        false,
    );
    let (m22, m7) = (&macs.m22, &macs.m7);
    let before = convergence::ids(m7);
    let written = member_set(m7, &entry("hosted", b"e2"), refuse()).unwrap();
    let MemberWritten::Refused(why) = &written else {
        panic!("{written:?}");
    };
    assert!(
        why.contains("is also held for [m7], which this Mac is one of"),
        "{why}"
    );
    assert_eq!(convergence::ids(m7), before, "nothing signed");
    assert_eq!(per_host_shadows(m7).unwrap(), 1, "the host counts its copy");
    assert_eq!(per_host_shadows(m22).unwrap(), 0, "m22 sees no host copy");
    assert_eq!(
        member_set(m22, &entry("hosted", b"e2"), refuse()).unwrap(),
        MemberWritten::Posted,
        "outside the audience, the write is posted"
    );
}

/// A person's patience for an unset: the hang detector, never a latency budget.
fn soon() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(60)
}

/// The `[everyone]` epoch that seals `name`'s winning value, as the Mac at `paths` opens its log.
fn sealing(paths: &Paths, name: &str) -> RecordId {
    let log = log_of(paths);
    let opened = log
        .open_audience_entries(&read_identity(paths).unwrap().unwrap())
        .unwrap()
        .expect("an opened log");
    let op = opened.winning_ops[&(Audience::Everyone, name.to_owned())];
    match log.get(&op).map(Record::body) {
        Some(Body::Put { epoch, .. }) => *epoch,
        other => panic!("{name}'s winning op is no Put: {other:?}"),
    }
}

/// Whether the Mac at `paths` holds the key of `epoch`, counting or keyed.
fn holds_key(paths: &Paths, epoch: &RecordId) -> bool {
    let opened = log_of(paths)
        .open_audience_entries(&read_identity(paths).unwrap().unwrap())
        .unwrap()
        .expect("an opened log");
    opened.unwrapped_keys.contains_key(epoch) || opened.keyed_keys.contains_key(epoch)
}

/// Where the materializer writes the entry `name` on the Mac at `paths`.
fn file_of(paths: &Paths, name: &str) -> std::path::PathBuf {
    name.strip_prefix("~/").map_or_else(
        || paths.home.join(".secrets").join(name),
        |rel| paths.home.join(rel),
    )
}

/// What [`unset_then_admit`] saw.
struct Admitted {
    unset: MemberUnset,
    /// What m7's unset signed.
    signed: Vec<Record>,
    /// Whether m40, admitted by m7 after the unset, holds the key that sealed the removed value.
    holds_removed_key: bool,
    /// m40's file of the entry that stays, and whether it has one of the removed entry.
    stays: Vec<u8>,
    removed_file: bool,
}

/// m7, seated with no admin key, unsets `secret`, then lets m40 in: what m40 is handed. With
/// `old_epoch`, the negative control: the `Delete` is signed under the epoch a writer seals
/// under, as a plain `Delete` is, and nothing is sealed again.
fn unset_then_admit(old_epoch: bool) -> Admitted {
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"everyone-v1".as_slice()),
        (Audience::Everyone, "secret", b"removed-before-m40"),
    ]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    assert!(matches!(can_administer(m7), Ok(false)), "CONTROL: no admin");
    let sealed = sealing(m7, "secret");
    let before = convergence::ids(m7);
    log_entries::UNSET_UNDER_OLD_EPOCH.with(|on| on.set(old_epoch));
    let unset = member_unset(m7, "secret", soon());
    log_entries::UNSET_UNDER_OLD_EPOCH.with(|on| on.set(false));
    let unset = unset.unwrap();
    let signed = convergence::signed_since(m7, &before);
    sync(m22, None).expect("m22 syncs");
    assert!(
        !m22.home.join(".secrets/secret").exists(),
        "m22 takes it back"
    );
    let (m40, _, _) = let_in_by(&macs, m7, "m40", "m40");
    let admitted = Admitted {
        unset,
        signed,
        holds_removed_key: holds_key(&m40, &sealed),
        stays: std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        removed_file: m40.home.join(".secrets/secret").exists(),
    };
    macs.server.stop();
    admitted
}

/// A member's unset mints a fresh `[everyone]` epoch, wrapped to every current seat, seals the
/// entry that stays again under it and deletes the name there, so the epoch that sealed the
/// removed value is neither newest nor winning: m40, admitted by m7 afterwards and handed every
/// record, the old `Put` included, holds no key that opens it, and reads the entry that stays.
/// NEGATIVE CONTROL: a plain `Delete` under the old epoch ([`log_entries::UNSET_UNDER_OLD_EPOCH`])
/// mints nothing, and m40 is handed the key that opens the removed value.
#[test]
fn a_member_unset_seals_the_removed_value_away_from_a_mac_admitted_after() {
    let fresh = unset_then_admit(false);
    assert_eq!(fresh.unset, MemberUnset::Posted);
    assert_eq!(
        convergence::epochs_by_audience(&fresh.signed).get(&EpochAudience::Everyone),
        Some(&1),
        "a fresh [everyone] epoch"
    );
    assert_eq!(puts(&fresh.signed), 1, "the entry that stays, sealed again");
    assert!(
        !fresh.holds_removed_key,
        "m40 holds no key that sealed the removed value"
    );
    assert_eq!(fresh.stays, b"everyone-v1");
    assert!(!fresh.removed_file);

    let plain = unset_then_admit(true);
    assert_eq!(plain.unset, MemberUnset::Posted, "NEGATIVE CONTROL");
    assert!(
        convergence::epochs_by_audience(&plain.signed).is_empty(),
        "NEGATIVE CONTROL: no epoch"
    );
    assert!(
        plain.holds_removed_key,
        "NEGATIVE CONTROL: under the old epoch, m40 is handed the removed value's key"
    );
    assert!(!plain.removed_file);
}

/// A Mac that does not open every `[everyone]` entry cannot seal every entry again, so its unset
/// is refused: after F21's split heals, m41 holds no key of the keyed epoch m40 minted, under which
/// m7 put `shared` = v2, and its unset of `other` signs nothing and takes nothing back. CONTROL:
/// m22, which opens every entry, removes it.
#[test]
fn a_mac_that_cannot_reseal_every_entry_is_refused() {
    let split = keyed_split(false);
    split.heal();
    let (m22, m41) = (&split.macs.m22, &split.m41);
    let identity = read_identity(m41).unwrap().unwrap();
    let opened = log_of(m41)
        .open_audience_entries(&identity)
        .unwrap()
        .unwrap();
    assert!(
        !opened.reputs_whole(&EpochAudience::Everyone),
        "CONTROL: m41 does not open every entry"
    );
    assert_eq!(
        write_route(m41, soon()).unwrap(),
        WriteRoute::Seated,
        "CONTROL"
    );
    let waiting = log_entries::waiting_values(m41, &identity);
    let before = convergence::ids(m41);
    let unset = member_unset(m41, "other", soon()).unwrap();
    assert_eq!(unset, MemberUnset::Refused(RESEAL_REFUSAL.to_owned()));
    assert_eq!(convergence::ids(m41), before, "nothing signed");
    assert_eq!(
        log_entries::waiting_values(m41, &identity),
        waiting,
        "nothing taken back"
    );
    assert_eq!(KeyedSplit::read(m41, "other"), Some(b"o1".to_vec()));

    assert_eq!(
        member_unset(m22, "other", soon()).unwrap(),
        MemberUnset::Posted,
        "CONTROL"
    );
    assert_eq!(KeyedSplit::read(m22, "other"), None);
    assert_eq!(
        KeyedSplit::read(m22, "shared"),
        Some(b"v2".to_vec()),
        "CONTROL: the reseal carries what stays"
    );
    split.macs.server.stop();
}

/// m22's first `set` of `fresh`, which the log never held, waits for a removal's rotation; then,
/// where `unset`, m22 unsets it. Then the rotation arrives and m22's passes run: what m22 reads of
/// `fresh`, and what the unset said.
fn first_set_then(unset: bool) -> (Option<Vec<u8>>, Option<MemberUnset>) {
    let lab = convergence::six();
    let chain = held_remove(&lab);
    let m22 = &lab.all[0];
    let written = member_set(m22, &entry("fresh", b"f1"), refuse()).unwrap();
    assert_eq!(written, MemberWritten::Waiting(WaitFor::RotationDue));
    let said = unset.then(|| {
        let before = convergence::ids(m22);
        let said = member_unset(m22, "fresh", soon()).unwrap();
        assert_eq!(
            convergence::ids(m22),
            before,
            "nothing to delete: nothing signed"
        );
        assert!(!log_entries::waiting_file(m22).exists(), "nothing waits");
        said
    });
    save_records(m22, &encode_records(&chain)).unwrap();
    let _ = two_passes(m22);
    let read = reads(m22, true, "fresh");
    lab.macs.server.stop();
    (read, said)
}

/// A waiting first `set` followed by an unset leaves nothing that comes back after the rotation:
/// the unset takes this Mac's own write back, so no pass signs it over the absence. CONTROL:
/// without the unset, the pass after the rotation signs it.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp):
/// `member_unset` taking nothing back (`take_back_member_write` replaced by `Ok(false)`) fails
/// "nothing waits".
#[test]
fn a_waiting_first_set_then_unset_leaves_nothing_to_come_back() {
    let (read, said) = first_set_then(true);
    assert_eq!(said, Some(MemberUnset::TookBack));
    assert_eq!(read, None, "nothing comes back after the rotation");
    let (read, _) = first_set_then(false);
    assert_eq!(read, Some(b"f1".to_vec()), "CONTROL: the pass signs it");
}

/// An unset never waits: with a removal's rotation due (m22 holds m7's `Remove` of m42 and not its
/// new key), m22's unset of a name the log holds is signed at once, under a fresh epoch that the
/// removed Mac is not wrapped to, as an admin's edit that takes a name out of `[everyone]` is.
#[test]
fn an_unset_with_a_rotation_due_signs_at_once_without_the_removed_mac() {
    let lab = convergence::six();
    let _ = held_remove(&lab);
    let m22 = &lab.all[0];
    let removed = lab.recipients[4];
    let before = convergence::ids(m22);
    assert_eq!(
        member_unset(m22, "shared", soon()).unwrap(),
        MemberUnset::Posted
    );
    let signed = convergence::signed_since(m22, &before);
    assert_eq!(
        convergence::epochs_by_audience(&signed).get(&EpochAudience::Everyone),
        Some(&1)
    );
    let wrapped: BTreeSet<Recipient> = signed
        .iter()
        .filter_map(|record| match record.body() {
            Body::Wrap { recipient, .. } => Some(*recipient),
            _ => None,
        })
        .collect();
    assert!(
        !wrapped.contains(&removed),
        "the removed Mac is not wrapped"
    );
    assert!(wrapped.contains(&lab.recipients[1]), "CONTROL: m7 is");
    assert_eq!(reads(m22, true, "shared"), None);
    lab.macs.server.stop();
}

/// Unsetting a held name (release material, the release ring, the site deployer key, fleet
/// configuration, git's config), a control record, or a case variant of release material signs
/// nothing, and the release Mac (m22, which holds every such file) keeps each file as it was.
/// CONTROL: a shared name, unset the same way, is removed there.
#[test]
fn unsetting_held_names_signs_nothing_and_the_release_macs_files_stay() {
    let held = [
        crate::entry_class::RELEASE_GH_TOKEN,
        crate::entry_class::NOTARY,
        crate::entry_class::RELEASE_RING[0],
        crate::entry_class::RELEASE_RING[2],
        crate::entry_class::SITE_DEPLOYER_KEY,
        crate::entry_class::FLEET_ACCOUNTS,
        crate::entry_class::GIT_CONFIG,
    ];
    let macs = two_macs(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    for name in held.into_iter().chain(["shared-control"]) {
        test_put_everyone(m22, name, b"release-fixture").expect("m22 puts it");
    }
    sync(m22, None).expect("m22 syncs");
    sync(m7, None).expect("m7 syncs");
    for name in held {
        assert_eq!(
            std::fs::read(file_of(m22, name)).ok().as_deref(),
            Some(b"release-fixture".as_slice()),
            "CONTROL: m22 holds {name}"
        );
    }
    for name in held.into_iter().chain([
        crate::entry_class::FLEET_CODE,
        "GH_ACCESS_TOKEN_ALABSYSTEMS",
        "Release-Notary",
        "~/.aterm/release-ring/Machine.key",
    ]) {
        let before = convergence::ids(m7);
        let unset = member_unset(m7, name, soon()).unwrap();
        assert!(
            matches!(unset, MemberUnset::Refused(_)),
            "{name}: {unset:?}"
        );
        assert_eq!(convergence::ids(m7), before, "{name} signs nothing");
    }
    assert_eq!(
        member_unset(m7, "shared-control", soon()).unwrap(),
        MemberUnset::Posted,
        "CONTROL"
    );
    sync(m22, None).expect("m22 syncs");
    for name in held {
        assert_eq!(
            std::fs::read(file_of(m22, name)).ok().as_deref(),
            Some(b"release-fixture".as_slice()),
            "{name} stays on the release Mac"
        );
    }
    assert!(!file_of(m22, "shared-control").exists(), "CONTROL: removed");
    macs.server.stop();
}

/// The vault's gh/git anchored set, put by m22, the admin: gh's hosts file fills a gap, and git's
/// config (held) and `gh-extra` (shared) are anchored to it; m7 synced after.
fn anchored_set() -> TwoMacs {
    let macs = two_macs(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let admin = admin_open(m22).expect("m22 is an admin");
    let mut doc = admin.doc.clone();
    let hosts = crate::entry_class::GH_HOSTS;
    for (name, anchor) in [
        (hosts, None),
        (crate::entry_class::GIT_CONFIG, Some(hosts)),
        ("gh-extra", Some(hosts)),
    ] {
        doc.set_entry_with(&Audience::Everyone, name, b"vault-value", true, anchor)
            .expect("the set");
    }
    admin_publish(m22, &admin, &doc).expect("m22 publishes the set");
    sync(m7, None).expect("m7 syncs");
    macs
}

/// AN UNSET NEVER TAKES AN ENTRY ANCHORED TO ITS NAME OFF EVERY MAC (§3.6, as an admin's edit
/// refuses it): every view drops an entry whose anchor is gone, so m7's unset of gh's hosts file,
/// to which git's config (held) and `gh-extra` (shared) are anchored, is refused naming both, and
/// signs nothing; m22 still writes git's config. Once only shared entries are anchored to it, the
/// refusal says to unset them first. CONTROL: with `gh-extra` and git's config gone (git's by
/// m22's edit), m7's unset of the anchor is posted.
///
/// NEGATIVE CONTROL (2026-11-06, restored with cp from a backup plus touch, confirmed with cmp):
/// with `anchored_refusal` returning `None`, m7's first unset of the anchor is `Posted`.
#[test]
fn a_member_unset_of_an_anchor_is_refused_while_an_entry_is_anchored_to_it() {
    let macs = anchored_set();
    let (m22, m7) = (&macs.m22, &macs.m7);
    let hosts = crate::entry_class::GH_HOSTS;
    let git = crate::entry_class::GIT_CONFIG;
    assert!(matches!(can_administer(m7), Ok(false)), "CONTROL: no admin");
    let before = convergence::ids(m7);
    let unset = member_unset(m7, hosts, soon()).unwrap();
    let MemberUnset::Refused(why) = &unset else {
        panic!("{unset:?}");
    };
    assert!(
        why.contains(&format!("`gh-extra`, `{git}` are anchored to `{hosts}`"))
            && why.ends_with("an admin's edit removes the whole set"),
        "{why}"
    );
    assert_eq!(convergence::ids(m7), before, "nothing signed");
    sync(m22, None).expect("m22 syncs");
    assert_eq!(
        reads(m22, true, git).as_deref(),
        Some(b"vault-value".as_slice()),
        "m22 still reads git's config"
    );

    let admin = admin_open(m22).expect("m22 is an admin");
    let mut doc = admin.doc.clone();
    assert!(doc.unset_entry(&Audience::Everyone, git).expect("unset"));
    admin_publish(m22, &admin, &doc).expect("m22 removes git's config");
    sync(m7, None).expect("m7 syncs");
    let unset = member_unset(m7, hosts, soon()).unwrap();
    assert_eq!(
        unset,
        MemberUnset::Refused(format!(
            "`gh-extra` is anchored to `{hosts}`, and every Mac drops an entry whose anchor is \
             gone, so this unset would remove it from every Mac too; unset `gh-extra` first"
        ))
    );
    assert_eq!(
        member_unset(m7, "gh-extra", soon()).unwrap(),
        MemberUnset::Posted,
        "CONTROL: the shared dependant goes first"
    );
    assert_eq!(
        member_unset(m7, hosts, soon()).unwrap(),
        MemberUnset::Posted,
        "CONTROL: then the anchor"
    );
    macs.server.stop();
}

/// A member's `--if-absent` set of `name` anchored to `anchor`.
fn anchored(name: &str, anchor: &str) -> MemberEntry {
    MemberEntry {
        if_absent: true,
        anchor: Anchor::To(anchor.to_owned()),
        fleet_reads_anchors: true,
        ..entry(name, b"anchored")
    }
}

/// A MEMBER SET NEVER NAMES AN ANCHOR NO MAC HOLDS (§3.5, as an admin's `set` refuses it): every
/// view drops an entry whose anchor is no if-absent entry, so the `Put` would be in every server's
/// log, on no Mac, and read as overtaken. m7's set anchored to a name the vault lacks, or to a
/// plain entry, is refused and signs nothing, and nothing waits. CONTROL: anchored to an if-absent
/// entry, it is posted; and an anchor written in the same draft holds.
///
/// NEGATIVE CONTROL (2026-11-06, restored with cp from a backup plus touch, confirmed with cmp):
/// with `dangling_anchor`'s refusal dropped, the set anchored to `absent` reads `Overtaken(None)`.
#[test]
fn a_member_set_anchored_to_no_if_absent_entry_is_refused() {
    let macs = two_macs(&[(Audience::Everyone, "plain", b"p1".as_slice())]);
    let m7 = &macs.m7;
    for anchor in ["absent", "plain"] {
        let before = convergence::ids(m7);
        let written = member_set(m7, &anchored("foo", anchor), refuse()).unwrap();
        assert_eq!(
            written,
            MemberWritten::Refused(format!(
                "`foo` is anchored to `{anchor}`, which is not an if-absent entry in [everyone]: \
                 set `{anchor}` --if-absent first"
            )),
            "{anchor}"
        );
        assert_eq!(convergence::ids(m7), before, "{anchor}: nothing signed");
        assert!(
            !log_entries::waiting_file(m7).exists(),
            "{anchor}: nothing waits"
        );
    }
    let gap = MemberEntry {
        if_absent: true,
        ..entry("gap", b"g1")
    };
    assert_eq!(
        member_set(m7, &gap, refuse()).unwrap(),
        MemberWritten::Posted,
        "CONTROL: the anchor"
    );
    assert_eq!(
        member_set(m7, &anchored("foo", "gap"), refuse()).unwrap(),
        MemberWritten::Posted,
        "CONTROL: anchored to it"
    );
    let pair = [
        MemberEntry {
            if_absent: true,
            ..entry("gap-2", b"g2")
        },
        anchored("foo-2", "gap-2"),
    ];
    assert_eq!(
        member_write(m7, &pair, refuse()).unwrap(),
        MemberWritten::Posted,
        "CONTROL: an anchor in the same draft"
    );
    macs.server.stop();
}
