// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lab vault that names its home LAN (`members::lab_lan`, step E1b of
//! `docs/DESIGN-alab-one-mesh-2026-11-05.md` §11), end to end in one process: the desk reads the
//! file from its own vault folder through [`crate::members::desk::Local::of`], with no seam on the
//! desk itself, and only the route this Mac's proof reader reads is scripted.

#[cfg(unix)]
use super::super::log_entries_tests::{TwoMacs, two_macs};
use super::*;
#[cfg(unix)]
use crate::client::with_live_genesis;
use crate::members::desk;
#[cfg(unix)]
use crate::members::desk::{Desk, Local};
#[cfg(unix)]
use crate::members::join::{self, Joined, Missed, Stream};
#[cfg(unix)]
use crate::members::joiner::Joining;
#[cfg(unix)]
use crate::members::lab_lan;
#[cfg(unix)]
use aterm_netplace::facts::{Script, parse_script, script_here};
#[cfg(unix)]
use ring::signature::{Ed25519KeyPair, KeyPair as _};
#[cfg(unix)]
use std::net::IpAddr;
#[cfg(unix)]
use std::sync::Mutex;

/// A fleet book with nothing against the proving Mac, every member at 0.148 or later.
#[cfg(unix)]
const CURRENT: Book = Book {
    node_id_held_out: false,
    node_id_seated: false,
    node_elsewhere: false,
    moved_seats_read: true,
};

/// The desk's default route: Wi-Fi, this Mac at 192.168.1.20 on the lab's /24.
#[cfg(unix)]
fn at_home() -> Script {
    parse_script(
        "route en0 192.168.1.20/24 via 192.168.1.1 hw aa:bb:cc:dd:ee:01\npath usable wifi\n",
    )
}

#[cfg(unix)]
fn node_pair(tag: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[tag ^ 0x5a; 32]).unwrap()
}

/// A fresh profile under `macs`'s root, pinned to the lab vault's signer at its URL and waiting
/// there, unapproved (Pending), as `commons-lan pin` leaves the fourth Mac.
#[cfg(unix)]
fn pinned(macs: &TwoMacs, name: &str) -> Paths {
    let paths = Paths::for_home(&macs.root.path().join(name));
    let joined = join(
        &paths,
        &JoinOptions {
            source: format!("{}#{}", macs.url, macs.signer),
            host: Some(name.to_owned()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .expect("the pin");
    assert!(matches!(joined, JoinOutcome::Pending { .. }), "{joined:?}");
    paths
}

/// One proof of `joiner` (node key `pair`) from `from` to the desk `local` (built on `member`),
/// the member reading its vault as on the live ALab genesis (`live`) or not, its desk asking the
/// check before possession (`checks_first`) or going straight to its decision.
#[cfg(unix)]
fn proof(
    member: (&Paths, Local),
    joiner: &Paths,
    pair: &Ed25519KeyPair,
    from: IpAddr,
    desk: (bool, bool),
) -> std::result::Result<Joined, Missed> {
    let dials = std::sync::atomic::AtomicUsize::new(0);
    proof_dialing(member, joiner, pair, from, desk, &dials)
}

/// [`proof`], counting in `dials` each connection the joiner opens to the member.
#[cfg(unix)]
fn proof_dialing(
    (member, local): (&Paths, Local),
    joiner: &Paths,
    pair: &Ed25519KeyPair,
    from: IpAddr,
    (live, checks_first): (bool, bool),
    dials: &std::sync::atomic::AtomicUsize,
) -> std::result::Result<Joined, Missed> {
    let desk = Mutex::new(Desk::new(local));
    let device = crate::commons::bootstrap::Device::open(joiner)
        .expect("device")
        .expect("an identity");
    let joining = Joining {
        records_bound: crate::crossing::RecordsBound::uncrossed(),
        device: &device,
        node: pair.public_key().as_ref().try_into().unwrap(),
        name: "m4".to_owned(),
        model: "Mac16,10".to_owned(),
    };
    let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);
    let sign_node = |message: &[u8]| -> [u8; 64] {
        pair.sign(&[prefix.as_bytes(), message].concat())
            .as_ref()
            .try_into()
            .unwrap()
    };
    let now = aterm_time::unix_millis().unwrap();
    std::thread::scope(|scope| {
        let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
            dials.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(Duration::from_secs(60)))?;
            }
            let desk = &desk;
            scope.spawn(move || {
                let decide = |proving: &desk::Proving<'_>| {
                    with_live_genesis(live, || prove_at_ms(member, proving, CURRENT, now))
                };
                let refuses = |proving: &desk::Proving<'_>| {
                    checks_first
                        .then(|| {
                            with_live_genesis(live, || {
                                refuses_before_possession_at_ms(member, proving, CURRENT, now)
                            })
                        })
                        .flatten()
                };
                let _ = desk::serve_with(
                    desk,
                    &mut ours,
                    from,
                    &|_| Err(crate::Error::new("no phrase runs here")),
                    Some(desk::Proof {
                        decide: &decide,
                        refuses: &refuses,
                        fleet: None,
                        authority: None,
                    }),
                );
            });
            Ok(Box::new(theirs))
        };
        join::prove_first(joiner, &dial, &joining, &sign_node, &["m7:7194".to_owned()])
    })
}

