// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One-mesh S1: a live seatless recipient takes its seat by itself, with the same keys.

use super::super::log_entries_tests::{TwoMacs, join_host, member_key, two_macs};
use super::super::readmit::{Book, prove_at_ms};

use super::*;
use crate::members::desk::{self, Desk, Local};
use crate::members::join::{self, Stream};
use crate::members::joiner::Joining;
use crate::membership_log::{EpochAudience, Kind, MemberId, Record};
use crate::text::Audience;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A fleet book in which every member reads moved seats.
const CURRENT: Book = Book {
    node_id_held_out: false,
    node_id_seated: false,
    node_elsewhere: false,
    moved_seats_read: true,
};

/// The lab of [`two_macs`] (m22 and m7 seated) and `s`, a Mac m22's approver let in through the
/// envelope alone after the genesis, as m22 let in 28 of ALab's recipients: the newest
/// `[everyone]` epoch wraps its recipient and no seat binds it. Its identity is a release
/// build's (unmarked) unless `lab`.
struct Seatless {
    macs: TwoMacs,
    s: Paths,
    /// The vault's signer read as ALab's on this thread from here on.
    alab: crate::envelope::Retired,
}

fn seatless(lab: bool) -> Seatless {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let s = Paths::for_home(&macs.root.path().join("s"));
    let making = if lab {
        crate::Making::now()
    } else {
        crate::Making::default()
    };
    let recipient = machine_identity_as(&s, making)
        .expect("identity")
        .recipient()
        .to_bech32();
    {
        let admin = admin_open(&macs.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("s", &recipient).unwrap();
        admin_publish_approved(&macs.m22, &admin, &doc, std::slice::from_ref(&recipient)).unwrap();
    }
    join_host(&s, &format!("{}#{}", macs.url, macs.signer), "s");
    sync(&s, None).expect("s syncs");
    let alab = crate::envelope::Retired {
        keys: Vec::new(),
        current: config(&s).expect("config").expect("a vault").signer,
    };
    crate::envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = Some(alab.clone()));
    Seatless { macs, s, alab }
}

fn node_pair(tag: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[tag; 32]).expect("node key")
}

fn log_of(paths: &Paths) -> crate::membership_log::Log {
    load_records(paths)
        .expect("records")
        .log()
        .cloned()
        .expect("a log")
}

/// The records `after` holds that `before` did not, by kind.
fn added(before: &crate::membership_log::Log, after: &crate::membership_log::Log) -> Vec<Kind> {
    let held: std::collections::BTreeSet<_> = before.records().map(|r| *r.id()).collect();
    after
        .records()
        .filter(|r| !held.contains(r.id()))
        .map(Record::kind)
        .collect()
}

/// The ask `s`'s subscriber makes through `ask_members_if_due`, as `aterm members`'s hook makes
/// it (`members_cli::ask_members`): a proof of `s`'s own request and node key to m22's desk,
/// reached on the home ALab LAN, at `now_ms` on m22's clock. Counts each ask in `asks`.
fn asking<'a>(
    world: &'a Seatless,
    pair: &'a Ed25519KeyPair,
    asks: &'a AtomicUsize,
    now_ms: u64,
) -> impl Fn(&Paths) -> crate::Result<usize> + 'a {
    asking_authorized(world, pair, asks, now_ms, &|| Ok(()), &|| {})
}

