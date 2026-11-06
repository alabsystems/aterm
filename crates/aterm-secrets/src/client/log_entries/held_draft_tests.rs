// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::Request;
use crate::crossing::fixture::LEGACY;
use crate::membership_log::RecordBudgets;

fn member(tag: u8) -> ([u8; 32], [u8; 16], MemberId, Entrant) {
    let seed = [tag; 32];
    let incarnation = [tag; 16];
    let identity = Identity::from_secret_bytes([tag + 64; 32]);
    let request = Request::create(
        [17; 16],
        [34; 32],
        [tag + 32; 16],
        incarnation,
        &identity,
        &seed,
    )
    .unwrap();
    let actor = request.binding().actor();
    (
        seed,
        incarnation,
        actor,
        Entrant {
            request,
            node_key: [tag + 128; 32],
            name: format!("m{tag}"),
            label: format!("m{tag}"),
        },
    )
}

fn name(log: &mut Log, actor: &MemberId, seed: &[u8; 32], incarnation: [u8; 16]) -> Record {
    let record = log
        .draft(
            actor,
            2_000,
            Body::Name {
                name: "test-member".into(),
            },
        )
        .sign(seed, incarnation)
        .unwrap();
    log.insert(record.clone()).unwrap();
    record
}

/// The envelope digest the held draft below is held under.
const VERSION: [u8; 32] = [9; 32];

#[test]
fn accepted_publication_keeps_its_whole_draft_or_preserves_the_recovery_file() {
    LEGACY.within(accepted_publication_at_a_full_held_store);
}

fn accepted_publication_at_a_full_held_store() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (seed, incarnation, actor, founder) = member(1);
    let (peer_seed, peer_incarnation, peer, peer_founder) = member(2);
    let mut base =
        Log::new(Record::found(&[7; 32], vec![founder, peer_founder], 1_000).unwrap()).unwrap();
    while base.held_count() < LEGACY.held - 2 {
        name(&mut base, &actor, &seed, incarnation);
    }
    let mut after_draft = base.clone();
    let drafted = Drafted {
        records: vec![
            name(&mut after_draft, &actor, &seed, incarnation),
            name(&mut after_draft, &actor, &seed, incarnation),
        ],
        heads: after_draft.heads(),
    };
    let incoming = name(&mut base, &peer, &peer_seed, peer_incarnation);
    assert_eq!(base.held_count(), LEGACY.held - 1);

    // An ordinary publish has already been accepted. Concurrent peer ingress
    // consumed one of the two slots its held draft needs before local retention.
    let mut outcomes = Vec::new();
    for bounded in [false, true] {
        let paths =
            Paths::for_home(
                &root
                    .path()
                    .join(if bounded { "bounded" } else { "ordinary" }),
            );
        super::super::save_records(&paths, &encode_records(base.records())).unwrap();
        hold_drafted(&paths, &drafted, 2, &VERSION, &[]).unwrap();
        let before = std::fs::read(paths.records_file()).unwrap();
        let held = std::fs::read(held_draft_file(&paths, 2, &VERSION)).unwrap();
        let deadline = bounded.then(|| Instant::now() + aterm_spec::wait::MUST_HAPPEN);
        let result = keep_drafted_until(&paths, &drafted, (2, &VERSION), &[], deadline, None);
        if let Ok(&kept) = result.as_ref() {
            // This is the publication caller's success path: it may discard
            // recovery only after every record was made durably complete.
            forget_landed_draft(&paths, 2, &VERSION, &[], &[], true, kept, deadline);
        }
        let store = super::super::load_records(&paths).unwrap();
        let retained = drafted
            .records
            .iter()
            .filter(|record| store.holds(record.id()))
            .count();
        assert!(store.holds(incoming.id()));
        outcomes.push((
            bounded,
            result.is_err(),
            std::fs::read(paths.records_file()).unwrap() == before,
            std::fs::read(held_draft_file(&paths, 2, &VERSION)).ok() == Some(held),
            retained,
        ));
    }
    assert_eq!(
        outcomes,
        [(false, true, true, true, 0), (true, true, true, true, 0)],
        "retention must refuse an incomplete draft without changing the store or discarding recovery"
    );
}