/// A lab vault's member whose vault folder names its home LAN lets a fresh Mac on that LAN in by
/// its proof, with nobody's Allow: the desk is `Local::of` the member's own files (the CIDR read
/// from disk, the route from this Mac's proof reader, scripted), and the Mac, pinned to the lab
/// vault's signer and waiting there unapproved, ends in it, its source the vault's URL. Before
/// the file is written, from an address outside the network it names, and on the live ALab
/// genesis (where the file is ignored and a lab identity is refused), the desk lets nobody in and
/// signs nothing.
///
/// NEGATIVE CONTROL: with `Local::of`'s `automatic_lan` read as the live vault's alone
/// (`alab_url`, the lab home dropped), the desk does not mark the Mac on the named LAN as from
/// its home: `assertion failed: local.automatically_admits(on_lan)`.
#[cfg(unix)]
#[test]
fn a_lab_vault_that_names_its_home_lan_lets_a_mac_on_it_in_by_its_proof() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"lab-v1".as_slice())]);
    let m7 = &macs.m7;
    let fresh = pinned(&macs, "fresh");
    assert_eq!(load_state(&fresh).unwrap().membership, Stored::Pending);
    let pair = node_pair(41);
    let desk_of = |live: bool| {
        script_here(at_home(), || with_live_genesis(live, || Local::of(m7)))
            .expect("m7's state reads")
            .expect("m7 is a member")
    };
    let records = || load_records(m7).unwrap().log().unwrap().records().count();
    let before = records();
    let on_lan: IpAddr = "192.168.1.45".parse().unwrap();
    let no_seat = |missed: &Missed| {
        missed
            .refused
            .iter()
            .all(|(_, why)| why.starts_with("no seat binds this node key"))
            && !missed.signed_maybe
    };

    // No file yet: the Mac on the LAN gets no seat, and nothing is signed.
    let local = desk_of(false);
    assert!(!local.automatically_admits(on_lan), "no lab home yet");
    let missed = proof((m7, local), &fresh, &pair, on_lan, (false, true))
        .expect_err("no seat with no lab home");
    assert!(no_seat(&missed), "{missed:?}");

    lab_lan::write(m7, &lab_lan::Cidr::parse("192.168.1.0/24").unwrap()).unwrap();

    // Off the network the file names: no seat.
    let local = desk_of(false);
    let outside: IpAddr = "192.168.2.45".parse().unwrap();
    assert!(!local.automatically_admits(outside));
    let missed = proof((m7, local), &fresh, &pair, outside, (false, true))
        .expect_err("no seat off the lab's network");
    assert!(no_seat(&missed), "{missed:?}");

    // On the live genesis the file is ignored: the desk's check before possession finds no home
    // LAN, and its decision refuses a lab identity.
    let local = desk_of(true);
    assert!(
        !local.automatically_admits(on_lan),
        "the live genesis ignores the file"
    );
    let missed = proof((m7, local), &fresh, &pair, on_lan, (true, true))
        .expect_err("refused on the live genesis");
    assert!(no_seat(&missed), "{missed:?}");
    let missed = proof((m7, desk_of(true)), &fresh, &pair, on_lan, (true, false))
        .expect_err("refused at the decision on the live genesis");
    assert_eq!(missed.refused[0].1, desk::LAB_REFUSED, "{missed:?}");
    assert_eq!(
        records(),
        before,
        "m7 signed nothing for a Mac it let nowhere"
    );
    assert_eq!(load_state(&fresh).unwrap().membership, Stored::Pending);

    // On the network the file names: in, with nobody's Allow.
    let local = desk_of(false);
    assert!(local.automatically_admits(on_lan));
    assert!(
        !local.automatically_admits("192.168.1.20".parse().unwrap()),
        "itself"
    );
    let joined = proof((m7, local), &fresh, &pair, on_lan, (false, true))
        .unwrap_or_else(|missed| panic!("the lab desk lets the Mac in: {missed:?}"));
    assert!(matches!(joined.admission, join::Admission::Logged));
    assert_eq!(load_state(&fresh).unwrap().membership, Stored::In);
    let configured = config(&fresh).unwrap().expect("a vault");
    assert_eq!(configured.source, Source::Url(macs.url.clone()));
    assert_eq!(
        configured.signer.map(|key| crate::b64::encode_padded(&key)),
        Some(macs.signer.clone()),
        "the pinned signer"
    );
    assert_eq!(crate::client::stored(&fresh, "shared").unwrap(), b"lab-v1");
    assert!(records() > before, "m7 signed the Mac's Admit");
    macs.server.stop();
    drop(macs.root);
}