fn asking_authorized<'a>(
    world: &'a Seatless,
    pair: &'a Ed25519KeyPair,
    asks: &'a AtomicUsize,
    now_ms: u64,
    current: &'a (dyn Fn() -> crate::Result<()> + Sync),
    handed: &'a (dyn Fn() + Sync),
) -> impl Fn(&Paths) -> crate::Result<usize> + 'a {
    move |paths: &Paths| {
        asks.fetch_add(1, Ordering::SeqCst);
        let m22 = &world.macs.m22;
        let desk = std::sync::Mutex::new(Desk::new(
            Local::of(m22)
                .expect("m22's desk")
                .expect("m22 is a member")
                .with_home(Some(crate::home_lan::HomeNet::test())),
        ));
        let device = crate::commons::bootstrap::Device::open(paths)
            .expect("device")
            .expect("an identity");
        let node: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
        let joining = Joining {
            records_bound: crate::crossing::RecordsBound::uncrossed(),
            device: &device,
            node,
            name: "s".to_owned(),
            model: "Mac15,3".to_owned(),
        };
        let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);
        let sign_node = |message: &[u8]| -> [u8; 64] {
            pair.sign(&[prefix.as_bytes(), message].concat())
                .as_ref()
                .try_into()
                .unwrap()
        };
        let alab = world.alab.clone();
        let decide = |proving: &desk::Proving<'_>| {
            crate::envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = Some(alab.clone()));
            let bundle = prove_at_ms(m22, proving, CURRENT, now_ms)?;
            handed();
            Ok(bundle)
        };
        let proving = join::ProofLock::take(paths, None)?.expect("no other proof");
        let expected = config(paths)?;
        let proved = std::thread::scope(|scope| {
            let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
                let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
                for end in [&ours, &theirs] {
                    end.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
                }
                let (desk, decide) = (&desk, &decide);
                scope.spawn(move || {
                    drop(desk::serve_with(
                        desk,
                        &mut ours,
                        std::net::IpAddr::from([192, 168, 1, 87]),
                        &|_| Err(crate::Error::new("no phrase runs here")),
                        Some(desk::Proof {
                            decide,
                            refuses: &|_| None,
                            authority: None,
                            fleet: None,
                        }),
                    ));
                });
                Ok(Box::new(theirs))
            };
            join::prove_first_admitted(
                paths,
                &dial,
                &joining,
                &sign_node,
                &["m22:7100".to_owned()],
                &join::ProofAdmission {
                    proving: &proving,
                    expected: expected.as_ref(),
                    default_alab: false,
                    current,
                },
            )
        });
        proved
            .map(|_| 1)
            .map_err(|missed| crate::Error::new(format!("no member seated s: {missed:?}")))
    }
}

/// A subscriber round's ask, its pause gone by (`ask_members_if_due`).
fn round(paths: &Paths, asked: &mut MembersAsked, ask: &dyn Fn(&Paths) -> crate::Result<usize>) {
    if let Some(at) = asked.at {
        asked.at = at.checked_sub(ask_members_pause(asked.asks) + Duration::from_secs(1));
    }
    ask_members_if_due(paths, asked, ask, &|line| eprintln!("round: {line}"));
}

fn me(paths: &Paths, of: &Paths) -> MemberId {
    let trust = crate::commons::bootstrap::Bootstrap::required(of)
        .expect("member")
        .trust();
    crate::commons::bootstrap::Device::open(paths)
        .expect("device")
        .expect("an identity")
        .request(trust.network, trust.authority)
        .expect("request")
        .binding()
        .actor()
}

/// Why `paths`'s Network State cards read it unusable: its secrets card's blocker
/// ([`observe_materialization`]) and its accounts card's ([`crate::commons::observe`]).
fn cards(
    paths: &Paths,
) -> (
    Option<MaterializationBlocker>,
    Option<crate::commons::Unavailable>,
) {
    let configured = config(paths).expect("config").expect("a vault");
    let secrets = match observe_materialization(paths, &configured, || Ok(())).expect("observed") {
        MaterializationObservation::Blocked(why) => Some(why),
        MaterializationObservation::Ready(_) => None,
    };
    let accounts = match crate::commons::observe(paths).expect("observed") {
        crate::commons::Observation::Unavailable(why) => Some(why),
        _ => None,
    };
    (secrets, accounts)
}