/// The version `url` serves: its sequence and envelope digest.
fn served(url: &str) -> (u64, [u8; 32]) {
    let Fetched::Body(bytes) = transport::fetch(
        &Source::Url(url.to_owned()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap() else {
        panic!("expected a served envelope on {url}");
    };
    let parsed = envelope::parse(&bytes).unwrap();
    (parsed.seq, parsed.digest())
}

/// The versions `paths` holds drafts of ([`held_drafts`]), oldest first.
fn held(paths: &Paths) -> Vec<(u64, [u8; 32])> {
    held_drafts(paths)
        .into_iter()
        .map(|(seq, version, _)| (seq, version))
        .collect()
}

/// Whether `paths`' log holds every record the stamp of `url`'s version names, as `paths`' own
/// identity reads it.
fn log_covers_served(paths: &Paths, url: &str) -> bool {
    let Fetched::Body(bytes) = transport::fetch(
        &Source::Url(url.to_owned()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap() else {
        panic!("expected a served envelope on {url}");
    };
    let envelope = envelope::parse(&bytes).unwrap();
    let identity = super::super::read_identity(paths).unwrap().unwrap();
    let Stamp::Heads(stamp) = envelope_stamp(&envelope, &identity).unwrap() else {
        panic!("the served version is stamped");
    };
    let store = super::super::load_records(paths).unwrap();
    stamp.iter().all(|id| store.contains(id))
}

/// What became of P1's rotation of `gh-token` ([`refused_retry_after_a_lost_reply`]).
struct AfterTheRetry {
    /// What m7 reads at `gh-token` once every Mac synced.
    m7_reads: Vec<u8>,
    /// Whether m22's log holds every record the head's stamp names.
    m22_covers: bool,
    /// Whether m7's log holds every record the head's stamp names.
    m7_covers: bool,
}

/// One-mesh W2's sequence on `two_macs`. P1 rotates `gh-token` to `token-v2`; the server stores
/// it, but its reply is lost, so P1 keeps nothing. P2, drafted from the version P1 was drafted from
/// (`token-v3`), is refused as not newer. Nobody acts: m22 syncs twice, then m7. `single_slot`
/// sets [`SINGLE_SLOT_HOLD`] for both publishes and every pass.
fn refused_retry_after_a_lost_reply(single_slot: bool) -> AfterTheRetry {
    let macs = super::super::log_entries_tests::two_macs(&[(
        Audience::Everyone,
        "gh-token",
        b"token-v1".as_slice(),
    )]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    {
        let admin = admin_open(m22).unwrap();
        admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    sync(m7, None).expect("m7 applies the stamped version");
    assert!(
        load_state(m7).unwrap().records_digest.is_some(),
        "m7 reads the log"
    );
    SINGLE_SLOT_HOLD.with(|cell| cell.set(single_slot));

    let opened = admin_open(m22).unwrap();
    let mut first = opened.doc.clone();
    first
        .set_entry(&Audience::Everyone, "gh-token", b"token-v2")
        .unwrap();
    CUT_AFTER_STORE.with(|cut| cut.set(true));
    assert!(
        admin_publish(m22, &opened, &first).is_err(),
        "P1's reply is lost"
    );
    let p1 = served(&macs.url);
    assert_eq!(p1.0, opened.seq + 1, "the server stored P1");

    let mut second = opened.doc.clone();
    second
        .set_entry(&Audience::Everyone, "gh-token", b"token-v3")
        .unwrap();
    let refused = admin_publish(m22, &opened, &second).expect_err("P2 is not newer than P1");
    assert_eq!(refused.kind(), crate::ErrorKind::NotStored, "{refused}");
    assert_eq!(served(&macs.url), p1, "the server still holds P1");

    for _ in 0..2 {
        let _ = sync(m22, None);
    }
    let _ = sync(m7, None);
    SINGLE_SLOT_HOLD.with(|cell| cell.set(false));
    let after = AfterTheRetry {
        m7_reads: std::fs::read(m7.home.join(".secrets/gh-token")).unwrap(),
        m22_covers: log_covers_served(m22, &macs.url),
        m7_covers: log_covers_served(m7, &macs.url),
    };
    macs.server.stop();
    after
}

/// One-mesh W2: a publish whose reply was lost, then a second publish from the same opened version
/// that the server refuses as not newer. The first is the vault's head, stamped with records only
/// its hold has. The refusal forgets the second's hold alone, m22's next pass takes the first's in
/// by its version and sends it, and every Mac's log reads the head: m7 reads `token-v2`.
///
/// NEGATIVE CONTROL: [`SINGLE_SLOT_HOLD`] (one hold for the Mac, as before), run in the test as
/// the CONTROL: P2's hold replaces P1's and P2's refusal forgets it, so the head's records exist
/// nowhere, neither log covers it, and m7 keeps `token-v1`.
#[test]
fn a_refused_publish_after_one_whose_reply_was_lost_keeps_the_first_edit() {
    let after = refused_retry_after_a_lost_reply(false);
    assert!(after.m22_covers, "m22's log takes in P1's records");
    assert!(after.m7_covers, "m7's log reads the head");
    assert_eq!(after.m7_reads, b"token-v2", "m7 reads P1's edit");

    let control = refused_retry_after_a_lost_reply(true);
    assert!(
        !control.m22_covers && !control.m7_covers,
        "CONTROL: with one hold, the head's records exist nowhere"
    );
    assert_eq!(
        control.m7_reads, b"token-v1",
        "CONTROL: with one hold, m7 never reads P1's edit"
    );
}

/// Three publishes from one opened version, each settled by its own answer. P1 (`token-v2`) is
/// stored and its reply lost; P2 (`token-v3`) ends between its hold and its PUT, so no server
/// has it; P3 (`token-v4`) is refused as not newer. The refusal forgets P3's hold alone. m22's
/// next pass reads P1 back as the head and takes in P1's hold, found by P1's digest, never P2's,
/// whose edit no server holds: m22 reads `token-v2` with no conflict. P2's hold stays until the
/// one server its publish may have reached takes a version of its sequence or above from m22,
/// which the next publish does; then no hold is left.
#[test]
fn only_the_refused_version_is_forgotten_and_the_stored_one_is_adopted_by_its_digest() {
    let macs = super::super::log_entries_tests::two_macs(&[(
        Audience::Everyone,
        "gh-token",
        b"token-v1".as_slice(),
    )]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    {
        let admin = admin_open(m22).unwrap();
        admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    sync(m7, None).expect("m7 applies the stamped version");
    let opened = admin_open(m22).unwrap();
    let edit = |value: &[u8]| {
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "gh-token", value)
            .unwrap();
        doc
    };

    CUT_AFTER_STORE.with(|cut| cut.set(true));
    assert!(admin_publish(m22, &opened, &edit(b"token-v2")).is_err());
    let p1 = served(&macs.url);
    assert_eq!(held(m22), [p1], "P1 is held under its own version");

    CUT_BEFORE_STORE.with(|cut| cut.set(true));
    assert!(admin_publish(m22, &opened, &edit(b"token-v3")).is_err());
    assert_eq!(served(&macs.url), p1, "no server took P2");
    let holds = held(m22);
    assert_eq!(holds.len(), 2, "P1 and P2 are each held: {holds:?}");
    let p2 = *holds.iter().find(|held| **held != p1).unwrap();
    assert_eq!(p2.0, p1.0, "P2 is drafted from the version P1 was");

    let refused = admin_publish(m22, &opened, &edit(b"token-v4")).expect_err("not newer");
    assert_eq!(refused.kind(), crate::ErrorKind::NotStored, "{refused}");
    let mut both = vec![p1, p2];
    both.sort();
    assert_eq!(held(m22), both, "the refusal forgets P3's hold alone");

    sync(m22, None).expect("m22 reads P1 back");
    assert!(log_covers_served(m22, &macs.url), "P1's hold is taken in");
    assert_eq!(held(m22), [p2], "P1's hold goes; P2's stays");
    assert_eq!(
        std::fs::read(m22.home.join(".secrets/gh-token")).unwrap(),
        b"token-v2"
    );
    assert!(
        conflicts(m22).unwrap().is_empty(),
        "P2's edit, which no server holds, never enters the log"
    );

    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "other", b"x").unwrap();
    admin_publish(m22, &admin, &doc).expect("a head above P1's sequence");
    assert_eq!(served(&macs.url).0, p1.0 + 1);
    assert!(
        held(m22).is_empty(),
        "the one server P2 may have reached took a newer version: P2's hold goes"
    );

    sync(m7, None).expect("m7 syncs");
    assert_eq!(
        std::fs::read(m7.home.join(".secrets/gh-token")).unwrap(),
        b"token-v2"
    );
    assert!(log_covers_served(m7, &macs.url));
    macs.server.stop();
}

/// The Path branch of a publish, on a vault file with a seeded log. P1 (`v2`) ends between its
/// hold and its write, so the file never holds it. P2 (`v3`), from the same opened version, is
/// written: its own hold goes, and so does P1's, since the one server P1 could have reached, the
/// file, now holds a version of P1's sequence and refuses P1 for good. A hold left by a publish
/// that ended before its write, a sequence above the file's, stays through P3 (`v4`), which is
/// refused as not newer and forgets its own hold alone. The next head, of that crashed hold's
/// sequence, settles it too, and P1's edit never entered the log.
#[test]
fn a_path_vault_settles_each_held_version_by_its_own_answer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m22 = Paths::for_home(&root.path().join("m22"));
    let vault = root.path().join("alab.vault");
    let created = test_create_unbound(&m22, vault.to_str().unwrap(), None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).unwrap());
    admin_commons_init(&m22).expect("commons init");
    let lineage = decode_key32(&created.signer).unwrap();
    genesis::sign_genesis(&m22, &root.path().join("alab.genesis"), &lineage, true)
        .expect("sign genesis");
    {
        let admin = admin_open(&m22).unwrap();
        admin_publish(&m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    let on_file = || {
        let bytes = fsx::read_owned(&vault, envelope::MAX_ENVELOPE)
            .unwrap()
            .unwrap();
        let parsed = envelope::parse(&bytes).unwrap();
        (parsed.seq, parsed.digest())
    };
    let opened = admin_open(&m22).unwrap();
    let edit = |value: &[u8]| {
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", value).unwrap();
        doc
    };
    let before = on_file();

    CUT_BEFORE_STORE.with(|cut| cut.set(true));
    assert!(admin_publish(&m22, &opened, &edit(b"v2")).is_err());
    assert_eq!(on_file(), before, "the file never holds P1");
    let p1 = held_drafts(&m22);
    assert_eq!(p1.len(), 1, "P1 is held");
    assert_eq!(p1[0].0, before.0 + 1);
    let reach = read_held(&p1[0].2).unwrap().unwrap().reach;
    assert_eq!(
        reach.len(),
        1,
        "P1's publish may reach the file alone: {reach:?}"
    );

    admin_publish(&m22, &opened, &edit(b"v3")).expect("P2 is written");
    assert_eq!(on_file().0, before.0 + 1);
    assert!(
        held(&m22).is_empty(),
        "P2's hold goes, and P1's: the file holds a version of P1's sequence"
    );

    let crashed = (before.0 + 2, [7; 32]);
    let nothing = Drafted {
        records: Vec::new(),
        heads: Vec::new(),
    };
    hold_drafted(&m22, &nothing, crashed.0, &crashed.1, &reach).unwrap();
    let refused = admin_publish(&m22, &opened, &edit(b"v4")).expect_err("not newer");
    assert_eq!(refused.kind(), crate::ErrorKind::NotStored, "{refused}");
    assert_eq!(held(&m22), [crashed], "the refusal forgets P3's hold alone");

    let admin = admin_open(&m22).unwrap();
    assert_eq!(
        admin
            .published_entry(&Audience::Everyone, "shared")
            .unwrap()
            .value
            .as_slice(),
        b"v3"
    );
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "other", b"x").unwrap();
    admin_publish(&m22, &admin, &doc).expect("a head of the crashed hold's sequence");
    assert_eq!(on_file().0, crashed.0);
    assert!(
        held(&m22).is_empty(),
        "the file took a version of the crashed hold's sequence"
    );
    assert!(
        conflicts(&m22).unwrap().is_empty(),
        "P1's edit never entered the log"
    );
}

/// What [`settle_held_drafts_locked`] leaves of six holds as four landed heads settle them, each
/// held under its sequence, digest and reach. A landed head of sequence 6 that names none of them
/// and that server B alone took narrows v2's reach to A and removes nothing: a hold goes by
/// sequence alone nowhere. One that names v0 in its parent spine and that A and the ALab name took
/// removes v0 by descent and v1 and v2, whose every server took it; va keeps the ALab name, which
/// moves between servers, so that its answer speaks for no earlier publish; vx, of a higher
/// sequence, and vu, whose reach is unknown, stay. A head of sequence 8 that names vu and that
/// both took settles vu and vx, and va only goes with a head that names it.
///
/// NEGATIVE CONTROLS, run by hand: settling by sequence alone (a hold of the head's sequence or
/// below goes whatever its reach) fails the first step, which removes v0, v1, v2, va and vu;
/// taking the ALab name's answer as one server's fails the second, which removes va.
#[test]
fn a_hold_goes_only_by_descent_or_the_answer_of_every_server_it_may_have_reached() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let nothing = Drafted {
        records: Vec::new(),
        heads: Vec::new(),
    };
    let (a, b, c) = (
        "https://a.example/v".to_owned(),
        "https://b.example/v".to_owned(),
        "https://c.example/v".to_owned(),
    );
    let alab = crate::ALAB_DEFAULT_URL.to_owned();
    let (v0, v1, v2, va, vu, vx) = (
        (4, [3; 32]),
        (5, [1; 32]),
        (5, [2; 32]),
        (5, [11; 32]),
        (5, [5; 32]),
        (7, [4; 32]),
    );
    for ((seq, version), reach) in [
        (v0, vec![c.clone()]),
        (v1, vec![a.clone()]),
        (v2, vec![a.clone(), b.clone()]),
        (va, vec![alab.clone(), a.clone()]),
        (vu, vec![REACH_UNKNOWN.to_owned()]),
        (vx, vec![a.clone()]),
    ] {
        hold_drafted(&paths, &nothing, seq, &version, &reach).unwrap();
    }
    let sorted = |mut versions: Vec<(u64, [u8; 32])>| {
        versions.sort();
        versions
    };
    let all = sorted(vec![v0, v1, v2, va, vu, vx]);
    assert_eq!(held(&paths), all);
    let reach_of = |(seq, version): (u64, [u8; 32])| {
        read_held(&held_draft_file(&paths, seq, &version))
            .unwrap()
            .unwrap()
            .reach
    };
    let land = |seq: u64, version: u8, parents: &[[u8; 32]], took: &[String]| {
        forget_landed_draft(&paths, seq, &[version; 32], parents, took, true, true, None);
    };

    land(6, 6, &[[6; 32]], std::slice::from_ref(&b));
    assert_eq!(held(&paths), all, "no hold goes by sequence alone");
    assert_eq!(
        reach_of(v2),
        std::slice::from_ref(&a),
        "B took a newer head: v2 waits on A alone"
    );
    assert_eq!(reach_of(v1), std::slice::from_ref(&a));

    land(6, 8, &[[8; 32], v0.1], &[a.clone(), alab.clone()]);
    assert_eq!(
        held(&paths),
        sorted(vec![va, vu, vx]),
        "v0 goes by descent, v1 and v2 by every server's answer"
    );
    assert_eq!(
        reach_of(va),
        std::slice::from_ref(&alab),
        "A took a newer head; the ALab name's answer speaks for no earlier publish"
    );

    land(8, 9, &[vu.1], &[a.clone(), alab.clone()]);
    assert_eq!(held(&paths), [va], "vu goes by descent, vx by A's answer");
    land(9, 10, &[va.1], &[]);
    assert!(held(&paths).is_empty(), "va goes by descent");
}

/// A hold is removed only under the state lock: where a bounded publish cannot take it by its
/// deadline, the refused version's hold and the landed version's both stay, and so does every
/// hold the landed one would settle, for the next settled head.
///
/// NEGATIVE CONTROL, run by hand: forgetting the refused version's hold whether or not the lock
/// was taken fails the first assertion.
#[test]
fn a_hold_is_removed_only_under_the_state_lock() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let nothing = Drafted {
        records: Vec::new(),
        heads: Vec::new(),
    };
    let a = vec!["https://a.example/v".to_owned()];
    let (refused, landed, older) = ((5, [1; 32]), (5, [2; 32]), (4, [3; 32]));
    for (seq, version) in [refused, landed, older] {
        hold_drafted(&paths, &nothing, seq, &version, &a).unwrap();
    }
    let all = held(&paths);
    let passed = Some(Instant::now());
    forget_held_draft(&paths, refused.0, &refused.1, passed);
    forget_landed_draft(
        &paths,
        landed.0,
        &landed.1,
        &[older.1],
        &a,
        true,
        true,
        passed,
    );
    assert_eq!(held(&paths), all, "no hold goes without the lock");

    forget_landed_draft(
        &paths,
        landed.0,
        &landed.1,
        &[older.1],
        &a,
        true,
        true,
        None,
    );
    assert!(
        held(&paths).is_empty(),
        "the next settled head removes them: its own, by descent and by A's answer"
    );
}

/// A landed version whose records were not kept settles the other holds by its servers alone,
/// never by its parent spine ([`nle::spine_settles`], the island's `landSpine`): its hold went by
/// descent before its keep, so the head that settled it carries its edit, and that head's spine,
/// capped at [`MAX_PARENTS`](crate::envelope::MAX_PARENTS), may leave out a version this one's
/// names. Here v2, drafted from v1, lands on B with its records not kept: v1's hold, which only
/// A may hold, stays. It goes by descent where v2 kept its records or drafted none. The island's
/// run `cappedSpine` (section 7) reaches this landing through v3, drafted from v2 with v1 left
/// out of its spine.
///
/// NEGATIVE CONTROL, run in the test: [`SPINE_ALWAYS`] (`spine-always`, a landed version settles
/// by its spine whatever became of its records, as before the gate) drops v1's hold.
#[test]
fn a_landed_version_whose_records_were_not_kept_settles_by_reach_alone() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let nothing = Drafted {
        records: Vec::new(),
        heads: Vec::new(),
    };
    let (a, b) = (
        vec!["https://a.example/v".to_owned()],
        vec!["https://b.example/v".to_owned()],
    );
    let (v1, v2) = ((2, [1; 32]), (3, [2; 32]));
    // Whether v1's hold stays once v2 lands on B, v1 its spine's one version.
    let stays = |drafted: bool, kept: bool| {
        hold_drafted(&paths, &nothing, v1.0, &v1.1, &a).unwrap();
        forget_landed_draft(&paths, v2.0, &v2.1, &[v1.1], &b, drafted, kept, None);
        held(&paths).contains(&v1)
    };
    assert!(
        stays(true, false),
        "v2's records were not kept: v1's hold waits on A"
    );
    assert!(
        !stays(true, true),
        "v2 kept its records: v1's hold goes by descent"
    );
    assert!(
        !stays(false, false),
        "v2 drafted none: v1's hold goes by descent"
    );

    SPINE_ALWAYS.with(|cell| cell.set(true));
    let stayed = stays(true, false);
    SPINE_ALWAYS.with(|cell| cell.set(false));
    assert!(
        !stayed,
        "CONTROL: settled by the spine of a version whose records were not kept, v1's hold goes"
    );
}

/// The one hold a build before per-version holds kept (`records.drafted`, records alone): P1's
/// records, stored by the server with its reply lost, sit there instead of in a versioned hold.
/// m22's next pass reads P1 back as the head and takes them in, then removes the file; once the
/// pass after sends them, m7 reads P1's edit.
#[test]
fn the_unversioned_hold_of_an_earlier_build_is_adopted_by_the_head_it_completes() {
    let macs = super::super::log_entries_tests::two_macs(&[(
        Audience::Everyone,
        "gh-token",
        b"token-v1".as_slice(),
    )]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    {
        let admin = admin_open(m22).unwrap();
        admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    sync(m7, None).expect("m7 applies the stamped version");
    let opened = admin_open(m22).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "gh-token", b"token-v2")
        .unwrap();
    CUT_AFTER_STORE.with(|cut| cut.set(true));
    assert!(
        admin_publish(m22, &opened, &doc).is_err(),
        "P1's reply is lost"
    );
    let [(_, _, path)] = &held_drafts(m22)[..] else {
        panic!("P1 is held under its version");
    };
    let held_p1 = read_held(path).unwrap().unwrap();
    let unversioned = unversioned_held_draft_file(m22);
    super::super::write_record_file(&unversioned, |writer| writer.write_all(held_p1.records()))
        .unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(
        !log_covers_served(m22, &macs.url),
        "only the old hold has P1's records"
    );

    sync(m22, None).expect("m22 reads P1 back");
    assert!(
        log_covers_served(m22, &macs.url),
        "the old hold is taken in"
    );
    assert!(!unversioned.exists(), "and removed");
    // m22's next pass sends them, as after any adopted hold.
    let _ = sync(m22, None);
    sync(m7, None).expect("m7 syncs");
    assert_eq!(
        std::fs::read(m7.home.join(".secrets/gh-token")).unwrap(),
        b"token-v2"
    );
    assert!(log_covers_served(m7, &macs.url));
    macs.server.stop();
}

/// No answer says whether the unnamed version of the old hold was stored, so a landed head
/// removes that hold only once every record it holds is in the records store. Records of a
/// publish no server took stay held through a landed head; the store's own records go with the
/// next.
#[test]
fn the_unversioned_hold_goes_only_once_the_store_holds_every_record_in_it() {
    let macs = super::super::log_entries_tests::two_macs(&[(
        Audience::Everyone,
        "gh-token",
        b"token-v1".as_slice(),
    )]);
    let m22 = &macs.m22;
    {
        let admin = admin_open(m22).unwrap();
        admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    let publish = |name: &str| {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, name, b"x").unwrap();
        admin_publish(m22, &admin, &doc)
    };
    CUT_BEFORE_STORE.with(|cut| cut.set(true));
    assert!(publish("never-stored").is_err());
    let [(_, _, path)] = &held_drafts(m22)[..] else {
        panic!("the cut publish is held under its version");
    };
    let never_stored = read_held(path).unwrap().unwrap();
    let unversioned = unversioned_held_draft_file(m22);
    let write = |records: &[u8]| {
        super::super::write_record_file(&unversioned, |writer| writer.write_all(records)).unwrap();
    };
    write(never_stored.records());
    std::fs::remove_file(path).unwrap();

    publish("landed").expect("a landed head");
    assert!(
        unversioned.exists(),
        "records no server may hold stay through a landed head"
    );

    let store = super::super::load_records(m22).unwrap();
    write(&store.encode_held());
    publish("again").expect("another landed head");
    assert!(
        !unversioned.exists(),
        "the store holds every record in it: the old hold goes"
    );
    assert!(held(m22).is_empty());
    macs.server.stop();
}

/// The unversioned hold's law (P3 section 13): every record it was written with is in it or in the
/// records store, so it goes, at an adoption as at a settled head, only once the store holds every
/// record in it ([`unversioned_goes`]). Here it holds P1's records and a record the store refuses
/// (another network's genesis): m22 reads P1 back and takes the hold in, since it completes P1's
/// stamp, and keeps the file for the refused record, through the next landed head too. With
/// [`UNVERSIONED_GOES_EARLY`] on, a landed head removes it once it reads, and the law breaks.
#[test]
fn the_unversioned_hold_stays_until_the_store_holds_every_record_and_an_early_removal_is_caught() {
    let macs = super::super::log_entries_tests::two_macs(&[(
        Audience::Everyone,
        "gh-token",
        b"token-v1".as_slice(),
    )]);
    let m22 = &macs.m22;
    {
        let admin = admin_open(m22).unwrap();
        admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    let publish = |name: &str| {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, name, b"x").unwrap();
        admin_publish(m22, &admin, &doc)
    };
    CUT_AFTER_STORE.with(|cut| cut.set(true));
    assert!(publish("p1").is_err(), "P1's reply is lost");
    let [(_, _, path)] = &held_drafts(m22)[..] else {
        panic!("P1 is held under its version");
    };
    let mut written =
        crate::membership_log::decode_records(read_held(path).unwrap().unwrap().records()).unwrap();
    let (_, _, _, founder) = member(1);
    written.push(Record::found(&[7; 32], vec![founder], 1_000).unwrap());
    let unversioned = unversioned_held_draft_file(m22);
    super::super::write_record_file(&unversioned, |writer| {
        writer.write_all(&encode_records(&written))
    })
    .unwrap();
    std::fs::remove_file(path).unwrap();
    let kept = || {
        let store = super::super::load_records(m22).unwrap();
        unversioned.exists() || written.iter().all(|record| store.holds(record.id()))
    };

    sync(m22, None).expect("m22 reads P1 back");
    assert!(
        log_covers_served(m22, &macs.url),
        "the old hold completes P1's stamp and is taken in"
    );
    assert!(
        unversioned.exists() && kept(),
        "the store refused one of its records: it stays"
    );
    publish("landed").expect("a landed head");
    assert!(unversioned.exists() && kept(), "a landed head keeps it too");

    UNVERSIONED_GOES_EARLY.with(|early| early.set(true));
    let landed = publish("early");
    UNVERSIONED_GOES_EARLY.with(|early| early.set(false));
    landed.expect("another landed head");
    assert!(
        !unversioned.exists() && !kept(),
        "removed before the store holds its records, the refused record is lost, and the law sees it"
    );
    macs.server.stop();
}

/// P3 section 13's premise is a complete record's: one the records store holds only waiting on a
/// parent it lacks is not stored for the unversioned hold ([`unversioned_goes`] asks
/// [`Store::holds`]), since the store drops waiting records, highest first, to make room for
/// complete ones. Here the hold's one record waits on a parent the store never had: the hold
/// stays, and once the store's held total fills with complete records it drops the waiting one,
/// so the hold is that record's one copy.
///
/// NEGATIVE CONTROL, run in the test: [`WAITING_COUNTS_STORED`] (`waiting-counts-stored`,
/// [`Store::contains`] in place of [`Store::holds`], the rule before) lets the hold go while its
/// record only waits; the store then drops that record, which is in neither.
#[test]
fn a_record_the_store_holds_only_waiting_keeps_the_unversioned_hold() {
    let budgets = RecordBudgets {
        held: 4,
        ..RecordBudgets::PRODUCTION
    };
    budgets.within(|| {
        let (seed, incarnation, actor, founder) = member(1);
        let (other_seed, other_incarnation, other, other_founder) = member(2);
        let genesis = Record::found(&[7; 32], vec![founder, other_founder], 1_000).unwrap();
        let mut drafted = Log::new(genesis.clone()).unwrap();
        let parent = name(&mut drafted, &actor, &seed, incarnation);
        let waiting = name(&mut drafted, &actor, &seed, incarnation);
        assert!(waiting.parents().contains(parent.id()));
        let mut store = Store::with_compiled(None);
        store.insert(genesis.clone()).unwrap();
        store.insert(waiting.clone()).unwrap();
        assert!(
            store.contains(waiting.id()) && !store.holds(waiting.id()),
            "it waits on its parent"
        );
        let hold = encode_records([&waiting]);
        assert!(
            !unversioned_goes(&hold, Some(&store)),
            "a record the store holds waiting keeps the hold"
        );
        WAITING_COUNTS_STORED.with(|cell| cell.set(true));
        let goes_early = unversioned_goes(&hold, Some(&store));
        WAITING_COUNTS_STORED.with(|cell| cell.set(false));
        assert!(
            goes_early,
            "CONTROL: a waiting record counted as stored lets the hold go"
        );

        let mut others = Log::new(genesis).unwrap();
        for _ in 0..budgets.held {
            if !store.contains(waiting.id()) {
                break;
            }
            store
                .insert(name(&mut others, &other, &other_seed, other_incarnation))
                .unwrap();
        }
        assert!(
            !store.contains(waiting.id()),
            "the store dropped the waiting record for complete ones"
        );
        assert!(
            !unversioned_goes(&hold, Some(&store)),
            "the hold is its one copy"
        );
    });
}