/// A Mac waiting at one lab vault, pinned to its signer, takes no bundle another lab vault's
/// member hands it for its proof: the bundle's signer is not the one it pinned, so it stays
/// pinned where it was and nothing is written.
///
/// NEGATIVE CONTROL: with `write_log_bundle`'s refusal of a bundle that starts another vault
/// afresh removed, the Mac takes the other vault's bundle: `another vault's bundle: Joined {
/// member: "m7", name: "m4", report: Report { written: ["shared"], … }, admission: Logged }`.
#[cfg(unix)]
#[test]
fn a_mac_pinned_to_one_lab_vault_takes_no_bundle_of_another() {
    let _answering = ServersAnswering::set();
    let ours = two_macs(&[(Audience::Everyone, "shared", b"ours".as_slice())]);
    let theirs = two_macs(&[(Audience::Everyone, "shared", b"theirs".as_slice())]);
    assert_ne!(ours.signer, theirs.signer, "PRECONDITION: two vaults");
    let fresh = pinned(&ours, "fresh");
    lab_lan::write(&theirs.m7, &lab_lan::Cidr::parse("192.168.1.0/24").unwrap()).unwrap();
    let local = script_here(at_home(), || Local::of(&theirs.m7))
        .expect("their m7 reads")
        .expect("a member");
    let on_lan: IpAddr = "192.168.1.45".parse().unwrap();
    assert!(
        local.automatically_admits(on_lan),
        "PRECONDITION: a lab home"
    );
    let missed = proof(
        (&theirs.m7, local),
        &fresh,
        &node_pair(42),
        on_lan,
        (false, true),
    )
    .expect_err("another vault's bundle");
    assert!(
        missed.refused[0].1.contains("another vault's"),
        "{missed:?}"
    );
    let configured = config(&fresh).unwrap().map(|config| config.source);
    assert_eq!(
        configured,
        Some(Source::Url(ours.url.clone())),
        "stays pinned to its own vault: {configured:?}"
    );
    assert_eq!(load_state(&fresh).unwrap().membership, Stored::Pending);
    assert!(!fresh.home.join(".secrets/shared").exists());
    ours.server.stop();
    theirs.server.stop();
}

/// Why a member whose vault no new Mac can reach (one it reads from a path) hands no bundle.
const PATH_REFUSAL: &str = "this Mac reads its vault from a path, which a new Mac cannot reach";

/// The admin `m1` of a vault it founded at a path under `root`, with the vault's genesis, so its
/// log seats it: a member, but of a vault no new Mac can reach.
fn path_member(root: &std::path::Path) -> Paths {
    let admin = Paths::for_home(&root.join("m1"));
    let vault = root.join("fixture.vault");
    create(&admin, &vault.to_string_lossy(), None, Some("m1")).expect("a path vault");
    seed_vault_genesis(&admin, &root.join("fixture.genesis")).expect("its genesis");
    assert!(matches!(
        config(&admin).unwrap().unwrap().source,
        Source::Path(_)
    ));
    admin
}

/// How many records `member`'s membership log holds.
fn records_of(member: &Paths) -> usize {
    load_records(member)
        .unwrap()
        .log()
        .unwrap()
        .records()
        .count()
}