/// A Mac m22's approver let in through the envelope alone holds the vault but no seat: its log
/// reads it [`Seat::Seatless`], its subscriber's first round asks once, by proof from the home
/// ALab LAN, and m22 seats it with the very recipient it held (no new identity, no rotation),
/// signing one `Admit` and one `Wrap` per epoch it hands on (3 records here); a second round, seated, asks
/// nothing. The next `[everyone]` rotation (m22 removes m7) carries the new seat.
///
/// NEGATIVE CONTROL: [`seat_ask::NO_SEAT_ASK`] on in
/// [`a_seatless_recipient_that_asks_nothing_drops_at_the_next_rotation`].
#[test]
fn a_seatless_recipient_asks_once_and_takes_a_seat_with_the_same_key() {
    let _answering = ServersAnswering::set();
    let world = seatless(false);
    let (m22, s) = (&world.macs.m22, &world.s);
    assert_eq!(
        seat(s),
        Seat::Seatless,
        "the envelope let s in with no seat"
    );
    let recipient = read_identity(s).unwrap().unwrap().recipient();
    let held = [
        s.source_file(),
        s.state_file(),
        s.identity_file(),
        s.home.join(".secrets/shared"),
    ];
    let before_files = held.each_ref().map(|path| std::fs::read(path).unwrap());
    let before = log_of(m22);
    assert!(before.wraps_maximal_audience_to(&EpochAudience::Everyone, &recipient));
    assert!(seat_ask::asks_for_seat(s));

    let asks = AtomicUsize::new(0);
    let pair = node_pair(0x5e);
    let node: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
    let now = aterm_time::unix_millis().unwrap();
    let ask = asking(&world, &pair, &asks, now);
    let mut asked = MembersAsked {
        network: || Some("home".to_owned()),
        ..MembersAsked::default()
    };
    round(s, &mut asked, &ask);
    assert_eq!(asks.load(Ordering::SeqCst), 1, "one ask on the first round");

    assert_eq!(seat(s), Seat::Seated, "s is seated");
    assert_eq!(
        held.each_ref().map(|path| std::fs::read(path).unwrap()),
        before_files
    );
    assert_eq!(
        read_identity(s).unwrap().unwrap().recipient(),
        recipient,
        "with the same recipient: no new identity"
    );
    let after = log_of(m22);
    let actor = me(s, m22);
    let membership = after.membership();
    let held = membership.get(&actor).expect("a seat binds s's request");
    assert_eq!(held.binding.recipient(), recipient);
    let signed = added(&before, &after);
    // The whole cost: the `Admit` (which binds the node key the Mac proved, so it signs no
    // `Node` record, and names it, so no `Name` follows) and one `Wrap` of each epoch it hands
    // on, here the newest `[everyone]` and Commons epochs.
    assert_eq!(
        signed,
        [Kind::Admit, Kind::Wrap, Kind::Wrap],
        "the seat's record cost"
    );
    assert_eq!(
        held.node_key,
        Some(node),
        "the Admit binds the node key it proved"
    );

    round(s, &mut asked, &ask);
    round(s, &mut asked, &ask);
    assert_eq!(asks.load(Ordering::SeqCst), 1, "a seated Mac asks nothing");

    // The next rotation carries the seat: m22 removes m7.
    let (_, actor7, _) = member_key(&world.macs.m7);
    super::super::remove::remove_actor(m22, actor7).expect("m22 removes m7");
    let rotated = log_of(m22);
    assert!(
        rotated.wraps_maximal_audience_to(&EpochAudience::Everyone, &recipient),
        "the rotation carries s"
    );
    drop(ask);
    world.macs.server.stop();
}