/// A member whose vault no new Mac can reach (one it reads from a path) signs nothing for a Mac's
/// proof, even one its desk marks as from the home LAN: before the possession challenge it says
/// why, so the Mac tries the next member, and its decision refuses before any `Admit` is signed,
/// ingested or written, so no Admit stays behind that handed the Mac nothing.
///
/// NEGATIVE CONTROL: with `admit_by_proof`'s read of what the bundle names moved back after
/// `write_records` (as before E1b), the refused proof leaves its `Admit` in the member's log:
/// `assertion `left == right` failed: the path member keeps no Admit for a Mac it handed nothing`
/// with `left: 9, right: 6`.
#[cfg(unix)]
#[test]
fn a_member_whose_vault_is_read_from_a_path_signs_nothing_for_a_proof() {
    let root = aterm_tempfile::tempdir().expect("temp");
    let admin = path_member(root.path());
    let local = Local::of(&admin)
        .expect("m1's state reads")
        .expect("m1 is a member")
        .with_automatic_lan([]);
    let on_lan: IpAddr = "192.168.1.40".parse().unwrap();
    assert!(
        local.automatically_admits(on_lan),
        "PRECONDITION: marked from the LAN"
    );
    let records = || records_of(&admin);
    let before = records();
    let fresh = Paths::for_home(&root.path().join("fresh"));
    machine_identity(&fresh).expect("identity");

    let missed = proof(
        (&admin, local),
        &fresh,
        &node_pair(43),
        on_lan,
        (false, true),
    )
    .expect_err("refused before possession");
    assert_eq!(missed.refused[0].1, PATH_REFUSAL, "{missed:?}");
    assert!(!missed.signed_maybe, "{missed:?}");
    let local = Local::of(&admin).unwrap().unwrap().with_automatic_lan([]);
    let missed = proof(
        (&admin, local),
        &fresh,
        &node_pair(43),
        on_lan,
        (false, false),
    )
    .expect_err("refused at the decision");
    assert_eq!(missed.refused[0].1, PATH_REFUSAL, "{missed:?}");
    assert_eq!(
        records(),
        before,
        "the path member keeps no Admit for a Mac it handed nothing"
    );
    assert_ne!(load_state(&fresh).unwrap().membership, Stored::In);
}

/// A member whose vault no new Mac can reach (one it reads from a path) signs nothing for a phrase
/// run either: after a person's Allow and the phrase, [`hand_over`] says why before its log path
/// signs an `Admit`, so the member's log keeps none for a Mac it handed nothing.
///
/// NEGATIVE CONTROL: with `hand_over_with`'s read of what the bundle names (`HandedVault::of`)
/// moved back after `log_path` (as before E1b), the refused run leaves its `Admit` in the
/// member's log: `assertion `left == right` failed: the path member keeps no Admit for a Mac it
/// handed nothing` with `left: 9, right: 6`.
#[test]
fn a_member_whose_vault_is_read_from_a_path_signs_nothing_for_a_phrase_run() {
    let root = aterm_tempfile::tempdir().expect("temp");
    let admin = path_member(root.path());
    let before = records_of(&admin);
    let (_, request, allowed) =
        super::super::log_entries_tests::newcomer(root.path(), &admin, "m40");
    let refused = hand_over(
        &admin,
        &desk::Handing {
            request: &request,
            node: request.binding().recipient().0,
            name: allowed.name(),
            allowed: &allowed,
            reader: crate::members::bundle::Reader::LegacyLog,
            restore: false,
            node_proven: false,
            held: None,
        },
    )
    .err()
    .expect("a path member hands nothing");
    assert_eq!(refused.to_string(), PATH_REFUSAL);
    assert_eq!(
        records_of(&admin),
        before,
        "the path member keeps no Admit for a Mac it handed nothing"
    );
}