/// A Mac whose records sit in a layout (its log past the fence) reads the seat a member's
/// records gave it at its next ask: those records go on the end of the layout's data file in
/// place, which changes nothing at the records entry (a directory), so the memoized seat is
/// marked by the data file too ([`seat_ask::Mark`]).
///
/// NEGATIVE CONTROL (2026-11-11): with the memo marked by the records entry's length and
/// modification time, as it was while every records write renamed a file into that directory,
/// the seated Mac's next ask still answers the memoized seatless read (`the seat is read`).
#[test]
fn a_seat_appended_to_a_layouts_data_file_is_read_by_the_next_ask() {
    use std::os::unix::fs::MetadataExt as _;
    let _answering = ServersAnswering::set();
    let world = seatless(false);
    let s = &world.s;
    drop(crate::records_storage::write_path(&s.records_file(), true).unwrap());
    let (entry, data) = (s.records_file(), s.records_data_file().unwrap());
    assert_eq!(data.parent(), Some(entry.as_path()), "a layout");
    assert!(seat_ask::asks_for_seat(s), "seatless, and memoized so");
    let stood = |path: &Path| {
        let meta = std::fs::symlink_metadata(path).unwrap();
        (meta.ino(), meta.len(), meta.modified().unwrap())
    };
    let (entry_before, data_before) = (stood(&entry), stood(&data));

    let asks = AtomicUsize::new(0);
    let pair = node_pair(0x5e);
    let ask = asking(&world, &pair, &asks, aterm_time::unix_millis().unwrap());
    let mut asked = MembersAsked {
        network: || Some("home".to_owned()),
        ..MembersAsked::default()
    };
    round(s, &mut asked, &ask);
    assert_eq!(seat(s), Seat::Seated, "m22 seated s");
    let data_after = stood(&data);
    assert_eq!(
        data_after.0, data_before.0,
        "the records were appended in place"
    );
    assert!(data_after.1 > data_before.1);
    assert_eq!(stood(&entry), entry_before, "nothing moved at the entry");
    assert!(!seat_ask::asks_for_seat(s), "the seat is read");
    drop(ask);
    world.macs.server.stop();
}

#[test]
fn a_seat_proof_rechecks_stop_before_merging_without_rewriting_the_existing_vault() {
    use std::sync::atomic::AtomicBool;
    let _answering = ServersAnswering::set();
    let world = seatless(false);
    let s = &world.s;
    let held = [
        s.records_file(),
        s.source_file(),
        s.state_file(),
        s.identity_file(),
        s.home.join(".secrets/shared"),
    ];
    let before = held.each_ref().map(|path| std::fs::read(path).unwrap());
    let stopped = AtomicBool::new(false);
    let current = || {
        if stopped.load(Ordering::SeqCst) {
            Err(crate::Error::new("setup stopped before taking the seat"))
        } else {
            assert!(
                StateLock::observe(s).unwrap().is_some(),
                "pre-dial guard holds no state lock"
            );
            Ok(())
        }
    };
    let handed = || {
        stopped.store(true, Ordering::SeqCst);
    };
    let asks = AtomicUsize::new(0);
    let pair = node_pair(0x5e);
    let ask = asking_authorized(
        &world,
        &pair,
        &asks,
        aterm_time::unix_millis().unwrap(),
        &current,
        &handed,
    );
    let why = ask(s).unwrap_err().to_string();
    assert!(
        why.contains("setup stopped before taking the seat"),
        "{why}"
    );
    assert_eq!(seat(s), Seat::Seatless);
    assert_eq!(
        held.each_ref().map(|path| std::fs::read(path).unwrap()),
        before
    );
    drop(ask);
    world.macs.server.stop();
}

/// NEGATIVE CONTROL of [`a_seatless_recipient_asks_once_and_takes_a_seat_with_the_same_key`]:
/// with the self-ask off ([`seat_ask::NO_SEAT_ASK`], as 0.154.0 and before), the seatless Mac's
/// rounds ask nothing, and the next `[everyone]` rotation, whose carry is the current seats,
/// drops it.
#[test]
fn a_seatless_recipient_that_asks_nothing_drops_at_the_next_rotation() {
    let _answering = ServersAnswering::set();
    let world = seatless(false);
    let (m22, s) = (&world.macs.m22, &world.s);
    let recipient = read_identity(s).unwrap().unwrap().recipient();
    let asks = AtomicUsize::new(0);
    let pair = node_pair(0x5e);
    let ask = asking(&world, &pair, &asks, aterm_time::unix_millis().unwrap());
    let mut asked = MembersAsked {
        network: || Some("home".to_owned()),
        ..MembersAsked::default()
    };
    seat_ask::NO_SEAT_ASK.with(|cell| cell.set(true));
    round(s, &mut asked, &ask);
    round(s, &mut asked, &ask);
    seat_ask::NO_SEAT_ASK.with(|cell| cell.set(false));
    assert_eq!(asks.load(Ordering::SeqCst), 0, "the control asks nothing");
    assert_eq!(seat(s), Seat::Seatless);

    let (_, actor7, _) = member_key(&world.macs.m7);
    super::super::remove::remove_actor(m22, actor7).expect("m22 removes m7");
    assert!(
        !log_of(m22).wraps_maximal_audience_to(&EpochAudience::Everyone, &recipient),
        "the rotation dropped the seatless recipient"
    );
    drop(ask);
    world.macs.server.stop();
}

/// A recipient a rotation drops with no `Remove` of it is out, not revoked (one-mesh E2, f204):
/// the seatless Mac asks nothing, m22's removal of m7 rotates `[everyone]` without it, and its
/// sync opens no locker ([`Stored::Revoked`]), as a removed Mac's does. Its own log reads it
/// [`Seat::Out`], [`dropped`] holds, and `aterm secrets status` and its JSON, and Network
/// State's secrets and accounts cards, say it was dropped, with the way back (a new Mac), where
/// they said "revoked".
///
/// NEGATIVE CONTROL: [`seat_ask::REVOKED_UNJUDGED`] on reads the Mac as before E2:
/// [`Seat::Unjudged`] and "revoked (out as of seq N)". Its counterpart,
/// [`a_removed_key_never_gets_a_seat_back`], holds a `Remove`d Mac to "revoked": with
/// `removed_by_an_act` cut from [`dropped`], it reads "dropped" there and fails.
#[test]
fn a_recipient_a_rotation_drops_with_no_remove_reads_out_and_says_dropped() {
    let _answering = ServersAnswering::set();
    let world = seatless(false);
    let (m22, s) = (&world.macs.m22, &world.s);
    let recipient = read_identity(s).unwrap().unwrap().recipient();
    assert_eq!(seat(s), Seat::Seatless);
    assert!(!dropped(s), "a Mac that is in was not dropped");

    let (_, actor7, _) = member_key(&world.macs.m7);
    super::super::remove::remove_actor(m22, actor7).expect("m22 removes m7");
    post_records_to_servers(m22).expect("m22 posts the rotation");
    let synced = sync(s, None).expect("s syncs the rotation");
    assert!(
        matches!(synced, SyncOutcome::NotIn),
        "the rotation left s out: {synced:?}"
    );
    assert_eq!(load_state(s).unwrap().membership, Stored::Revoked);
    let rotated = log_of(m22);
    assert!(!rotated.recipient_is_removed(&recipient), "no act names s");
    assert!(
        rotated.audience_left_out_recipient(&EpochAudience::Everyone, &recipient),
        "the rotation left s's recipient out"
    );
    let Membership::Revoked { seq } = status(s).expect("status").membership else {
        panic!("s reads itself out");
    };

    // CONTROL: read as before E2.
    seat_ask::REVOKED_UNJUDGED.with(|cell| cell.set(true));
    let control = (
        seat(s),
        dropped(s),
        out_words(s, seq),
        dropped_words(s, seq),
        cards(s),
    );
    seat_ask::REVOKED_UNJUDGED.with(|cell| cell.set(false));
    assert_eq!(control.0, Seat::Unjudged, "the control judges nothing");
    assert!(!control.1);
    assert_eq!(control.2, format!("revoked (out as of seq {seq})"));
    assert_eq!(control.3, None);
    assert_eq!(
        control.4,
        (
            Some(MaterializationBlocker::Revoked),
            Some(crate::commons::Unavailable::Revoked)
        )
    );

    assert_eq!(seat(s), Seat::Out, "s's own log reads it out");
    assert!(dropped(s), "no act took s out");
    let words = out_words(s, seq);
    assert!(
        words.starts_with("dropped: no seat; a rotation left this Mac out with no Remove"),
        "{words}"
    );
    assert!(words.contains("come back as a new Mac"), "{words}");
    assert!(!words.contains("revoked"), "{words}");
    assert_eq!(dropped_words(s, seq).as_deref(), Some(words.as_str()));
    assert_eq!(
        cards(s),
        (
            Some(MaterializationBlocker::Dropped),
            Some(crate::commons::Unavailable::Dropped)
        ),
        "Network State's secrets and accounts cards say dropped"
    );
    let json = crate::operational::metadata(s);
    assert_eq!(
        json["vault"]["value"]["membership"]["state"].as_str(),
        Some("dropped"),
        "{json}"
    );
    assert!(
        !seat_ask::asks_for_seat(s),
        "a dropped Mac asks for no seat"
    );
    world.macs.server.stop();
}