/// A Mac proves itself into a format-2 vault, asking twice (tracker FL-148E2,
/// `crate::membership_log::follow`). The member, whose format mark is stored, refuses the proof
/// that states format 1 before its possession challenge, naming its format (`NO later=2`), and
/// signs nothing. A Mac whose build reads format 2 proves itself to the member once more,
/// stating format 2, is handed the member's log, and on the member's signed version-2 record
/// in it stores its own mark before its configuration and records. A Mac whose build reads no
/// format 2 proves itself once, fails with the member's reason, and holds no mark; no member
/// may have signed for it.
///
/// NEGATIVE CONTROLS, each a change to the code run against this test and seen to fail:
/// - `desk::later_refused` answering `None` (a plain `NO`, as before this fix): ``the proof
///   lets in a Mac that reads format 2``, the member's format reason, the Mac having asked
///   once.
/// - `join::prove_first_with` running one round whatever a member answered: the same failure.
/// - `client::later_records_bound` not asking [`crate::crossing::FormatMark::follows`] (every
///   Mac proves itself again): the dark Mac's proof no longer ends with the member's reason
///   and nothing signed, ``assertion failed: missed.refused[0].1.contains("format 2") &&
///   !missed.signed_maybe``.
#[cfg(unix)]
#[test]
fn vault_checkpoint_a_proving_mac_asks_again_at_format_two() {
    use crate::crossing::{FORMAT_TWO, Faults, FormatMark, inject};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"lab-v1".as_slice())]);
    let m7 = &macs.m7;
    lab_lan::write(m7, &lab_lan::Cidr::parse("192.168.1.0/24").unwrap()).unwrap();
    let dark = pinned(&macs, "dark");
    let fresh = pinned(&macs, "fresh");
    let follows = Faults {
        follows: true,
        ..Faults::default()
    };

    // The vault moves into format 2: m7 follows on a version-2 `Put` it signs, and the vault's
    // server, whose build reads format 2 too, follows on m7's post of it, so m7 reads the
    // vault's newest records from it as a member that lets a Mac in must.
    let server_mark = FormatMark::at(
        macs.root
            .path()
            .join("s1")
            .join(format!("{}.format", crate::ALAB_VAULT)),
    );
    inject(server_mark.path(), follows);
    let two = super::super::follow_tests::follows_on_its_own_put(m7);
    post_records_to_servers(m7).expect("m7's records reach the vault's server");
    assert_eq!(server_mark.stored().unwrap(), FORMAT_TWO);

    let local = || {
        script_here(at_home(), || Local::of(m7))
            .expect("m7's state reads")
            .expect("m7 is a member")
    };
    let records = || load_records(m7).unwrap().log().unwrap().records().count();
    let before = records();
    let on_lan: IpAddr = "192.168.1.45".parse().unwrap();
    let dials = AtomicUsize::new(0);

    // A build that reads no format 2: one proof, the member's reason, nothing signed.
    let missed = proof_dialing(
        (m7, local()),
        &dark,
        &node_pair(41),
        on_lan,
        (false, true),
        &dials,
    )
    .expect_err("format 1 and format 2 exchange no log");
    assert!(
        missed.refused[0].1.contains("format 2") && !missed.signed_maybe,
        "{missed:?}"
    );
    assert_eq!(
        dials.swap(0, Ordering::SeqCst),
        1,
        "a build that reads no format 2 proves itself once"
    );
    assert_eq!(records(), before, "m7 signed nothing for it");
    assert!(!format_mark(&dark).path().exists());
    assert_eq!(load_state(&dark).unwrap().membership, Stored::Pending);

    inject(format_mark(&fresh).path(), follows);
    let joined = proof_dialing(
        (m7, local()),
        &fresh,
        &node_pair(42),
        on_lan,
        (false, true),
        &dials,
    )
    .unwrap_or_else(|missed| panic!("the proof lets in a Mac that reads format 2: {missed:?}"));
    assert!(matches!(joined.admission, join::Admission::Logged));
    assert_eq!(
        dials.load(Ordering::SeqCst),
        2,
        "refused at format 1, then proved again stating format 2"
    );
    assert_eq!(format_mark(&fresh).stored().unwrap(), FORMAT_TWO);
    assert_eq!(load_state(&fresh).unwrap().membership, Stored::In);
    assert!(load_records(&fresh).unwrap().holds(two.id()));
    assert_eq!(
        crate::client::stored(&fresh, "format-two-value").unwrap(),
        b"synthetic-two"
    );
    assert!(records() > before, "m7 signed the Mac's Admit");
    for mark in [
        format_mark(m7),
        format_mark(&dark),
        format_mark(&fresh),
        server_mark,
    ] {
        inject(mark.path(), Faults::default());
    }
    macs.server.stop();
    drop(macs.root);
}