/// A dropped Mac's join of the vault that dropped it sends no key (one-mesh E2, f299): `aterm
/// secrets join` is refused under the state lock with [`crate::ErrorKind::Dropped`] and
/// [`dropped_words`], the vault queues no request with its recipient, and the Mac stays revoked,
/// so it still reads dropped and says how it comes back.
///
/// NEGATIVE CONTROL: [`seat_ask::REVOKED_UNJUDGED`] on (no dropped judgment), the same join goes
/// on as before f299: it moves the Mac to pending and queues its request with the refused key.
#[test]
fn a_dropped_macs_join_sends_no_key_and_keeps_its_way_back() {
    let _answering = ServersAnswering::set();
    let world = seatless(false);
    let (m22, s) = (&world.macs.m22, &world.s);
    let recipient = read_identity(s).unwrap().unwrap().recipient().to_bech32();
    let (_, actor7, _) = member_key(&world.macs.m7);
    super::super::remove::remove_actor(m22, actor7).expect("m22 removes m7");
    post_records_to_servers(m22).expect("m22 posts the rotation");
    sync(s, None).expect("s syncs the rotation");
    assert!(dropped(s), "the rotation dropped s");
    let Membership::Revoked { seq } = status(s).expect("status").membership else {
        panic!("s reads itself out");
    };
    let source = format!("{}#{}", world.macs.url, world.macs.signer);
    let join_s = || {
        join(
            s,
            &JoinOptions {
                source: source.clone(),
                host: Some("s".to_owned()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
    };
    let queued = || {
        requests_for(m22, &config(m22).unwrap().unwrap(), None)
            .expect("the vault's requests")
            .iter()
            .any(|request| request.recipient == recipient)
    };

    let refused = join_s().expect_err("a dropped Mac's join is refused");
    assert_eq!(refused.kind(), crate::ErrorKind::Dropped, "{refused}");
    assert_eq!(Some(refused.to_string()), dropped_words(s, seq));
    assert_eq!(load_state(s).unwrap().membership, Stored::Revoked);
    assert!(dropped(s), "s still reads dropped");
    assert!(!queued(), "no request carries the refused key");

    seat_ask::REVOKED_UNJUDGED.with(|cell| cell.set(true));
    let control = join_s();
    seat_ask::REVOKED_UNJUDGED.with(|cell| cell.set(false));
    assert!(
        !control
            .as_ref()
            .is_err_and(|error| error.kind() == crate::ErrorKind::Dropped),
        "the control judges nothing: {control:?}"
    );
    assert_eq!(load_state(s).unwrap().membership, Stored::Pending);
    assert!(queued(), "the control's join asks with the refused key");
    world.macs.server.stop();
}

/// A join reads the vault this Mac has configured again ([`super::super::joins_again`], where a
/// dropped Mac's join is refused) by its name and a key its pin admits, never by its address:
/// the pin the join verified or the key it handed off to, or one the configured vault followed.
/// Another name or another signer is another vault, which a dropped Mac joins as before.
#[test]
fn a_join_reads_the_configured_vault_again_by_name_and_pin_not_address() {
    let joins_again = super::super::joins_again;
    let (signer, handed, followed, other) = ([1; 32], [2; 32], [3; 32], [4; 32]);
    let configured = Config {
        source: Source::Url("http://192.0.2.1:7/v1/alab".to_owned()),
        mirrors: Vec::new(),
        name: "alab".to_owned(),
        signer: Some(signer),
        followed: vec![followed],
        host: "s".to_owned(),
    };
    assert!(joins_again(&configured, "alab", signer, signer));
    assert!(
        joins_again(&configured, "alab", handed, signer),
        "the pin verified handed off to the configured key"
    );
    assert!(
        joins_again(&configured, "alab", signer, handed),
        "the configured key handed off"
    );
    assert!(joins_again(&configured, "alab", followed, followed));
    assert!(
        !joins_again(&configured, "alab", other, other),
        "another signer"
    );
    assert!(
        !joins_again(&configured, "team", signer, signer),
        "another name"
    );
}

/// A seatless Mac whose identity a test, a debug build, a lab lane or a VM made founds its own
/// lab: its rounds ask ALab's members nothing, though its log reads it seatless.
#[test]
fn a_lab_identity_that_is_seatless_asks_for_no_seat() {
    let world = seatless(true);
    let s = &world.s;
    assert_eq!(seat(s), Seat::Seatless);
    assert!(
        !seat_ask::asks_for_seat(s),
        "a lab identity asks for no seat"
    );
    let asks = AtomicUsize::new(0);
    let ask = |_: &Paths| Ok(asks.fetch_add(1, Ordering::SeqCst));
    let mut asked = MembersAsked::default();
    round(s, &mut asked, &ask);
    round(s, &mut asked, &ask);
    assert_eq!(asks.load(Ordering::SeqCst), 0);
    world.macs.server.stop();
}

/// A key the log removed never takes a seat back by asking: once seated and then removed, the
/// Mac reads its removal, the log reads its recipient [`Seat::Out`], its rounds ask nothing, and
/// a proof it makes anyway, under a fresh node key, is refused for the removal with nothing
/// signed.
#[test]
fn a_removed_key_never_gets_a_seat_back() {
    let _answering = ServersAnswering::set();
    let world = seatless(false);
    let (m22, s) = (&world.macs.m22, &world.s);
    let asks = AtomicUsize::new(0);
    let pair = node_pair(0x5e);
    let ask = asking(&world, &pair, &asks, aterm_time::unix_millis().unwrap());
    let mut asked = MembersAsked {
        network: || Some("home".to_owned()),
        ..MembersAsked::default()
    };
    round(s, &mut asked, &ask);
    assert_eq!(seat(s), Seat::Seated);
    super::super::remove::remove_actor(m22, me(s, m22)).expect("m22 removes s");
    post_records_to_servers(m22).expect("m22 posts the removal");
    let synced = sync(s, None).expect("s syncs the removal");
    assert!(
        matches!(synced, SyncOutcome::NotIn),
        "s reads its removal: {synced:?}"
    );
    assert_eq!(load_state(s).unwrap().membership, Stored::Revoked);
    let recipient = read_identity(s).unwrap().unwrap().recipient();
    let removed = log_of(m22);
    assert!(removed.recipient_is_removed(&recipient));
    assert_eq!(super::judge(&removed, &recipient), Seat::Out);
    // A `Remove` revokes (one-mesh E2): its own log reads it out, and its status says revoked.
    assert_eq!(seat(s), Seat::Out, "s's own log reads its removal");
    assert!(!dropped(s), "a removed Mac was revoked, not dropped");
    let Membership::Revoked { seq } = status(s).expect("status").membership else {
        panic!("s reads itself out");
    };
    assert_eq!(out_words(s, seq), format!("revoked (out as of seq {seq})"));
    assert_eq!(dropped_words(s, seq), None);
    assert_eq!(
        cards(s),
        (
            Some(MaterializationBlocker::Revoked),
            Some(crate::commons::Unavailable::Revoked)
        ),
        "a removed Mac's cards say revoked"
    );
    let json = crate::operational::metadata(s);
    assert_eq!(
        json["vault"]["value"]["membership"]["state"].as_str(),
        Some("revoked"),
        "{json}"
    );
    assert!(
        !seat_asks(s) && !seat_ask::asks_for_seat(s),
        "a removed Mac asks for no seat"
    );
    round(s, &mut asked, &ask);
    round(s, &mut asked, &ask);
    assert_eq!(asks.load(Ordering::SeqCst), 1, "its rounds ask nothing");

    // A proof it makes anyway, from the home ALab LAN under a fresh node key, is refused for the
    // removal, with nothing signed.
    let before = log_of(m22);
    let fresh = node_pair(0x6f);
    let forced = asking(&world, &fresh, &asks, aterm_time::unix_millis().unwrap())(s);
    let why = forced
        .expect_err("a removed key's proof is refused")
        .to_string();
    assert!(why.contains("the vault removed this Mac"), "{why}");
    assert!(
        added(&before, &log_of(m22)).is_empty(),
        "nothing was signed for a removed key"
    );
    drop(ask);
    world.macs.server.stop();
}

/// Off the home ALab LAN a seatless Mac's proof is refused, and its plain ask shows a row; a
/// person's Allow and the phrase then seat it with the same recipient: its run accepts the log
/// bundle as its seat's records ([`accepts_seat`]) and merges them ([`take_seat`]), keeping its
/// configuration and files, where a new Mac's write refuses a Mac already in a vault.
///
/// NEGATIVE CONTROL: the run accepting by `client::accepts_bundle` alone refuses the bundle
/// ("already in a vault") and the run's `expect` fails.
#[test]
fn a_persons_allow_seats_a_seatless_recipient_off_the_lan() {
    let _answering = ServersAnswering::set();
    let world = seatless(false);
    let (m22, s) = (&world.macs.m22, &world.s);
    let recipient = read_identity(s).unwrap().unwrap().recipient();
    let source = std::fs::read(s.source_file()).unwrap();
    let desk = std::sync::Mutex::new(Desk::new(
        Local::of(m22)
            .expect("m22's desk")
            .expect("m22 is a member")
            .with_home(Some(crate::home_lan::HomeNet::test())),
    ));
    let device = crate::commons::bootstrap::Device::open(s)
        .expect("device")
        .expect("an identity");
    let joining = Joining {
        records_bound: crate::crossing::RecordsBound::uncrossed(),
        device: &device,
        node: [0x51; 32],
        name: "s".to_owned(),
        model: "Mac15,3".to_owned(),
    };
    let hand = |handing: &crate::members::desk::Handing<'_>| hand_over(m22, handing);
    std::thread::scope(|scope| {
        let dial = |_: &str| -> crate::Result<Box<dyn Stream>> {
            let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
            }
            let (desk, hand) = (&desk, &hand);
            scope.spawn(move || {
                drop(desk::serve(
                    desk,
                    &mut ours,
                    std::net::IpAddr::from([203, 0, 113, 9]),
                    hand,
                ));
            });
            Ok(Box::new(theirs))
        };
        let (_, missed) = join::ask_all(&dial, &joining, &["m22:7100".to_owned()]);
        assert!(missed.is_empty(), "{missed:?}");
        let words = {
            let mut desk = desk.lock().unwrap();
            let now = aterm_time::Instant::now();
            let rows = desk.rows(now);
            assert_eq!(rows.len(), 1, "the seatless Mac's ask shows a row");
            let phrase = desk.allow(rows[0].id, now).expect("a person allows it");
            desk.words(phrase, now).expect("live").join(" ")
        };
        let heard = join::ask_all(&dial, &joining, &["m22:7100".to_owned()]).0;
        assert_eq!(heard.len(), 1);
        let phrase = crate::phrase::Phrase::parse(&words).expect("three words");
        join::run_phrase(s, &dial, &joining, &heard, &phrase, None, &mut |_| {})
            .expect("the phrase seats the seatless Mac");
    });
    assert_eq!(seat(s), Seat::Seated);
    assert_eq!(read_identity(s).unwrap().unwrap().recipient(), recipient);
    assert_eq!(
        std::fs::read(s.source_file()).unwrap(),
        source,
        "its configuration is kept"
    );
    assert_eq!(
        std::fs::read(s.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    world.macs.server.stop();
}
