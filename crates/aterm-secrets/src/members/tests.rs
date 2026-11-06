// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::bundle::Handed;
use super::desk::{self, Answer, Desk, Handing, Handout, Local, RowState};
use super::joiner::{self, Joining};
use super::{Allowed, Bundle};
use crate::client::{self, Paths};
use crate::commons::bootstrap::Device;
use crate::commons::journal::Trust;
use crate::phrase::Phrase;
use crate::text::{self, Audience, Entry};
use crate::{Result, SecretBytes};
use aterm_time::Instant;
use std::net::IpAddr;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

const TRUST: Trust = Trust {
    network: [1; 16],
    authority: [2; 32],
    genesis: [3; 32],
};

/// A Mac with a machine key and a Commons device, under `root/name`.
fn mac(root: &Path, name: &str) -> (Paths, Device) {
    let paths = Paths::for_home(&root.join(name));
    client::machine_identity(&paths).expect("identity");
    let device = Device::open(&paths).expect("device").expect("an identity");
    (paths, device)
}

fn member(root: &Path, name: &str) -> Mutex<Desk> {
    let (_, device) = mac(root, name);
    Mutex::new(Desk::new(
        Local::with(device, TRUST, true, name).expect("member"),
    ))
}

fn token() -> String {
    format!(
        "synthetic-gh-{}",
        aterm_uds::rand::hex_token::<6>().expect("random")
    )
}

/// The F10 bundle a member hands: one `[everyone]` file holding `value`.
fn files_bundle(allowed: &Allowed, value: &str) -> Result<SecretBytes> {
    Bundle {
        vault: "alab".into(),
        signer: [9; 32],
        followed: Vec::new(),
        servers: vec!["http://192.0.2.1:7000/v1/alab".into()],
        allowed: allowed.clone(),
        handed: Handed::Files(text::render_everyone_locker(&[Entry {
            name: "gh_access_token".into(),
            value: SecretBytes::new(value.as_bytes().to_vec()),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        }])),
    }
    .encode()
}

/// A desk's bundle maker that hands [`files_bundle`] holding `value`.
fn bundle_of(value: &str) -> impl Fn(&Handing<'_>) -> Result<Handout> + '_ {
    move |handing| {
        Ok(Handout {
            bundle: files_bundle(handing.allowed, value)?,
            logged: false,
            note: None,
        })
    }
}

/// One connection: the member serves it while `joiner` speaks, each end with a one-minute hang
/// detector.
fn exchange<T>(
    desk: &Mutex<Desk>,
    from: IpAddr,
    files: &str,
    joiner: impl FnOnce(&mut UnixStream) -> T,
) -> (Result<Option<desk::Let>>, T) {
    let (mut ours, mut theirs) = UnixStream::pair().expect("pair");
    for end in [&ours, &theirs] {
        end.set_read_timeout(Some(Duration::from_secs(60)))
            .expect("timeout");
    }
    std::thread::scope(|scope| {
        let bundle = bundle_of(files);
        let served = scope.spawn(move || desk::serve(desk, &mut ours, from, &bundle));
        let said = joiner(&mut theirs);
        drop(theirs);
        (served.join().expect("member"), said)
    })
}

fn joining<'a>(device: &'a Device, node: [u8; 32], name: &str) -> Joining<'a> {
    Joining {
        records_bound: crate::crossing::RecordsBound::uncrossed(),
        device,
        node,
        name: name.into(),
        model: "Mac15,3".into(),
    }
}

fn ask(desk: &Mutex<Desk>, from: IpAddr, joining: &Joining<'_>) -> Answer {
    exchange(desk, from, "", |stream| {
        let greeting = joiner::greeting(stream)?;
        joiner::ask(stream, &greeting, joining)
    })
    .1
    .expect("asked")
}

#[test]
fn an_admitted_ask_rechecks_after_the_real_member_greeting() {
    use super::join;
    use std::sync::atomic::{AtomicUsize, Ordering};
    for withdraw in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let desk = member(root.path(), "member");
        let (_, device) = mac(root.path(), "joining");
        let joining = joining(&device, [7; 32], "joining");
        let checks = AtomicUsize::new(0);
        let current = || {
            let check = checks.fetch_add(1, Ordering::SeqCst);
            if withdraw && check > 0 {
                Err(crate::Error::new("setup stopped after greeting"))
            } else {
                Ok(())
            }
        };
        std::thread::scope(|scope| {
            let dial = |_: &str| -> Result<Box<dyn join::Stream>> {
                let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
                let ours = std::net::TcpStream::connect(listener.local_addr()?)?;
                let (mut theirs, from) = listener.accept()?;
                for stream in [&ours, &theirs] {
                    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
                }
                let desk = &desk;
                scope.spawn(move || {
                    let _ = desk::serve(desk, &mut theirs, from.ip(), &bundle_of(""));
                });
                Ok(Box::new(ours))
            };
            let (heard, missed) =
                join::ask_all_admitted(&dial, &joining, &["member".into()], &current);
            assert_eq!(heard.len(), usize::from(!withdraw));
            assert_eq!(missed.len(), usize::from(withdraw));
        });
        assert_eq!(
            states(&desk).len(),
            usize::from(!withdraw),
            "a greeting alone must create no admission request"
        );
    }
}

fn run(
    desk: &Mutex<Desk>,
    from: IpAddr,
    joining: &Joining<'_>,
    words: &str,
    files: &str,
) -> (Result<Option<desk::Let>>, Result<Bundle>) {
    let phrase = Phrase::parse(words).expect("three words");
    exchange(desk, from, files, |stream| {
        let greeting = joiner::greeting(stream)?;
        joiner::run(stream, &greeting, joining, &phrase)
    })
}

fn done(desk: &Mutex<Desk>, from: IpAddr, joining: &Joining<'_>) {
    exchange(desk, from, "", |stream| {
        let greeting = joiner::greeting(stream)?;
        joiner::done(stream, &greeting, joining)
    })
    .1
    .expect("done");
}

/// Allow the only asking row on `desk`, and the words its sheet would draw.
fn allow(desk: &Mutex<Desk>) -> String {
    let mut desk = desk.lock().unwrap();
    let now = Instant::now();
    let row = desk.rows(now)[0].id;
    let phrase = desk.allow(row, now).expect("allowed");
    desk.words(phrase, now).expect("live").join(" ")
}

fn states(desk: &Mutex<Desk>) -> Vec<RowState> {
    let desk = desk.lock().unwrap();
    desk.rows(Instant::now())
        .into_iter()
        .map(|row| row.state)
        .collect()
}

fn address(n: u8) -> IpAddr {
    IpAddr::from([192, 0, 2, n])
}

#[test]
fn an_allowed_round_trips_and_refuses_a_changed_byte_or_its_own_writer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, joiner) = mac(root.path(), "j");
    let (_, member) = mac(root.path(), "m");
    let binding = joiner.request(TRUST.network, TRUST.authority).unwrap();
    let allowed = member
        .with_seed(|seed| Allowed::sign(binding.binding(), "j", seed))
        .unwrap();
    assert_eq!(Allowed::from_text(&allowed.to_text()).unwrap(), allowed);
    let mut changed = allowed.bytes().to_vec();
    let last = changed.len() - 1;
    changed[last] ^= 1;
    assert!(Allowed::decode(&changed).is_err(), "a changed signature");
    changed[last] ^= 1;
    changed[200] ^= 1;
    assert!(Allowed::decode(&changed).is_err(), "a changed signer");
    let own = joiner.with_seed(|seed| Allowed::sign(binding.binding(), "j", seed));
    assert!(own.is_err(), "a Mac cannot allow itself");
    assert!(
        member
            .with_seed(|seed| Allowed::sign(binding.binding(), "Not A Name", seed))
            .is_err()
    );
}

#[test]
fn a_bundle_round_trips_its_fields_and_files() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, joiner) = mac(root.path(), "j");
    let (_, member) = mac(root.path(), "m");
    let request = joiner.request(TRUST.network, TRUST.authority).unwrap();
    let allowed = member
        .with_seed(|seed| Allowed::sign(request.binding(), "j", seed))
        .unwrap();
    let value = token();
    let text = files_bundle(&allowed, &value).unwrap();
    let bundle = Bundle::decode(text.as_slice()).unwrap();
    assert_eq!(bundle.vault, "alab");
    assert_eq!(bundle.allowed, allowed);
    let entries = text::parse_locker(bundle.files().expect("files").as_slice()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].value.as_slice(), value.as_bytes());
    assert!(Bundle::decode(&text.as_slice()[1..]).is_err());
}

#[test]
fn log_bundles_preserve_explicit_source_bounds_and_legacy_absence() {
    use crate::crossing::RecordsBound;
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, joiner) = mac(root.path(), "j");
    let (_, member) = mac(root.path(), "m");
    let request = joiner.request(TRUST.network, TRUST.authority).unwrap();
    let allowed = member
        .with_seed(|seed| Allowed::sign(request.binding(), "j", seed))
        .unwrap();
    for source in [
        None,
        Some(RecordsBound::of(4096, false)),
        Some(RecordsBound::of(8192, true)),
    ] {
        let mut bundle =
            Bundle::decode(files_bundle(&allowed, "synthetic").unwrap().as_slice()).unwrap();
        bundle.handed = Handed::Log {
            source,
            admit: [7; 32],
            records: b"opaque record framing".to_vec(),
        };
        let wire = bundle.encode().unwrap();
        assert!(wire.as_slice().starts_with(if source.is_some() {
            b"alab-join-bundle/3\n"
        } else {
            b"alab-join-bundle/2\n"
        }));
        let Handed::Log {
            source: decoded,
            admit,
            records,
        } = Bundle::decode(wire.as_slice()).unwrap().handed
        else {
            panic!("a log bundle must remain a log bundle");
        };
        assert_eq!(decoded, source);
        assert_eq!(admit, [7; 32]);
        assert_eq!(records, b"opaque record framing");
    }
}

#[test]
fn bounded_log_bundle_refuses_malformed_missing_or_downgraded_bounds() {
    use crate::crossing::RecordsBound;
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, joiner) = mac(root.path(), "j");
    let (_, member) = mac(root.path(), "m");
    let request = joiner.request(TRUST.network, TRUST.authority).unwrap();
    let allowed = member
        .with_seed(|seed| Allowed::sign(request.binding(), "j", seed))
        .unwrap();
    let mut bundle =
        Bundle::decode(files_bundle(&allowed, "synthetic").unwrap().as_slice()).unwrap();
    bundle.handed = Handed::Log {
        source: Some(RecordsBound::of(8192, true)),
        admit: [7; 32],
        records: b"opaque record framing".to_vec(),
    };
    let wire = bundle.encode().unwrap();
    let wire = std::str::from_utf8(wire.as_slice()).unwrap();
    for replacement in [
        "",
        "records-bound 1 0 crossed\n",
        "records-bound 1 08192 crossed\n",
        "records-bound 2 8192 crossed\n",
        "records-bound 1 8192 unknown\n",
        "records-bound 1 8192 crossed extra\n",
        "records-bound 1 4294967296 crossed\n",
        "records-bound 1 8192 crossed\nrecords-bound 1 8192 crossed\n",
    ] {
        let invalid = wire.replacen("records-bound 1 8192 crossed\n", replacement, 1);
        assert!(
            Bundle::decode(invalid.as_bytes()).is_err(),
            "{replacement:?}"
        );
    }
    let downgrade = wire.replacen("alab-join-bundle/3", "alab-join-bundle/2", 1);
    assert!(Bundle::decode(downgrade.as_bytes()).is_err());
    if let Handed::Log { source, .. } = &mut bundle.handed {
        *source = Some(RecordsBound::of(0, false));
    }
    assert!(bundle.encode().is_err());
}

#[test]
fn a_request_seen_by_three_members_shows_on_three_and_the_first_allow_closes_the_rest() {
    let root = aterm_tempfile::tempdir().unwrap();
    let members = ["m1", "m2", "m3"].map(|name| member(root.path(), name));
    let (_, device) = mac(root.path(), "j");
    let j = joining(&device, [7; 32], "j");
    // A name that is no machine name, or a model with a control byte, shows no row.
    {
        let request = device.request(TRUST.network, TRUST.authority).unwrap();
        let first = &members[0];
        let mut desk = first.lock().unwrap();
        for (name, model) in [
            ("j\nm22 allowed it", "Mac15,3"),
            ("j", "Mac\u{7}"),
            ("j", ""),
        ] {
            let key = *desk.local().key();
            let signature = ask_signature(&device, &key, [7; 32], name, model, &request);
            let asking = desk::Asking {
                wire: request.bytes(),
                node: [7; 32],
                name,
                model,
                signature: &signature,
            };
            let heard = desk.hear(&asking, address(9), Instant::now());
            assert!(heard.is_err(), "{name:?} {model:?}");
        }
        assert!(desk.rows(Instant::now()).is_empty());
    }
    for desk in &members {
        assert_eq!(ask(desk, address(9), &j), Answer::Asking);
        assert_eq!(states(desk), [RowState::Asking], "the row shows here");
    }
    // Asking again keeps one row.
    assert_eq!(ask(&members[0], address(9), &j), Answer::Asking);
    assert_eq!(states(&members[0]).len(), 1);

    // Allowing on m0 too, and re-allowing on m1 five times in a row, cancels each replaced
    // live phrase on that row so m1 never hits the 4-phrase cap.
    let m0 = &members[0];
    let m0_phrase = {
        let mut desk = m0.lock().unwrap();
        let now = Instant::now();
        let row = desk.rows(now)[0].id;
        desk.allow(row, now).unwrap()
    };
    let mut words = String::new();
    for _ in 0..5 {
        words = allow(&members[1]);
    }
    assert_eq!(ask(&members[1], address(9), &j), Answer::Allowed);
    let value = token();
    // Even if the joiner switches its name in `run`, the signed Allowed uses the name on the row
    // the person allowed ("j"), not the switched name ("m22").
    let switched = joining(&device, [7; 32], "m22");
    let (served, got) = run(&members[1], address(9), &switched, &words, &value);
    let bundle = got.expect("the Allowing member's bundle");
    let let_in = served.expect("served").expect("a run that sent its bundle");
    assert_eq!(let_in.allowed, bundle.allowed);
    assert_eq!(let_in.allowed.name(), "j");
    assert_eq!(
        text::parse_locker(bundle.files().expect("files").as_slice()).unwrap()[0]
            .value
            .as_slice(),
        value.as_bytes()
    );
    assert!(matches!(states(&members[1])[..], [RowState::Closed]));

    for desk in [&members[0], &members[2]] {
        done(desk, address(9), &j);
        assert_eq!(
            states(desk),
            [RowState::Closed],
            "the first Allow closed it"
        );
        let mut desk = desk.lock().unwrap();
        let row = desk.rows(Instant::now())[0].id;
        assert!(desk.allow(row, Instant::now()).is_err(), "a closed row");
    }
    assert_eq!(
        m0.lock().unwrap().phrase_state(m0_phrase),
        Some(crate::pake::PhraseState::Burned),
        "closing a row on another member cancels its live phrase"
    );
}

/// A Mac let in whose bundle never landed (a crash, a Ctrl-C or a dropped network between the
/// member's send and its write, or a write that failed) asks again: the row the run closed shows
/// again, and an Allow there lets it in, though that member is the one Mac it reaches.
///
/// NEGATIVE CONTROL: answer a closed row as before (`hear` returning `answer` for a found row
/// without reopening it): the ask answers Closed and the Allow is refused.
#[test]
fn a_mac_whose_bundle_never_landed_asks_again_and_the_member_that_let_it_in_shows_it_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, device) = mac(root.path(), "j");
    let j = joining(&device, [7; 32], "j");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let words = allow(&m1);
    let (served, got) = run(&m1, address(9), &j, &words, &token());
    served.expect("served").expect("a run that sent its bundle");
    drop(got.expect("the bundle, which the new Mac then loses"));
    assert_eq!(states(&m1), [RowState::Closed]);

    assert_eq!(ask(&m1, address(9), &j), Answer::Asking, "it asks anew");
    assert_eq!(states(&m1), [RowState::Asking], "the row shows again");
    let words = allow(&m1);
    let value = token();
    let (served, got) = run(&m1, address(9), &j, &words, &value);
    served.expect("served").expect("a run that sent its bundle");
    let bundle = got.expect("a second bundle");
    assert_eq!(
        text::parse_locker(bundle.files().expect("files").as_slice()).unwrap()[0]
            .value
            .as_slice(),
        value.as_bytes()
    );
}

#[test]
fn a_phrase_typed_into_another_mac_admits_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let m2 = member(root.path(), "m2");
    let (_, j_device) = mac(root.path(), "j");
    let (_, k_device) = mac(root.path(), "k");
    let j = joining(&j_device, [7; 32], "j");
    // K copies J's public node key, so only J's binding tells their channels apart.
    let k = joining(&k_device, [7; 32], "k");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    assert_eq!(ask(&m1, address(10), &k), Answer::Asking);
    let words = {
        let mut desk = m1.lock().unwrap();
        let now = Instant::now();
        let row = desk
            .rows(now)
            .into_iter()
            .find(|row| row.name == "j")
            .unwrap()
            .id;
        let phrase = desk.allow(row, now).unwrap();
        desk.words(phrase, now).unwrap().join(" ")
    };
    let (served, got) = run(&m1, address(10), &k, &words, "");
    assert!(got.is_err() && served.is_err(), "K typed J's phrase");
    let (served, got) = run(&m2, address(9), &j, &words, "");
    assert!(
        got.is_err() && served.is_err(),
        "J ran it with another member"
    );
    // Neither attempt spent it: J, on the member that showed it, still gets in.
    let (served, got) = run(&m1, address(9), &j, &words, "");
    assert!(got.is_ok(), "{:?}", got.err());
    assert!(served.unwrap().is_some());
}

#[test]
fn a_run_by_a_mac_without_the_requests_keys_gets_no_bundle() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, j_device) = mac(root.path(), "j");
    let (_, k_device) = mac(root.path(), "k");
    let j = joining(&j_device, [7; 32], "j");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let words = allow(&m1);
    // K knows the phrase, replays J's public request and node, and even signs the run with J's
    // writer key: CPace confirms, and step 8's challenge, sealed to J's age key, stops it.
    let request = j.request(&joiner_greeting(&m1)).unwrap();
    let phrase = Phrase::parse(&words).unwrap();
    let (served, got) = exchange(&m1, address(10), "", |stream| {
        let greeting = joiner::greeting(stream)?;
        stolen_run(stream, &greeting, &request, &j_device, &k_device, &phrase)
    });
    assert!(served.is_err(), "the member refused the possession proof");
    assert!(got.is_err(), "no bundle");
    let pid = phrase_of(&m1);
    assert_eq!(states(&m1), [RowState::Allowed(pid)]);
    assert_eq!(
        m1.lock().unwrap().phrase_state(pid),
        Some(crate::pake::PhraseState::Burned),
        "failed possession proof must burn the phrase rather than leaving it Admitted"
    );
}

/// `device`'s signature of an ask of `member` for `request` under `node`, `name` and `model`.
fn ask_signature(
    device: &Device,
    member: &[u8; 32],
    node: [u8; 32],
    name: &str,
    model: &str,
    request: &crate::commons::binding::Request,
) -> [u8; 64] {
    joiner::ask_signature(device, member, &node, name, model, request.bytes()).unwrap()
}

/// A flood of requests from many addresses, each a fresh Mac, never pushes a row a person
/// allowed off the desk: its phrase stays live, and the Mac it was shown for still gets in.
/// NEGATIVE CONTROL: the eviction this desk had (the oldest row, once no ignored or let-in one
/// is left) drops the allowed row, and that Mac's run is refused ("no join phrase is shown").
#[test]
fn a_flood_of_requests_never_takes_a_row_a_person_allowed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, device) = mac(root.path(), "j");
    let j = joining(&device, [7; 32], "j");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let words = allow(&m1);
    let pid = phrase_of(&m1);
    for n in 0..desk::MAX_HEARD + 8 {
        let name = format!("f{n}");
        let (_, flood) = mac(root.path(), &name);
        let from = IpAddr::from([
            10,
            0,
            u8::try_from(n / 200).unwrap(),
            u8::try_from(n % 200 + 1).unwrap(),
        ]);
        assert_eq!(
            ask(&m1, from, &joining(&flood, [8; 32], &name)),
            Answer::Asking
        );
    }
    let held = m1.lock().unwrap().rows(Instant::now());
    assert_eq!(held.len(), desk::MAX_HEARD, "the desk stays bounded");
    assert!(
        held.iter()
            .any(|row| row.name == "j" && row.state == RowState::Allowed(pid)),
        "the allowed row stays"
    );
    assert_eq!(
        m1.lock().unwrap().phrase_state(pid),
        Some(crate::pake::PhraseState::Live)
    );
    let (served, got) = run(&m1, address(9), &j, &words, &token());
    assert!(got.is_ok(), "J still gets in: {:?}", got.err());
    assert!(served.unwrap().is_some());
}

/// One source's new requests take the place of its own oldest once it holds
/// [`desk::PER_SOURCE`] (an IPv6 source is its /64): a LAN host that sends requests under fresh
/// keys churns only its own rows, so another Mac's row stays for a person to allow.
/// NEGATIVE CONTROL: a desk with no per-source bound drops K's row, the oldest asking one,
/// under the flood.
#[test]
fn one_sources_flood_of_requests_churns_only_its_own_rows() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, device) = mac(root.path(), "k");
    assert_eq!(
        ask(&m1, address(20), &joining(&device, [7; 32], "k")),
        Answer::Asking
    );
    for n in 0..desk::MAX_HEARD + 8 {
        let name = format!("f{n}");
        let (_, flood) = mac(root.path(), &name);
        let from = IpAddr::from([0xfd00, 0, 0, 1, 0, 0, 0, u16::try_from(n + 1).unwrap()]);
        assert_eq!(
            ask(&m1, from, &joining(&flood, [8; 32], &name)),
            Answer::Asking
        );
    }
    let held = m1.lock().unwrap().rows(Instant::now());
    assert!(
        held.iter().any(|row| row.name == "k"),
        "K's row stays: {held:?}"
    );
    assert_eq!(
        held.iter().filter(|row| row.name.starts_with('f')).count(),
        desk::PER_SOURCE,
        "one /64 holds its bound"
    );
}

/// A host with many addresses (one per row) fills the desk with fresh requests, pauses, and
/// floods again. K, which asked in the pause and asked again later as a waiting Mac does, keeps
/// its row through the second flood: a row that asked again goes only once no row that has not
/// is left.
/// NEGATIVE CONTROL: the eviction this desk had (the oldest asking row, once no settled one is
/// left) drops the flood's first rows and then K's ("K's row stays").
#[test]
fn a_flood_from_many_addresses_never_takes_a_row_that_asked_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let flood = |from: usize, to: usize| {
        for n in from..to {
            let name = format!("f{n}");
            let (_, device) = mac(root.path(), &name);
            let at = IpAddr::from([
                10,
                0,
                u8::try_from(n / 200).unwrap(),
                u8::try_from(n % 200 + 1).unwrap(),
            ]);
            assert_eq!(
                ask(&m1, at, &joining(&device, [8; 32], &name)),
                Answer::Asking
            );
        }
    };
    flood(0, desk::MAX_HEARD);

    let (_, device) = mac(root.path(), "k");
    let k = joining(&device, [7; 32], "k");
    let request = k.request(&joiner_greeting(&m1)).unwrap();
    let key = *m1.lock().unwrap().local().key();
    let signature = ask_signature(&device, &key, [7; 32], "k", "Mac15,3", &request);
    let asking = desk::Asking {
        wire: request.bytes(),
        node: [7; 32],
        name: "k",
        model: "Mac15,3",
        signature: &signature,
    };
    let first = Instant::now();
    for now in [first, first + desk::ASKED_AGAIN_AFTER] {
        let heard = m1.lock().unwrap().hear(&asking, address(20), now);
        assert_eq!(heard.unwrap(), Answer::Asking);
    }

    flood(desk::MAX_HEARD, 2 * desk::MAX_HEARD + 8);
    let held = m1.lock().unwrap().rows(Instant::now());
    assert_eq!(held.len(), desk::MAX_HEARD, "the desk stays bounded");
    assert!(
        held.iter().any(|row| row.name == "k"),
        "K's row stays: {held:?}"
    );
}

/// `device`'s ask as `name`, which `desk` hears from `from` at `now` (no connection, so a test
/// sets the clock).
fn hear_at(desk: &Mutex<Desk>, device: &Device, name: &str, from: IpAddr, now: Instant) -> Answer {
    let request = joining(device, [7; 32], name)
        .request(&joiner_greeting(desk))
        .unwrap();
    let key = *desk.lock().unwrap().local().key();
    let signature = ask_signature(device, &key, [7; 32], name, "Mac15,3", &request);
    let asking = desk::Asking {
        wire: request.bytes(),
        node: [7; 32],
        name,
        model: "Mac15,3",
        signature: &signature,
    };
    desk.lock().unwrap().hear(&asking, from, now).unwrap()
}

/// A host with 16 addresses fills the desk (4 rows from each) and asks each of its requests
/// again, as a waiting Mac does. New Mac K then asks once, and the host sends fresh requests,
/// each from an address that holds fewer than [`desk::PER_SOURCE`] rows, so each one makes room
/// on the whole desk. K's row stays: what goes is a row of the addresses that hold the most,
/// and K's holds one. Before, the newest row that had not asked again went, which was K's
/// whenever the host sent its next request.
/// NEGATIVE CONTROLS: choosing among every asking row (no `Desk::crowded`) drops K's row at the
/// host's first fresh request ("K's row stays: [Row { id: 17, name: \"f16\", …"); so does the
/// first form of this fix (the newest row that had not asked again, among every asking row),
/// and the oldest asking row before it does after 63 requests ("K's row stays: [Row { id: 130,
/// name: \"g64\", …").
#[test]
fn a_flood_from_a_few_addresses_never_takes_a_new_macs_row() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let source = |n: usize| IpAddr::from([10, 0, 0, u8::try_from(n % 16 + 1).unwrap()]);
    let first = Instant::now();
    let flood: Vec<(String, Device)> = (0..desk::MAX_HEARD)
        .map(|n| {
            let name = format!("f{n}");
            let (_, device) = mac(root.path(), &name);
            (name, device)
        })
        .collect();
    for now in [first, first + desk::ASKED_AGAIN_AFTER] {
        for (n, (name, device)) in flood.iter().enumerate() {
            assert_eq!(hear_at(&m1, device, name, source(n), now), Answer::Asking);
        }
    }

    let later = first + desk::ASKED_AGAIN_AFTER + Duration::from_secs(1);
    let (_, k) = mac(root.path(), "k");
    assert_eq!(hear_at(&m1, &k, "k", address(20), later), Answer::Asking);
    for n in 0..2 * desk::MAX_HEARD {
        let held = m1.lock().unwrap().rows(later);
        let from = (0..16)
            .map(source)
            .find(|at| held.iter().filter(|row| row.from == *at).count() < desk::PER_SOURCE)
            .unwrap_or_else(|| source(n));
        let name = format!("g{n}");
        let (_, device) = mac(root.path(), &name);
        assert_eq!(hear_at(&m1, &device, &name, from, later), Answer::Asking);
    }
    let held = m1.lock().unwrap().rows(later);
    assert_eq!(held.len(), desk::MAX_HEARD, "the desk stays bounded");
    assert!(
        held.iter().any(|row| row.name == "k"),
        "K's row stays: {held:?}"
    );
}

/// A row follows its Mac (D3): L, which asked first, asked again, then went quiet, lapses
/// [`desk::STILL_ASKING`] after its last ask, so it holds no place ahead of a fresh row, while K,
/// which asks again every 15 minutes, keeps its row and its handle; when L asks again it shows
/// under the handle it had, so its entry is the same one. A row whose phrase can still admit
/// stays past the lapse until the phrase ends.
/// NEGATIVE CONTROLS: rows kept for a day from their first ask (the old `REQUEST_LIFETIME`
/// rule) still show L ("L lapsed: [Row { … name: \"l\" …"); a counter for handles gives L's
/// second row another handle ("L's handle is its request's"); `lapsed` without the live-phrase
/// keep drops J's Allowed row ("J's live phrase keeps its row").
#[test]
fn a_row_lapses_once_its_mac_stops_asking_and_keeps_its_handle() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let first = Instant::now();
    let (_, l) = mac(root.path(), "l");
    let (_, k) = mac(root.path(), "k");
    assert_eq!(hear_at(&m1, &l, "l", address(20), first), Answer::Asking);
    assert_eq!(hear_at(&m1, &k, "k", address(21), first), Answer::Asking);
    let again = first + desk::ASKED_AGAIN_AFTER;
    assert_eq!(hear_at(&m1, &l, "l", address(20), again), Answer::Asking);
    let handle = |name: &str, now: Instant| {
        m1.lock()
            .unwrap()
            .rows(now)
            .into_iter()
            .find(|row| row.name == name)
            .map(|row| row.id)
    };
    let (l_id, k_id) = (handle("l", again).unwrap(), handle("k", again).unwrap());
    for minutes in [15, 30] {
        let now = first + Duration::from_secs(minutes * 60);
        assert_eq!(hear_at(&m1, &k, "k", address(21), now), Answer::Asking);
    }
    let quiet = again + desk::STILL_ASKING;
    let held = m1.lock().unwrap().rows(quiet);
    assert!(
        !held.iter().any(|row| row.name == "l"),
        "L lapsed: {held:?}"
    );
    assert_eq!(
        handle("k", quiet),
        Some(k_id),
        "K, still asking, keeps its row"
    );
    assert_eq!(hear_at(&m1, &l, "l", address(20), quiet), Answer::Asking);
    assert_eq!(
        handle("l", quiet),
        Some(l_id),
        "L's handle is its request's"
    );

    let (_, j) = mac(root.path(), "j");
    assert_eq!(hear_at(&m1, &j, "j", address(22), quiet), Answer::Asking);
    let pressed = quiet + desk::STILL_ASKING - Duration::from_secs(1);
    let j_id = m1
        .lock()
        .unwrap()
        .rows(quiet)
        .into_iter()
        .find(|row| row.name == "j")
        .unwrap()
        .id;
    let phrase = m1.lock().unwrap().allow(j_id, pressed).expect("allowed");
    let past = quiet + desk::STILL_ASKING + Duration::from_secs(1);
    let held = m1.lock().unwrap().rows(past);
    assert!(
        held.iter()
            .any(|row| row.id == j_id && row.state == RowState::Allowed(phrase)),
        "J's live phrase keeps its row: {held:?}"
    );
}

/// Rows heard on a network this Mac left go when it moves (D3): a Mac that asked at home is not
/// at the café to be let in, so its row and the phrase allowed for it end there, while a row
/// heard on the network this Mac is on stays, and a moment with no network to name changes
/// nothing.
/// NEGATIVE CONTROL: `on_network` that only records the network keeps A's row and its phrase
/// across the move (the move's count reads `left: 0`, `right: 1`).
#[test]
fn rows_heard_on_another_network_go_when_this_mac_moves() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let now = Instant::now();
    assert_eq!(m1.lock().unwrap().on_network(Some("home".into())), 0);
    let (_, a) = mac(root.path(), "a");
    assert_eq!(hear_at(&m1, &a, "a", address(30), now), Answer::Asking);
    let a_id = m1.lock().unwrap().rows(now)[0].id;
    let phrase = m1.lock().unwrap().allow(a_id, now).expect("allowed");
    assert_eq!(m1.lock().unwrap().on_network(None), 0);
    assert_eq!(m1.lock().unwrap().on_network(Some("home".into())), 0);
    assert_eq!(
        m1.lock().unwrap().rows(now).len(),
        1,
        "same network, same rows"
    );

    assert_eq!(m1.lock().unwrap().on_network(Some("cafe".into())), 1);
    let held = m1.lock().unwrap().rows(now);
    assert!(held.is_empty(), "A's row went with the move: {held:?}");
    assert!(
        m1.lock().unwrap().words(phrase, now).is_none(),
        "its phrase ended with it"
    );
    let (_, b) = mac(root.path(), "b");
    assert_eq!(hear_at(&m1, &b, "b", address(31), now), Answer::Asking);
    assert_eq!(m1.lock().unwrap().on_network(Some("cafe".into())), 0);
    assert_eq!(m1.lock().unwrap().rows(now).len(), 1, "B was heard here");
}

/// A row closes on any admission this Mac reads (D3): once the Commons roster binds the asking
/// Mac's writer key (an approver enrolled it from another member's `Allowed`), its row here
/// closes, as an `Admit` in the membership log closes it.
/// NEGATIVE CONTROL: `refresh` reading only the log's admissions leaves J's row asking ("J's row
/// closed once the roster bound it").
#[test]
fn a_row_closes_once_the_commons_roster_binds_its_mac() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, m1_device) = mac(root.path(), "m1");
    let m1 = Mutex::new(Desk::new(
        Local::with(m1_device, TRUST, true, "m1").expect("member"),
    ));
    let now = Instant::now();
    let (_, j) = mac(root.path(), "j");
    assert_eq!(hear_at(&m1, &j, "j", address(40), now), Answer::Asking);
    let writer = *j
        .request(TRUST.network, TRUST.authority)
        .unwrap()
        .binding()
        .writer();
    let (_, again) = mac(root.path(), "m1");
    m1.lock().unwrap().refresh(
        Local::with(again, TRUST, true, "m1")
            .expect("member")
            .with_bound(std::iter::once(writer).collect()),
    );
    assert_eq!(
        states(&m1),
        [RowState::Closed],
        "J's row closed once the roster bound it"
    );
}

/// A recipient the vault's signed swarm roster names but no seat of the member's log binds (a
/// seatless recipient, one-mesh S1) asks for a seat: its ask shows a row for a person's Allow
/// rather than closing at once as a member's would, and it closes once the log seats it.
/// NEGATIVE CONTROL: `Local::knows_recipient` reading the roster alone closes S's first ask and
/// the first assert fails.
#[test]
fn a_seatless_recipient_the_roster_names_shows_a_row_until_the_log_seats_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, s) = mac(root.path(), "s");
    let request = s.request(TRUST.network, TRUST.authority).unwrap();
    let recipient = request.binding().recipient().to_bech32();
    let (_, m1_device) = mac(root.path(), "m1");
    let m1_actor = m1_device
        .request(TRUST.network, TRUST.authority)
        .unwrap()
        .binding()
        .actor();
    let local = |device: Device, seats: Vec<crate::membership_log::MemberId>| {
        Local::with(device, TRUST, true, "m1")
            .expect("member")
            .with_automatic_lan([recipient.clone()])
            .with_admitted(seats)
    };
    let m1 = Mutex::new(Desk::new(local(m1_device, vec![m1_actor])));
    let now = Instant::now();
    assert_eq!(
        hear_at(&m1, &s, "s", address(41), now),
        Answer::Asking,
        "a seatless recipient's ask shows a row"
    );
    let (_, again) = mac(root.path(), "m1");
    m1.lock()
        .unwrap()
        .refresh(local(again, vec![m1_actor, request.binding().actor()]));
    assert_eq!(
        states(&m1),
        [RowState::Closed],
        "the log seats S: its row closes"
    );
}

/// A host that copied a Mac's request from its `ask` cannot show it under another node key,
/// name or model: an ask is signed by the request's writer key over them, so a phrase a person
/// allows is bound to the node key the real Mac runs with, under the name it asked for.
/// NEGATIVE CONTROL: a `hear` that skips the ask's signature shows the copy as J's row under
/// node `[9; 32]`, and J's run under its own node key is then refused.
#[test]
fn a_copied_request_shows_only_as_the_mac_that_signed_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, j_device) = mac(root.path(), "j");
    let (_, k_device) = mac(root.path(), "k");
    let j = joining(&j_device, [7; 32], "j");
    let request = j.request(&joiner_greeting(&m1)).unwrap();
    let key = *m1.lock().unwrap().local().key();
    let genuine = ask_signature(&j_device, &key, [7; 32], "j", "Mac15,3", &request);
    for (node, name, signature, why) in [
        (
            [9; 32],
            "j",
            ask_signature(&k_device, &key, [9; 32], "j", "Mac15,3", &request),
            "signed by another key",
        ),
        (
            [9; 32],
            "j",
            genuine,
            "J's signature under another node key",
        ),
        ([7; 32], "m22", genuine, "J's signature under another name"),
    ] {
        let asking = desk::Asking {
            wire: request.bytes(),
            node,
            name,
            model: "Mac15,3",
            signature: &signature,
        };
        let heard = m1
            .lock()
            .unwrap()
            .hear(&asking, address(10), Instant::now());
        assert!(heard.is_err(), "{why}");
    }
    assert!(m1.lock().unwrap().rows(Instant::now()).is_empty());
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let words = allow(&m1);
    let (served, got) = run(&m1, address(9), &j, &words, &token());
    assert!(got.is_ok(), "{:?}", got.err());
    assert!(served.unwrap().is_some());
}

/// When two answers both say a person allowed this Mac (a host that copied a member's greeting,
/// its name and key included, or two members of one name), the joiner's refusal names each by
/// the address `--at` takes, and `--member` takes an address, a host or a key prefix where the
/// names are the same.
/// NEGATIVE CONTROL: the `pick` this joiner had lists the two as "m7, m7" with no address and
/// no `--at`, and finds no member for `--member 10.0.0.66:7100`.
#[test]
fn a_joiner_shown_several_phrases_is_told_each_ones_address() {
    use super::join::{self, Heard};
    let at = |at: &str, key: u8| Heard {
        at: at.into(),
        greeting: joiner::Greeting {
            key: [key; 32],
            network: TRUST.network,
            authority: TRUST.authority,
            genesis: TRUST.genesis,
            name: "m7".into(),
            can_allow: true,
            invite: false,
            challenge: [0; desk::CHALLENGE_LEN],
        },
        answer: Answer::Allowed,
        signed: joiner::Signed::Unsigned,
    };
    let heard = [at("10.0.0.7:7100", 0xab), at("10.0.0.66:7100", 0xcd)];
    for member in [None, Some("m7")] {
        let why = join::pick(&heard, member).unwrap_err().to_string();
        assert!(
            why.contains("m7 at 10.0.0.7:7100")
                && why.contains("m7 at 10.0.0.66:7100")
                && why.contains("--at <host:port>"),
            "{why}"
        );
    }
    let picked = |member| join::pick(&heard, Some(member)).map(|heard| heard.at.clone());
    assert_eq!(picked("10.0.0.66:7100").unwrap(), "10.0.0.66:7100");
    assert_eq!(picked("10.0.0.7").unwrap(), "10.0.0.7:7100");
    let v6_heard = [at("[fd00::7]:7100", 0xef), at("10.0.0.66:7100", 0xcd)];
    assert_eq!(
        join::pick(&v6_heard, Some("fd00::7")).unwrap().at,
        "[fd00::7]:7100"
    );
    assert_eq!(
        join::pick(&v6_heard, Some("[fd00::7]")).unwrap().at,
        "[fd00::7]:7100"
    );
    assert_eq!(picked("ABABABAB").unwrap(), "10.0.0.7:7100", "a key prefix");
    assert!(
        picked("abab").is_err(),
        "a prefix under 8 digits names no one"
    );
    let why = picked("m9").unwrap_err().to_string();
    assert!(why.contains("--at <host:port>"), "{why}");
}

fn joiner_greeting(desk: &Mutex<Desk>) -> joiner::Greeting {
    let desk = desk.lock().unwrap();
    let local = desk.local();
    joiner::Greeting {
        key: *local.key(),
        network: local.trust().network,
        authority: local.trust().authority,
        genesis: local.trust().genesis,
        name: local.name().to_owned(),
        can_allow: true,
        invite: false,
        challenge: [0; desk::CHALLENGE_LEN],
    }
}

fn phrase_of(desk: &Mutex<Desk>) -> crate::pake::PhraseId {
    match states(desk)[0] {
        RowState::Allowed(phrase) => phrase,
        other => panic!("{other:?}"),
    }
}

/// J's side of a run, by a Mac that holds the phrase and J's public request, signs the run with
/// `signer`'s writer key, and answers the possession challenge with `device`.
fn stolen_run(
    stream: &mut UnixStream,
    greeting: &joiner::Greeting,
    request: &crate::commons::binding::Request,
    signer: &Device,
    device: &Device,
    phrase: &Phrase,
) -> Result<SecretBytes> {
    use super::wire::{self, Line};
    let ci = crate::pake::ChannelId::new(
        &greeting.key,
        request.binding(),
        &[7; 32],
        &greeting.genesis,
    );
    let (start, nonce) = crate::pake::Joiner::start(phrase, ci)?;
    let signature = joiner::sign(
        signer,
        &desk::run_message(
            &greeting.key,
            &greeting.challenge,
            &nonce,
            &[7; 32],
            request.bytes(),
        ),
    )?;
    wire::write_line(
        stream,
        &format!(
            "run request={} node={} name=j nonce={} sig={}",
            wire::b64(request.bytes()),
            wire::hex(&[7; 32]),
            wire::hex(&nonce),
            wire::hex(signature.as_ref())
        ),
    )?;
    let line = wire::read_line(stream)?;
    let (shared, ours) = start.on_nonce(&wire::answer(&line)?.hex("nonce")?)?;
    wire::write_line(stream, &format!("share y={}", wire::hex(&ours)))?;
    let line = wire::read_line(stream)?;
    let (confirming, tag) = shared.on_share(&wire::answer(&line)?.hex("y")?)?;
    wire::write_line(stream, &format!("confirm t={}", wire::hex(&tag)))?;
    let line = wire::read_line(stream)?;
    let parsed: Line<'_> = wire::answer(&line)?;
    let session = confirming.on_confirm(&parsed.hex("t")?)?;
    let own = device.request(greeting.network, greeting.authority)?;
    let response = device
        .answer(&own, &parsed.b64("challenge")?)
        .unwrap_or_else(|_| vec![0; crate::commons::binding::RESPONSE_LEN]);
    wire::write_line(
        stream,
        &format!("possess response={}", wire::b64(&response)),
    )?;
    let line = wire::read_line(stream)?;
    let size: usize = wire::answer(&line)?.get("bundle")?.parse().unwrap();
    let mut sealed = vec![0; size];
    std::io::Read::read_exact(stream, &mut sealed)?;
    session.open_bundle(&sealed)
}

/// One connection from `from` that sends `run` for J's copied `request` and node `[7; 32]`,
/// signed as `sign` says over the challenge this connection's greeting carries (no `sig` field
/// when it says `None`), then hangs up after the member's first answer: that answer.
fn copied_run(
    desk: &Mutex<Desk>,
    from: IpAddr,
    request: &crate::commons::binding::Request,
    sign: impl FnOnce(&[u8; desk::CHALLENGE_LEN], &crate::pake::Nonce) -> Option<Vec<u8>>,
) -> String {
    use super::wire;
    let (_, said) = exchange(desk, from, "", |stream| {
        let greeting = joiner::greeting(stream)?;
        let nonce = [5; crate::pake::NONCE_LEN];
        let sig = sign(&greeting.challenge, &nonce)
            .map(|sig| format!(" sig={}", wire::hex(&sig)))
            .unwrap_or_default();
        wire::write_line(
            stream,
            &format!(
                "run request={} node={} name=j nonce={}{sig}",
                wire::b64(request.bytes()),
                wire::hex(&[7; 32]),
                wire::hex(&nonce)
            ),
        )?;
        wire::read_line(stream)
    });
    said.expect("the member answered")
}

/// A LAN host that copied J's request and node key from J's `ask` (J sends both to every
/// `_alab._tcp` host it browses) cannot spend the phrase a person Allowed for J: a run it signs
/// with its own key, one it sends unsigned, and J's own signature taken on another connection
/// are each refused before the phrase is touched, and J's run then gets its bundle.
/// NEGATIVE CONTROL: a `Desk::begin` that skips the run's signature check lets the copy's run
/// take the phrase, and its hang-up burns it, so J's run is refused.
#[test]
fn a_mac_that_copied_the_request_cannot_spend_the_phrase_allowed_for_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, j_device) = mac(root.path(), "j");
    let (_, k_device) = mac(root.path(), "k");
    let j = joining(&j_device, [7; 32], "j");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let words = allow(&m1);
    let pid = phrase_of(&m1);
    let request = j.request(&joiner_greeting(&m1)).unwrap();
    let key = *m1.lock().unwrap().local().key();
    let signed_by =
        |device: &Device, challenge: &[u8; desk::CHALLENGE_LEN], nonce: &crate::pake::Nonce| {
            let message = desk::run_message(&key, challenge, nonce, &[7; 32], request.bytes());
            joiner::sign(device, &message).unwrap().as_ref().to_vec()
        };
    let own = copied_run(&m1, address(10), &request, |challenge, nonce| {
        Some(signed_by(&k_device, challenge, nonce))
    });
    assert!(
        own.starts_with("NO "),
        "signed by the copy's own key: {own}"
    );
    let unsigned = copied_run(&m1, address(11), &request, |_, _| None);
    assert!(unsigned.starts_with("NO "), "unsigned: {unsigned}");
    // J's signature over the challenge of a connection J saw (a fake member that relayed it),
    // replayed on a new one.
    let (_, seen) = exchange(&m1, address(12), "", |stream| {
        joiner::greeting(stream).map(|greeting| greeting.challenge)
    });
    let seen = seen.unwrap();
    let replayed = copied_run(&m1, address(12), &request, |_, nonce| {
        Some(signed_by(&j_device, &seen, nonce))
    });
    assert!(replayed.starts_with("NO "), "replayed: {replayed}");
    assert_eq!(
        m1.lock().unwrap().phrase_state(pid),
        Some(crate::pake::PhraseState::Live),
        "no copied run touched the phrase"
    );
    let value = token();
    let (served, got) = run(&m1, address(9), &j, &words, &value);
    assert!(got.is_ok(), "J gets in: {:?}", got.err());
    assert!(served.unwrap().is_some());
}

#[test]
fn a_fourth_run_from_one_address_in_a_minute_is_refused() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, device) = mac(root.path(), "j");
    let j = joining(&device, [7; 32], "j");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let words = allow(&m1);
    let wrong = loop {
        let other = Phrase::generate().unwrap().words().join(" ");
        if other != words {
            break other;
        }
    };
    for _ in 0..3 {
        let (served, _) = run(&m1, address(9), &j, &wrong, "");
        let why = served.err().expect("refused").to_string();
        assert!(!why.contains("too many"), "{why}");
    }
    let (served, _) = run(&m1, address(9), &j, &words, "");
    let why = served.err().expect("refused").to_string();
    assert!(why.contains("too many join runs"), "{why}");
    // IPv4-mapped IPv6 for the same IPv4 source shares its rate-limit bucket.
    let mapped_9 = IpAddr::from([0, 0, 0, 0, 0, 0xffff, 0xc000, 0x0209]);
    let (served, _) = run(&m1, mapped_9, &j, &words, "");
    let why = served.err().expect("refused").to_string();
    assert!(why.contains("too many join runs"), "{why}");
    // Different interface identifiers inside one IPv6 /64 share one rate-limit bucket.
    for host in 1u16..=3 {
        let v6 = IpAddr::from([0xfd00, 0, 0, 1, 0, 0, 0, host]);
        let (served, _) = run(&m1, v6, &j, &wrong, "");
        let why = served.err().expect("refused").to_string();
        assert!(!why.contains("too many"), "{why}");
    }
    let v6_fourth = IpAddr::from([0xfd00, 0, 0, 1, 0, 0, 0, 4]);
    let (served, _) = run(&m1, v6_fourth, &j, &words, "");
    let why = served.err().expect("refused").to_string();
    assert!(why.contains("too many join runs"), "{why}");
    // Direct `Desk::hear` also enforces the 64-byte `model` bound.
    let over_model = "M".repeat(65);
    let req_bytes = j.request(&joiner_greeting(&m1)).unwrap().bytes().to_vec();
    let mut desk = m1.lock().unwrap();
    let err = desk
        .hear(
            &desk::Asking {
                wire: &req_bytes,
                node: [7; 32],
                name: "j",
                model: &over_model,
                signature: &[0; 64],
            },
            address(10),
            Instant::now(),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("no model"), "{err}");
}

#[test]
fn a_member_with_no_roster_binding_shows_the_row_and_cannot_allow() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, device) = mac(root.path(), "m1");
    let m1 = Mutex::new(Desk::new(Local::with(device, TRUST, false, "m1").unwrap()));
    let (_, j_device) = mac(root.path(), "j");
    let j = joining(&j_device, [7; 32], "j");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let mut desk = m1.lock().unwrap();
    let row = desk.rows(Instant::now())[0].id;
    let why = desk.allow(row, Instant::now()).unwrap_err().to_string();
    assert!(why.contains("no Commons binding"), "{why}");
    assert!(desk.invite(Instant::now()).is_err());
}

#[test]
fn refreshing_without_a_roster_binding_or_on_a_new_network_cancels_live_phrases() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, j_device) = mac(root.path(), "j");
    let j = joining(&j_device, [7; 32], "j");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let (words, allowed_id, invite_id) = {
        let mut desk = m1.lock().unwrap();
        let now = Instant::now();
        let row = desk.rows(now)[0].id;
        let allowed_id = desk.allow(row, now).unwrap();
        let words = desk.words(allowed_id, now).unwrap().join(" ");
        let invite_id = desk.invite(now).unwrap();
        (words, allowed_id, invite_id)
    };
    let (_, unbound_device) = mac(root.path(), "m1");
    m1.lock()
        .unwrap()
        .refresh(Local::with(unbound_device, TRUST, false, "m1").unwrap());
    assert_eq!(states(&m1), [RowState::Asking]);
    assert_eq!(
        m1.lock().unwrap().phrase_state(allowed_id),
        Some(crate::pake::PhraseState::Burned)
    );
    assert_eq!(
        m1.lock().unwrap().phrase_state(invite_id),
        Some(crate::pake::PhraseState::Burned)
    );
    let (served, got) = run(&m1, address(9), &j, &words, "");
    assert!(served.is_err() && got.is_err());

    let other_trust = Trust {
        network: [9; 16],
        ..TRUST
    };
    let (_, other_device) = mac(root.path(), "m1");
    m1.lock()
        .unwrap()
        .refresh(Local::with(other_device, other_trust, true, "m1").unwrap());
    assert!(m1.lock().unwrap().rows(Instant::now()).is_empty());
}

/// A run the node job relays to the window's desk gets its bundle, and the relay passes the new
/// Mac's acknowledgement back, so the row closes.
///
/// NEGATIVE CONTROL: `relay` returning after the bundle without passing the acknowledgement on
/// (`if acks && false`): `left: Unacknowledged("the other Mac closed the join exchange")`,
/// `right: Acknowledged`.
#[test]
fn a_run_relayed_between_the_node_job_and_the_window_gets_its_bundle() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, device) = mac(root.path(), "j");
    let j = joining(&device, [7; 32], "j");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let words = allow(&m1);
    let value = token();
    let phrase = Phrase::parse(&words).unwrap();
    let (mut window, mut window_end) = UnixStream::pair().unwrap();
    let (mut joiner_end, mut node_end) = UnixStream::pair().unwrap();
    for end in [&window, &window_end, &joiner_end, &node_end] {
        end.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    }
    let shared = &m1;
    let got = std::thread::scope(|scope| {
        let bundle = bundle_of(&value);
        let served = scope.spawn(move || desk::serve(shared, &mut window, address(9), &bundle));
        let relayed = scope.spawn(move || super::relay(&mut node_end, &mut window_end));
        let greeting = joiner::greeting(&mut joiner_end).unwrap();
        let got = joiner::run(&mut joiner_end, &greeting, &j, &phrase);
        let let_in = served
            .join()
            .unwrap()
            .unwrap()
            .expect("a run that sent its bundle");
        assert_eq!(
            let_in.delivered,
            desk::Delivered::Acknowledged,
            "the relay passes the acknowledgement on"
        );
        relayed.join().unwrap().unwrap();
        got
    });
    assert_eq!(states(&m1), [RowState::Closed]);
    let bundle = got.expect("the bundle through the relay");
    assert_eq!(
        text::parse_locker(bundle.files().expect("files").as_slice()).unwrap()[0]
            .value
            .as_slice(),
        value.as_bytes()
    );
}

#[test]
fn a_removed_mac_cannot_burn_a_live_invite_phrase_and_refresh_cancels_newly_removed_allows() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, m1_device) = mac(root.path(), "m1");
    let (_, removed_device) = mac(root.path(), "removed");
    let (_, honest_device) = mac(root.path(), "honest");
    let removed_rec = removed_device
        .request(TRUST.network, TRUST.authority)
        .unwrap()
        .binding()
        .recipient()
        .to_bech32();
    let removed_joining = joining(&removed_device, [0x44; 32], "removed");
    let honest_joining = joining(&honest_device, [0x55; 32], "honest");

    let mut notes = crate::client::RemovedNotes::default();
    notes.by_recipient.insert(
        removed_rec.clone(),
        crate::client::RemovedNote {
            label: "m44".to_owned(),
            by: vec!["m1".to_owned()],
            on_ms: 1_700_000_000_000,
            quorum: false,
        },
    );
    let m1 = Mutex::new(Desk::new(
        Local::with(m1_device, TRUST, true, "m1")
            .unwrap()
            .with_removed(notes),
    ));
    let (invite_id, words) = {
        let mut desk = m1.lock().unwrap();
        let now = Instant::now();
        let id = desk.invite(now).unwrap();
        let words = desk.words(id, now).unwrap().join(" ");
        (id, words)
    };

    // 1. Removed Mac attempts a run while an invite phrase is live: refused before touching the
    //    invite phrase, leaving the invite `Live` for the honest Mac.
    let (served_rem, got_rem) = run(&m1, address(44), &removed_joining, &words, "");
    assert!(served_rem.is_err() && got_rem.is_err());
    assert!(
        got_rem.unwrap_err().to_string().contains("Allow again"),
        "removed Mac must be told Allow again is required"
    );
    assert_eq!(
        m1.lock().unwrap().phrase_state(invite_id),
        Some(crate::pake::PhraseState::Live),
        "removed Mac must not burn a live invite phrase"
    );

    // Honest Mac can still use the same invite phrase and receive its bundle.
    let value = token();
    let (served_ok, got_ok) = run(&m1, address(55), &honest_joining, &words, &value);
    assert!(served_ok.unwrap().is_some());
    assert!(got_ok.is_ok());

    // 2. Refreshing a desk when an allowed Mac becomes quorum-removed cancels its phrase.
    assert_eq!(ask(&m1, address(44), &removed_joining), Answer::Asking);
    let allow_again_id = {
        let mut desk = m1.lock().unwrap();
        let now = Instant::now();
        let row = desk
            .rows(now)
            .into_iter()
            .find(|r| r.name == "removed")
            .unwrap()
            .id;
        desk.allow_again(row, now).unwrap()
    };
    let mut quorum_notes = crate::client::RemovedNotes::default();
    quorum_notes.by_recipient.insert(
        removed_rec,
        crate::client::RemovedNote {
            label: "m44".to_owned(),
            by: vec!["m1".to_owned(), "m2".to_owned()],
            on_ms: 1_700_000_000_000,
            quorum: true,
        },
    );
    let (_, m1_device2) = mac(root.path(), "m1");
    m1.lock().unwrap().refresh(
        Local::with(m1_device2, TRUST, true, "m1")
            .unwrap()
            .with_removed(quorum_notes),
    );
    assert_eq!(
        m1.lock().unwrap().phrase_state(allow_again_id),
        Some(crate::pake::PhraseState::Burned),
        "quorum removal on refresh must cancel an active Allow again phrase"
    );
}

/// A node key's record signature of a proof, as `NodeIdentity::sign_record(PROVE_RECORD, ..)`
/// makes it.
fn node_signer(seed: u8) -> (impl Fn(&[u8]) -> [u8; 64] + Sync, [u8; 32]) {
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    let pair = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("node key");
    let key: [u8; 32] = pair.public_key().as_ref().try_into().expect("32 bytes");
    let prefix = format!("aterm-fleet/v3 record {}\0", desk::PROVE_RECORD);
    let sign = move |message: &[u8]| -> [u8; 64] {
        pair.sign(&[prefix.as_bytes(), message].concat())
            .as_ref()
            .try_into()
            .expect("64 bytes")
    };
    (sign, key)
}

/// One `prove` connection: the member serves it with `proof` (its `decide` hands
/// [`files_bundle`] signed by `signer`, the member's own device) while `joiner` speaks.
fn proving<T>(
    desk: &Mutex<Desk>,
    fleet: Option<desk::Fleet>,
    decide: &(dyn Fn(&desk::Proving<'_>) -> Result<SecretBytes> + Sync),
    joiner: impl FnOnce(&mut UnixStream) -> T,
) -> (Result<Option<desk::Let>>, T) {
    proving_checked(desk, (fleet, address(5)), &|_| None, decide, joiner)
}

/// [`proving`] from `from`, the member's check before possession `refuses`.
fn proving_checked<T>(
    desk: &Mutex<Desk>,
    (fleet, from): (Option<desk::Fleet>, IpAddr),
    refuses: &(dyn Fn(&desk::Proving<'_>) -> Option<String> + Sync),
    decide: &(dyn Fn(&desk::Proving<'_>) -> Result<SecretBytes> + Sync),
    joiner: impl FnOnce(&mut UnixStream) -> T,
) -> (Result<Option<desk::Let>>, T) {
    let (mut ours, mut theirs) = UnixStream::pair().expect("pair");
    for end in [&ours, &theirs] {
        end.set_read_timeout(Some(Duration::from_secs(60)))
            .expect("timeout");
    }
    std::thread::scope(|scope| {
        let bundle = bundle_of("");
        let served = scope.spawn(move || {
            desk::serve_with(
                desk,
                &mut ours,
                from,
                &bundle,
                Some(desk::Proof {
                    decide,
                    refuses,
                    authority: None,
                    fleet,
                }),
            )
        });
        let said = joiner(&mut theirs);
        drop(theirs);
        (served.join().expect("member"), said)
    })
}

/// The member's `decide` for these tests: [`files_bundle`] holding `value`, its `Allowed`
/// signed by the member's device at `paths`.
fn hands(paths: &Paths, value: &str) -> impl Fn(&desk::Proving<'_>) -> Result<SecretBytes> + Sync {
    let paths = paths.clone();
    let value = value.to_owned();
    move |proving| {
        let device = Device::open(&paths)?.expect("member device");
        let allowed = device
            .with_seed(|seed| Allowed::sign(proving.request.binding(), proving.name, seed))?;
        files_bundle(&allowed, &value)
    }
}

/// Deliver the possession line, then lose the flush result after the member decided it.
struct FailsPossessionFlush {
    inner: UnixStream,
    possessing: bool,
    decided: std::sync::mpsc::Receiver<()>,
}

impl std::io::Read for FailsPossessionFlush {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.inner, bytes)
    }
}

impl std::io::Write for FailsPossessionFlush {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.possessing |= bytes.starts_with(b"possess ");
        std::io::Write::write(&mut self.inner, bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.inner)?;
        if self.possessing {
            self.decided
                .recv_timeout(Duration::from_secs(60))
                .expect("the member decided the delivered possession proof");
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "lost possession flush result",
            ));
        }
        Ok(())
    }
}

#[test]
fn a_failed_possession_flush_cannot_send_the_mac_to_a_second_member() {
    use super::join::{self, Stream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = aterm_tempfile::tempdir().unwrap();
    let desk = member(root.path(), "member");
    let (paths, device) = mac(root.path(), "newcomer");
    let (sign_node, node) = node_signer(0x36);
    let joining = joining(&device, node, "newcomer");
    let dials = AtomicUsize::new(0);
    let decisions = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        let dial = |at: &str| -> Result<Box<dyn Stream>> {
            dials.fetch_add(1, Ordering::SeqCst);
            if at != "a-member:7" {
                return Err(crate::Error::new("a second member was dialed"));
            }
            let (mut ours, theirs) = UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(Duration::from_secs(60)))?;
                end.set_write_timeout(Some(Duration::from_secs(60)))?;
            }
            let (decided, heard) = std::sync::mpsc::channel();
            let (desk, decisions) = (&desk, &decisions);
            scope.spawn(move || {
                let decide = |_: &desk::Proving<'_>| {
                    decisions.fetch_add(1, Ordering::SeqCst);
                    decided.send(()).expect("joiner waits for the decision");
                    Err(crate::Error::new(
                        "decision reached before the flush failed",
                    ))
                };
                let result = desk::serve_with(
                    desk,
                    &mut ours,
                    address(5),
                    &|_| Err(crate::Error::new("no phrase here")),
                    Some(desk::Proof {
                        decide: &decide,
                        refuses: &|_| None,
                        authority: None,
                        fleet: None,
                    }),
                );
                assert!(result.is_err(), "the test decision refuses its bundle");
            });
            Ok(Box::new(FailsPossessionFlush {
                inner: theirs,
                possessing: false,
                decided: heard,
            }))
        };
        let missed = join::prove_first(
            &paths,
            &dial,
            &joining,
            &sign_node,
            &["a-member:7".into(), "b-member:7".into()],
        )
        .expect_err("the possession flush failed");
        assert_eq!(decisions.load(Ordering::SeqCst), 1);
        assert!(missed.signed_maybe, "{missed:?}");
        assert_eq!(dials.load(Ordering::SeqCst), 1, "{missed:?}");
        assert_eq!(missed.refused.len(), 1);
        assert!(missed.refused[0].1.contains("lost possession flush result"));
    });
}

/// A Mac that signs this connection's challenge with its writer key and its fleet node key, and
/// proves its age identity, gets the bundle the member's `decide` hands, sealed to it; `decide`
/// runs with the desk's lock free (the vault's state lock and the log are taken there, and a
/// window's desk must not wait on them).
///
/// NEGATIVE CONTROL: `prove` holding the desk's lock across `(proof.decide)(..)` (taken just
/// before it, dropped just after): the hook's `try_lock` fails and the "the desk's lock is free
/// while decide runs" assert fails.
#[test]
fn a_mac_that_proves_its_node_key_gets_the_bundle_sealed_to_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (m_paths, device) = mac(root.path(), "m");
    let desk = Mutex::new(Desk::new(
        Local::with(device, TRUST, true, "m").expect("member"),
    ));
    let (_, j_device) = mac(root.path(), "j");
    let (sign_node, node) = node_signer(0x31);
    let value = token();
    let handed = hands(&m_paths, &value);
    let free = std::sync::atomic::AtomicBool::new(false);
    let seen = Mutex::new(None);
    let decide = |proving: &desk::Proving<'_>| {
        free.store(desk.try_lock().is_ok(), std::sync::atomic::Ordering::SeqCst);
        *seen.lock().unwrap() = Some((proving.node, proving.fleet, proving.name.to_owned()));
        handed(proving)
    };
    let j = joining(&j_device, node, "m7");
    let (served, got) = proving(&desk, None, &decide, |stream| {
        let greeting = joiner::greeting(stream)?;
        joiner::prove(stream, &greeting, &j, &sign_node, &mut false)
    });
    served.expect("the member served the proof");
    let bundle = got.expect("the Mac took its bundle");
    let entries = text::parse_locker(bundle.files().expect("files").as_slice()).unwrap();
    assert_eq!(entries[0].value.as_slice(), value.as_bytes());
    assert!(
        free.load(std::sync::atomic::Ordering::SeqCst),
        "the desk's lock is free while decide runs"
    );
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some((node, false, "m7".to_owned()))
    );
}

/// A proof that comes from another host on the member's home ALab LAN reaches `decide` marked
/// `lan`, which lets a new Mac in for being there. One from a private address off the member's
/// home subnet (another interface's network, a phone hotspot, a VM bridge) is not marked, nor is
/// any proof to the same member on no home ALab LAN, so a Mac with no seat and no fleet key gets
/// nothing there.
///
/// NEGATIVE CONTROLS: `prove` setting `lan: false` in place of
/// `held.local.automatically_admits(from)`: the home-LAN proof reaches `decide` unmarked and the
/// first assert fails. `Local::automatically_admits` taking every private source
/// (`self.automatic_lan && matches!(from, IpAddr::V4(ip) if ip.is_private())`): the proof from
/// `10.0.0.9` reaches `decide` marked and the "off the home subnet" assert fails. It taking any
/// source while the member has no home (`self.home.is_none_or(…)`): the "no home ALab LAN"
/// assert fails for `192.168.1.40`.
#[test]
fn a_proof_from_the_home_lan_reaches_decide_marked_lan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (m_paths, _) = mac(root.path(), "m");
    let (_, j_device) = mac(root.path(), "j");
    let (sign_node, node) = node_signer(0x32);
    let j = joining(&j_device, node, "m7");
    let value = token();
    let handed = hands(&m_paths, &value);
    // A fresh desk of the same member (its `decide` signs as it) for each proof, at home on the
    // test home network (`HomeNet::test`, 192.168.1.20/24) or on none.
    let lan_of = |at_home: bool, from: IpAddr| {
        let device = Device::open(&m_paths).unwrap().expect("member device");
        let local = Local::with(device, TRUST, true, "m")
            .expect("member")
            .with_automatic_lan([]);
        let desk = Mutex::new(Desk::new(if at_home {
            local
        } else {
            local.with_home(None)
        }));
        let seen = Mutex::new(None);
        let decide = |proving: &desk::Proving<'_>| {
            *seen.lock().unwrap() = Some(proving.lan);
            handed(proving)
        };
        let (served, got) = proving_checked(&desk, (None, from), &|_| None, &decide, |stream| {
            let greeting = joiner::greeting(stream)?;
            joiner::prove(stream, &greeting, &j, &sign_node, &mut false)
        });
        served.expect("the member served the proof");
        got.expect("the Mac took its bundle");
        seen.into_inner().unwrap().expect("decide ran")
    };
    let on_home = IpAddr::from([192, 168, 1, 40]);
    assert!(
        lan_of(true, on_home),
        "a proof from the member's home ALab LAN is marked lan"
    );
    for off in [
        [10, 0, 0, 9],
        [172, 20, 0, 5],
        [172, 20, 10, 5],
        [192, 168, 64, 3],
    ] {
        let from = IpAddr::from(off);
        assert!(!lan_of(true, from), "off the home subnet: {from}");
    }
    for from in [on_home, IpAddr::from([10, 0, 0, 9])] {
        assert!(
            !lan_of(false, from),
            "a member on no home ALab LAN marks no proof lan: {from}"
        );
    }
}

/// What the check before possession read of the vault on a connection reaches the decision after
/// it on that connection ([`desk::Proving::fresh`]), so one exchange reads the vault's newest
/// records once, inside the fleet listener's handshake watchdog; a connection whose check read
/// nothing hands its decision no read.
///
/// NEGATIVE CONTROL: `prove` handing `decide` a fresh `Cell` of its own in place of the one
/// `refuses` saw: the decision reads `false` and the first assert fails.
#[test]
fn the_check_before_possession_hands_its_read_to_the_decision() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (m_paths, device) = mac(root.path(), "m");
    let desk = Mutex::new(Desk::new(
        Local::with(device, TRUST, true, "m").expect("member"),
    ));
    let (_, j_device) = mac(root.path(), "j");
    let (sign_node, node) = node_signer(0x33);
    let j = joining(&j_device, node, "m7");
    let value = token();
    let handed = hands(&m_paths, &value);
    let fresh_of = |reads: bool| {
        let seen = Mutex::new(None);
        let refuses = |proving: &desk::Proving<'_>| {
            if reads {
                proving.fresh.set(true);
            }
            None
        };
        let decide = |proving: &desk::Proving<'_>| {
            *seen.lock().unwrap() = Some(proving.fresh.get());
            handed(proving)
        };
        let (served, got) =
            proving_checked(&desk, (None, address(5)), &refuses, &decide, |stream| {
                let greeting = joiner::greeting(stream)?;
                joiner::prove(stream, &greeting, &j, &sign_node, &mut false)
            });
        served.expect("the member served the proof");
        got.expect("the Mac took its bundle");
        seen.into_inner().unwrap().expect("decide ran")
    };
    assert!(fresh_of(true), "the check's read reaches the decision");
    assert!(!fresh_of(false), "a check that read nothing hands no read");
}

/// A proof proves nothing on another connection or for another Mac: a `prove` line replayed on a
/// new connection (a new challenge) is refused before any possession challenge, and a Mac that
/// names another Mac's node key, with that key's signature from an earlier proof, is refused too.
///
/// NEGATIVE CONTROLS: `prove_message` without the challenge: the replayed line passes and the
/// member answers `OK challenge=`, so the "refused at the prove line" assert fails. The node
/// key's signature check cut from `begin_proof`: the borrowing Mac answers its own possession
/// challenge and takes a bundle, and the "a borrowed node key" assert fails.
#[test]
fn a_replayed_or_borrowed_proof_proves_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (m_paths, device) = mac(root.path(), "m");
    let desk = Mutex::new(Desk::new(
        Local::with(device, TRUST, true, "m").expect("member"),
    ));
    let (_, j_device) = mac(root.path(), "j");
    let (sign_node, node) = node_signer(0x32);
    let decide = hands(&m_paths, "v");
    let j = joining(&j_device, node, "m7");
    // Record the honest Mac's prove line as a LAN host would see it.
    let recorded = Mutex::new(String::new());
    let (served, _) = proving(&desk, None, &decide, |stream| {
        let greeting = joiner::greeting(stream)?;
        let mut tee = Tee {
            inner: stream,
            first: &recorded,
        };
        joiner::prove(&mut tee, &greeting, &j, &sign_node, &mut false)
    });
    served.expect("the honest proof");
    let line = recorded.lock().unwrap().clone();
    assert!(line.starts_with("prove "), "{line}");

    let (served, answer) = proving(&desk, None, &decide, |stream| {
        joiner::greeting(stream)?;
        super::wire::write_line(stream, line.trim_end())?;
        super::wire::read_line(stream)
    });
    let answer = answer.expect("an answer");
    assert!(
        answer.starts_with("NO"),
        "a replayed prove line is refused at the prove line: {answer}"
    );
    assert!(served.is_err());

    // Another Mac names `node` with the signature `node` made for the honest Mac's proof.
    let (_, other) = mac(root.path(), "x");
    let x = joining(&other, node, "m7");
    let honest_sig = super::wire::Line::parse(&line)
        .hex::<64>("nodesig")
        .expect("nodesig");
    let borrowed = move |_: &[u8]| honest_sig;
    let (served, got) = proving(&desk, None, &decide, |stream| {
        let greeting = joiner::greeting(stream)?;
        joiner::prove(stream, &greeting, &x, &borrowed, &mut false)
    });
    assert!(
        served.is_err() && got.is_err(),
        "a borrowed node key proves nothing"
    );
}

/// A prove line naming a node key of small order proves no node key: the identity's forgery,
/// which ring alone takes for every message, is refused at the prove line, and the Mac takes no
/// bundle.
///
/// NEGATIVE CONTROL: `Desk::begin_proof` checking the node key's signature through ring's
/// `UnparsedPublicKey`: the desk serves the proof to its end, refusing nothing.
#[test]
fn a_small_order_node_key_proves_nothing_at_the_desk() {
    use crate::ed25519::{FORGED, SMALL_ORDER_KEY};
    let root = aterm_tempfile::tempdir().unwrap();
    let (m_paths, device) = mac(root.path(), "m");
    let desk = Mutex::new(Desk::new(
        Local::with(device, TRUST, true, "m").expect("member"),
    ));
    let (_, j_device) = mac(root.path(), "j");
    let decide = hands(&m_paths, "v");
    let j = joining(&j_device, SMALL_ORDER_KEY, "m7");
    let forge = |_: &[u8]| FORGED;
    let (served, got) = proving(&desk, None, &decide, |stream| {
        let greeting = joiner::greeting(stream)?;
        joiner::prove(stream, &greeting, &j, &forge, &mut false)
    });
    let refused = served.map(|_| ()).unwrap_err().to_string();
    assert!(
        refused.contains("not signed by the node key it names"),
        "{refused}"
    );
    assert!(got.is_err(), "a small-order node key took a bundle");
}

/// A member's answer signed under a greeting key of small order proves nothing: the identity's
/// forgery, which ring alone takes for every message, is no signed answer.
///
/// NEGATIVE CONTROL: `joiner::ask_checked` through ring's `UnparsedPublicKey`: the forged answer
/// is heard as `Signed::Verified`.
#[test]
fn a_small_order_greeting_key_signs_no_answer() {
    use super::wire;
    use crate::ed25519::{FORGED, SMALL_ORDER_KEY};
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, device) = mac(root.path(), "j");
    let j = joining(&device, [7; 32], "j");
    let greeting = joiner::Greeting {
        key: SMALL_ORDER_KEY,
        network: TRUST.network,
        authority: TRUST.authority,
        genesis: TRUST.genesis,
        name: "m5".to_owned(),
        can_allow: true,
        invite: false,
        challenge: [0; desk::CHALLENGE_LEN],
    };
    let (mut member, mut ours) = UnixStream::pair().expect("pair");
    for end in [&member, &ours] {
        end.set_read_timeout(Some(Duration::from_secs(60)))
            .expect("timeout");
    }
    let asked = std::thread::scope(|scope| {
        let answered = scope.spawn(move || -> Result<()> {
            wire::read_line(&mut member)?;
            let answer = format!("OK state=allowed gsig={}", wire::hex(&FORGED));
            wire::write_line(&mut member, &answer)
        });
        let asked = joiner::ask_checked(&mut ours, &greeting, &j);
        answered.join().expect("member").expect("answered");
        asked
    });
    let error = asked.map(|_| ()).unwrap_err().to_string();
    assert!(
        error.contains("not signed by the member its greeting names"),
        "{error}"
    );
}

/// A proof that came over the fleet's channel (`vault-join`) must name the node key that
/// channel proved, and only then is it a proof of the fleet's key too.
///
/// NEGATIVE CONTROL: the `fleet.is_some_and(..)` check cut from `begin_proof`: the proof of
/// another node key than the channel's goes through and the first assert fails.
#[test]
fn a_proof_over_the_fleet_names_the_node_key_its_channel_proved() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (m_paths, device) = mac(root.path(), "m");
    let desk = Mutex::new(Desk::new(
        Local::with(device, TRUST, true, "m").expect("member"),
    ));
    let (_, j_device) = mac(root.path(), "j");
    let (sign_node, node) = node_signer(0x33);
    let handed = hands(&m_paths, "v");
    let fleet_seen = std::sync::atomic::AtomicBool::new(false);
    let decide = |proving: &desk::Proving<'_>| {
        fleet_seen.store(proving.fleet, std::sync::atomic::Ordering::SeqCst);
        handed(proving)
    };
    let j = joining(&j_device, node, "m7");
    for (idx, current) in [true, false].into_iter().enumerate() {
        let from = address(u8::try_from(9 + idx).expect("host octet"));
        let (served, got) = proving_checked(
            &desk,
            (
                Some(desk::Fleet {
                    node: [0x44; 32],
                    current,
                }),
                from,
            ),
            &|_| None,
            &decide,
            |stream| {
                let greeting = joiner::greeting(stream)?;
                joiner::prove(stream, &greeting, &j, &sign_node, &mut false)
            },
        );
        assert!(
            served.is_err() && got.is_err(),
            "a proof of another node key than the channel's (current={current})"
        );
    }
    let (served, got) = proving_checked(
        &desk,
        (
            Some(desk::Fleet {
                node,
                current: false,
            }),
            address(11),
        ),
        &|_| None,
        &decide,
        |stream| {
            let greeting = joiner::greeting(stream)?;
            joiner::prove(stream, &greeting, &j, &sign_node, &mut false)
        },
    );
    served.expect("served under stale fleet code");
    got.expect("the bundle under stale fleet code");
    assert!(!fleet_seen.load(std::sync::atomic::Ordering::SeqCst));

    let (served, got) = proving_checked(
        &desk,
        (
            Some(desk::Fleet {
                node,
                current: true,
            }),
            address(12),
        ),
        &|_| None,
        &decide,
        |stream| {
            let greeting = joiner::greeting(stream)?;
            joiner::prove(stream, &greeting, &j, &sign_node, &mut false)
        },
    );
    served.expect("served");
    got.expect("the bundle");
    assert!(fleet_seen.load(std::sync::atomic::Ordering::SeqCst));
}

/// A stream that keeps the first line written through it.
struct Tee<'a, S: ?Sized> {
    inner: &'a mut S,
    first: &'a Mutex<String>,
}

impl<S: std::io::Read + ?Sized> std::io::Read for Tee<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<S: std::io::Write + ?Sized> std::io::Write for Tee<'_, S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut first = self.first.lock().unwrap();
        if !first.ends_with('\n') {
            first.push_str(&String::from_utf8_lossy(buf));
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// A stream that loses everything the joiner writes from its acknowledgement on: the link went
/// down just after the bundle arrived.
struct LosesTheAck<'a> {
    inner: &'a mut UnixStream,
    lost: bool,
}

impl std::io::Read for LosesTheAck<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(self.inner, buf)
    }
}

impl std::io::Write for LosesTheAck<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.lost |= buf.starts_with(b"ack ");
        if self.lost {
            return Ok(buf.len());
        }
        std::io::Write::write(self.inner, buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self.inner)
    }
}

/// A bundle the member wrote that the new Mac never acknowledged (the link dropped after the
/// write, or a relay cut it) leaves the row allowed, its phrase spent, so a person's Allow again
/// works at once and that phrase lets the Mac in; the row closes on the acknowledgement. A joiner
/// that asks for no acknowledgement (one from before them) closes the row at the write, as
/// before.
///
/// NEGATIVE CONTROL: `steps` closing the row before it reads the acknowledgement (the
/// `desk.close(..)` moved above the `acknowledged` read): the row is closed at once,
/// `left: [Closed]`, `right: [Allowed(PhraseId(..))]`.
#[test]
fn a_bundle_the_new_mac_never_acknowledged_leaves_its_row_open_for_allow_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = member(root.path(), "m1");
    let (_, device) = mac(root.path(), "j");
    let j = joining(&device, [7; 32], "j");
    assert_eq!(ask(&m1, address(9), &j), Answer::Asking);
    let words = allow(&m1);
    let pid = phrase_of(&m1);
    let phrase = Phrase::parse(&words).expect("three words");
    let (served, got) = exchange(&m1, address(9), &token(), |stream| {
        let greeting = joiner::greeting(stream)?;
        let mut lossy = LosesTheAck {
            inner: stream,
            lost: false,
        };
        joiner::run(&mut lossy, &greeting, &j, &phrase)
    });
    got.expect("the bundle arrived, its acknowledgement did not");
    let sent = served
        .expect("served")
        .expect("the member wrote its bundle");
    assert!(
        matches!(sent.delivered, desk::Delivered::Unacknowledged(_)),
        "sent, not acknowledged"
    );
    assert_eq!(
        states(&m1),
        [RowState::Allowed(pid)],
        "an unacknowledged bundle leaves the row allowed"
    );
    assert_eq!(
        m1.lock().unwrap().phrase_state(pid),
        Some(crate::pake::PhraseState::Admitted),
        "its phrase is spent"
    );

    let words = allow(&m1);
    let value = token();
    let (served, got) = run(&m1, address(9), &j, &words, &value);
    let bundle = got.expect("Allow again lets it in");
    let let_in = served.expect("served").expect("a run that sent its bundle");
    assert_eq!(let_in.delivered, desk::Delivered::Acknowledged);
    assert_eq!(
        text::parse_locker(bundle.files().expect("files").as_slice()).unwrap()[0]
            .value
            .as_slice(),
        value.as_bytes()
    );
    assert_eq!(
        states(&m1),
        [RowState::Closed],
        "acknowledged, the row closes"
    );

    // A joiner that asks for no acknowledgement: closed at the write.
    let (_, k_device) = mac(root.path(), "k");
    let k = joining(&k_device, [7; 32], "k");
    assert_eq!(ask(&m1, address(10), &k), Answer::Asking);
    let words = {
        let mut desk = m1.lock().unwrap();
        let now = Instant::now();
        let row = desk
            .rows(now)
            .iter()
            .find(|row| row.name == "k")
            .unwrap()
            .id;
        let phrase = desk.allow(row, now).expect("allowed");
        desk.words(phrase, now).expect("live").join(" ")
    };
    let phrase = Phrase::parse(&words).expect("three words");
    let request = k.request(&joiner_greeting(&m1)).unwrap();
    let (served, got) = exchange(&m1, address(10), "", |stream| {
        let greeting = joiner::greeting(stream)?;
        stolen_run(stream, &greeting, &request, &k_device, &k_device, &phrase)
    });
    got.expect("the bundle");
    let let_in = served.expect("served").expect("a run that sent its bundle");
    assert_eq!(let_in.delivered, desk::Delivered::Written);
    assert_eq!(states(&m1), [RowState::Closed, RowState::Closed]);
}

/// A stream that keeps every byte read through it.
struct Recording<'a, S: ?Sized> {
    inner: &'a mut S,
    read: Vec<u8>,
}

impl<S: std::io::Read + ?Sized> std::io::Read for Recording<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.extend_from_slice(&buf[..n]);
        Ok(n)
    }
}

impl<S: std::io::Write + ?Sized> std::io::Write for Recording<'_, S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// A LAN host that copies a member's greeting (its key, name and network) and answers every ask
/// "allowed" is no candidate beside that member: its greeting proves no key, while the member's
/// own answers prove its key on each connection. One member reached at two addresses (Wi-Fi and
/// Ethernet) is one candidate, so the phrase goes to it with no `--at`.
///
/// NEGATIVE CONTROLS: `ask_checked` taking any `gsig` as verified without checking it: the
/// copier that replays an answer m5 signed for another connection is heard,
/// `an answer m5 signed for another connection proves nothing: []`. `pick` skipping `outranked`:
/// `one candidate: Error("3 members show a phrase (m5 at 10.0.0.67:7100, m5 at 10.0.0.5:7100,
/// m5 at [fd00::5]:7100): ...`.
#[test]
fn a_host_that_copies_a_members_greeting_is_no_candidate_beside_it() {
    use super::join;
    use super::wire;
    let root = aterm_tempfile::tempdir().unwrap();
    let m5 = member(root.path(), "m5");
    let (_, device) = mac(root.path(), "j");
    let j = joining(&device, [7; 32], "j");
    assert_eq!(ask(&m5, address(9), &j), Answer::Asking);
    let _words = allow(&m5);
    // What the copier took from m5: its greeting line, and an answer m5 gave another ask.
    let (_, copied) = exchange(&m5, address(66), "", |stream| {
        let mut recording = Recording {
            inner: stream,
            read: Vec::new(),
        };
        let greeting = joiner::greeting(&mut recording)?;
        joiner::ask(&mut recording, &greeting, &j)?;
        Ok::<_, crate::Error>(String::from_utf8(recording.read).unwrap())
    });
    let copied = copied.expect("m5 answered the copier");
    let mut lines = copied.lines();
    let (greeting_line, answer_line) = (
        lines.next().unwrap().to_owned(),
        lines.next().unwrap().to_owned(),
    );
    let bundle = bundle_of("");
    std::thread::scope(|scope| {
        let dial = |at: &str| -> Result<Box<dyn join::Stream>> {
            let (ours, theirs) = UnixStream::pair()?;
            for end in [&ours, &theirs] {
                end.set_read_timeout(Some(Duration::from_secs(60)))?;
            }
            let replays = match at {
                "10.0.0.66:7100" => Some(answer_line.clone()),
                "10.0.0.67:7100" => Some("OK state=allowed".to_owned()),
                _ => None,
            };
            let (m5, bundle, greeting_line) = (&m5, &bundle, &greeting_line);
            scope.spawn(move || {
                let mut ours = ours;
                match replays {
                    Some(answer) => {
                        wire::write_line(&mut ours, greeting_line)?;
                        wire::read_line(&mut ours)?;
                        wire::write_line(&mut ours, &answer)
                    }
                    None => desk::serve(m5, &mut ours, address(9), bundle).map(drop),
                }
            });
            Ok(Box::new(theirs))
        };
        let ats = [
            "10.0.0.66:7100",
            "10.0.0.67:7100",
            "10.0.0.5:7100",
            "[fd00::5]:7100",
        ]
        .map(String::from);
        let (heard, missed) = join::ask_all(&dial, &j, &ats);
        assert!(
            !heard.iter().any(|heard| heard.at == "10.0.0.66:7100"),
            "an answer m5 signed for another connection proves nothing: {missed:?}"
        );
        assert!(
            missed.iter().any(|(at, _)| at == "10.0.0.66:7100"),
            "{missed:?}"
        );
        let signed = |at: &str| {
            heard
                .iter()
                .find(|heard| heard.at == at)
                .map(|heard| heard.signed)
        };
        assert_eq!(signed("10.0.0.67:7100"), Some(joiner::Signed::Unsigned));
        assert_eq!(signed("10.0.0.5:7100"), Some(joiner::Signed::Verified));
        assert_eq!(signed("[fd00::5]:7100"), Some(joiner::Signed::Verified));
        for member in [None, Some("m5")] {
            let picked = join::pick(&heard, member).expect("one candidate");
            assert!(
                picked.at == "10.0.0.5:7100" || picked.at == "[fd00::5]:7100",
                "{}",
                picked.at
            );
        }
    });
}

/// What a member at `at` said to an ask, for [`super::join::pick`]: its greeting under `key` and
/// `name`, showing an invite where `invite`, its answer, and whether it proved its key.
fn heard_from(
    at: &str,
    key: u8,
    name: &str,
    invite: bool,
    answer: Answer,
    signed: joiner::Signed,
) -> super::join::Heard {
    super::join::Heard {
        at: at.to_owned(),
        greeting: joiner::Greeting {
            key: [key; 32],
            network: TRUST.network,
            authority: TRUST.authority,
            genesis: TRUST.genesis,
            name: name.to_owned(),
            can_allow: true,
            invite,
            challenge: [0; desk::CHALLENGE_LEN],
        },
        answer,
        signed,
    }
}

/// A host that copies a member's greeting and answers "allowed", where the member itself shows an
/// invite (it answers "asking" with `invite=1`, signed), is no candidate: the copy proves no key,
/// and the member beside it proves that key. The phrase goes to the member, with no `--at`.
///
/// NEGATIVE CONTROL: ranking the answers after the tier is chosen (as before): the copier alone
/// said "allowed", nothing signed in that tier, and the phrase goes to it ("the phrase went to the
/// copier: 10.0.0.66:7100").
#[test]
fn a_copier_saying_allowed_is_no_candidate_beside_the_member_showing_an_invite() {
    let heard = [
        heard_from(
            "10.0.0.5:7100",
            5,
            "m5",
            true,
            Answer::Asking,
            joiner::Signed::Verified,
        ),
        heard_from(
            "10.0.0.66:7100",
            5,
            "m5",
            true,
            Answer::Allowed,
            joiner::Signed::Unsigned,
        ),
    ];
    let picked = super::join::pick(&heard, None).expect("one candidate");
    assert_eq!(
        picked.at, "10.0.0.5:7100",
        "the phrase went to the copier: {}",
        picked.at
    );
}

/// A member from before signed greetings (m3) and a member that signs (m5) both show a phrase for
/// this Mac: two candidates under two keys, so the run names both and sends the phrase to neither,
/// as before signed greetings. Sending it to m5 because m5 signs would burn m5's phrase when the
/// person typed m3's, and they would have to Allow again.
///
/// NEGATIVE CONTROL: dropping every unsigned answer beside any signed one (as before): the phrase
/// goes to m5 ("an old member beside a new one: Ok(\"10.0.0.5:7100\")").
#[test]
fn an_old_member_and_a_new_one_that_both_show_a_phrase_are_both_named() {
    let heard = [
        heard_from(
            "10.0.0.3:7100",
            3,
            "m3",
            false,
            Answer::Allowed,
            joiner::Signed::Unsigned,
        ),
        heard_from(
            "10.0.0.5:7100",
            5,
            "m5",
            false,
            Answer::Allowed,
            joiner::Signed::Verified,
        ),
    ];
    let picked = super::join::pick(&heard, None).map(|heard| heard.at.clone());
    let Err(error) = &picked else {
        panic!("an old member beside a new one: {picked:?}");
    };
    let said = error.to_string();
    assert!(
        said.contains("m3 at 10.0.0.3:7100") && said.contains("m5 at 10.0.0.5:7100"),
        "{said}"
    );
}

/// A Mac whose machine key a release build made on a Mac's own hardware: unmarked, whatever binary
/// opens it later (A12).
fn release_mac(root: &Path, name: &str) -> (Paths, Device) {
    let paths = Paths::for_home(&root.join(name));
    client::machine_identity_as(&paths, crate::Making::default()).expect("identity");
    let device = Device::open(&paths).expect("device").expect("an identity");
    (paths, device)
}

/// A joiner's end that keeps every byte it writes, so a test reads the lines it sent.
struct Tap<'a> {
    stream: &'a mut UnixStream,
    sent: Vec<u8>,
}

impl std::io::Read for Tap<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(self.stream, bytes)
    }
}

impl std::io::Write for Tap<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let wrote = std::io::Write::write(self.stream, bytes)?;
        self.sent.extend_from_slice(&bytes[..wrote]);
        Ok(wrote)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self.stream)
    }
}

/// A12: an identity this test binary made is a lab identity, its Commons device says so, and its
/// `ask` line carries `lab=1`; an identity a release build made carries nothing, opened by the same
/// binary.
#[test]
fn a_test_binarys_ask_carries_the_lab_mark_and_a_release_identitys_does_not() {
    let root = aterm_tempfile::tempdir().expect("temp");
    let m1 = member(root.path(), "m1");
    let (_, lab) = mac(root.path(), "lab");
    let (_, release) = release_mac(root.path(), "release");
    assert_eq!(lab.lab(), Some(crate::LabMark::TestBinary));
    assert_eq!(release.lab(), None);
    for (device, marked) in [(&lab, true), (&release, false)] {
        let joining = joining(device, [7; 32], "m9");
        let (_, sent) = exchange(&m1, address(9), "", |stream| {
            let mut tap = Tap {
                stream,
                sent: Vec::new(),
            };
            let greeting = joiner::greeting(&mut tap).expect("greeting");
            joiner::ask(&mut tap, &greeting, &joining).expect("asked");
            String::from_utf8(tap.sent).expect("ASCII")
        });
        let line = sent.lines().next().expect("the ask line");
        assert!(line.starts_with("ask "), "{line}");
        assert_eq!(line.ends_with(" lab=1"), marked, "{line}");
    }
}

/// A12: a member on the live ALab genesis refuses a lab identity's ask once its signature checks,
/// showing no row; a member on another genesis (a lab's) admits the same ask, and the live member
/// admits a release identity's. The pair shows the refusal bites and keys on the mark alone.
#[test]
fn a_member_on_the_live_genesis_refuses_a_lab_identitys_ask_and_one_on_another_admits_it() {
    let root = aterm_tempfile::tempdir().expect("temp");
    let (_, m1) = mac(root.path(), "m1");
    let live = Mutex::new(Desk::new(
        Local::with(m1, TRUST, true, "m1")
            .expect("member")
            .with_live_genesis(true),
    ));
    let other = member(root.path(), "m2");
    assert!(live.lock().unwrap().local().on_live_genesis());
    assert!(!other.lock().unwrap().local().on_live_genesis());
    let (_, lab) = mac(root.path(), "lab");
    let lab_joining = joining(&lab, [7; 32], "m9");

    let (served, said) = exchange(&live, address(9), "", |stream| {
        let greeting = joiner::greeting(stream)?;
        joiner::ask(stream, &greeting, &lab_joining)
    });
    assert_eq!(
        said.expect_err("refused").to_string(),
        desk::LAB_REFUSED,
        "the joiner reads the member's refusal"
    );
    assert!(served.is_err());
    assert!(states(&live).is_empty(), "a refused ask shows no row");

    assert_eq!(ask(&other, address(9), &lab_joining), Answer::Asking);
    assert_eq!(states(&other), [RowState::Asking]);

    let (_, release) = release_mac(root.path(), "release");
    assert_eq!(
        ask(&live, address(10), &joining(&release, [8; 32], "m10")),
        Answer::Asking
    );
    assert_eq!(states(&live), [RowState::Asking]);
}

/// A12: a lab identity's run with an invite on the live genesis is refused before the phrase is
/// touched, so the invite stays live, and a release identity's run with the same words is let in.
#[test]
fn a_lab_identitys_run_on_the_live_genesis_spends_no_phrase() {
    let root = aterm_tempfile::tempdir().expect("temp");
    let (_, m1) = mac(root.path(), "m1");
    let live = Mutex::new(Desk::new(
        Local::with(m1, TRUST, true, "m1")
            .expect("member")
            .with_live_genesis(true),
    ));
    let now = Instant::now();
    let (phrase, words) = {
        let mut desk = live.lock().unwrap();
        let phrase = desk.invite(now).expect("invite");
        (phrase, desk.words(phrase, now).expect("live").join(" "))
    };
    let (_, lab) = mac(root.path(), "lab");
    let (served, got) = run(&live, address(9), &joining(&lab, [7; 32], "m9"), &words, "");
    assert_eq!(got.expect_err("refused").to_string(), desk::LAB_REFUSED);
    assert!(served.is_err());
    assert_eq!(
        live.lock().unwrap().phrase_state(phrase),
        Some(crate::pake::PhraseState::Live),
        "the refusal spent no phrase"
    );

    let (_, release) = release_mac(root.path(), "release");
    let value = token();
    let (served, got) = run(
        &live,
        address(10),
        &joining(&release, [8; 32], "m10"),
        &words,
        &value,
    );
    served.expect("served");
    got.expect("a release identity's run is let in");
}

/// A Mac that asks with the node key a live seat holds, under another vault key, is that seat's
/// Mac rebuilt with its node key kept (one-mesh A15): its row names the seat it takes back, the
/// card says when the old key last signed and that it reads no newer secret once Allowed, and a
/// plain Allow (no "Allow again") gives it a phrase. Only a Mac that proved the node key on this
/// desk shows so (an ask names it unproven). The seat's own Mac asking again is no move, and a
/// Mac with another node key is a new one.
///
/// NEGATIVE CONTROL: `Desk::kept_seat` reading only `self.local.kept.get(&heard.node)` (the seat
/// filter cut): the seat's own Mac reads as moving its own seat and the `own` assert fails.
#[test]
fn a_rebuilt_macs_row_names_the_seat_it_takes_back_and_when_it_last_signed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, m1_device) = mac(root.path(), "m1");
    let (_, old_device) = mac(root.path(), "m10-old");
    let (_, rebuilt_device) = mac(root.path(), "m10");
    let (_, stranger_device) = mac(root.path(), "stranger");
    let node = [0x10; 32];
    let old_seat = old_device
        .request(TRUST.network, TRUST.authority)
        .unwrap()
        .binding()
        .actor();
    // 2026-11-05 14:02 UTC.
    let signed = 1_793_887_320_000;
    let kept = client::KeptSeat {
        seat: old_seat,
        label: "m10".to_owned(),
        last_signed_ms: Some(signed),
    };
    let desk = Mutex::new(Desk::new(
        Local::with(m1_device, TRUST, true, "m1")
            .unwrap()
            .with_kept(std::collections::BTreeMap::from([(node, kept.clone())])),
    ));
    for (device, name, node) in [
        (&rebuilt_device, "m10", node),
        (&old_device, "m10-old", node),
        (&stranger_device, "stranger", [0x11; 32]),
    ] {
        assert_eq!(
            ask(&desk, address(10), &joining(device, node, name)),
            Answer::Asking
        );
    }
    // An ask names its node key unproven: until the Mac proves it here, it is a new Mac.
    let now = Instant::now();
    let unproven = desk.lock().unwrap().rows(now);
    assert!(unproven.iter().all(|r| r.moves.is_none()), "{unproven:?}");
    for device in [&rebuilt_device, &old_device] {
        let request = device.request(TRUST.network, TRUST.authority).unwrap();
        desk.lock().unwrap().with_proven_node(&request, node, now);
    }
    let rows = desk.lock().unwrap().rows(now);
    let row = |name: &str| rows.iter().find(|r| r.name == name).unwrap().clone();
    let rebuilt = row("m10");
    assert_eq!(rebuilt.moves.as_ref(), Some(&kept));
    assert!(rebuilt.removed.is_none());
    assert_eq!(
        kept.title(),
        "m10 is back with a new vault key. Its old key last signed on Nov 5 at 14:02 UTC; once \
         you Allow this, that key reads no newer secret"
    );
    assert!(
        row("m10-old").moves.is_none(),
        "own: no move of its own seat"
    );
    assert!(row("stranger").moves.is_none());
    desk.lock()
        .unwrap()
        .allow(rebuilt.id, now)
        .expect("a plain Allow moves the seat");
}

/// End to end on one desk: a Mac rebuilt with its node key kept proves that key first
/// (`join::prove_first`), and the member refuses it before possession while its old seat signed
/// within the day; its plain ask that follows shows the seat it takes back, and its run hands the
/// bundle builder `node_proven`. Another Mac that asks naming the same node key, with no proof,
/// shows as a new Mac.
///
/// NEGATIVE CONTROL: `Desk::begin_proof` not recording the proven node (its `self.proven.push`
/// cut): the rebuilt Mac's row shows no move and the `moves` assert fails.
#[test]
fn a_refused_proof_of_a_kept_node_key_lets_the_ask_that_follows_show_the_move() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (_, m1_device) = mac(root.path(), "m1");
    let (_, rebuilt_device) = mac(root.path(), "m10");
    let (_, namer_device) = mac(root.path(), "namer");
    let (sign_node, node) = node_signer(0x61);
    let kept = client::KeptSeat {
        seat: m1_device
            .request(TRUST.network, TRUST.authority)
            .unwrap()
            .binding()
            .actor(),
        label: "m10".to_owned(),
        last_signed_ms: None,
    };
    let desk = Mutex::new(Desk::new(
        Local::with(m1_device, TRUST, true, "m1")
            .unwrap()
            .with_kept(std::collections::BTreeMap::from([(node, kept)])),
    ));
    let rebuilt = joining(&rebuilt_device, node, "m10");
    let refused = |_: &desk::Proving<'_>| {
        Some("the seat this node key names signed a record in the last day".to_owned())
    };
    let never = |_: &desk::Proving<'_>| -> Result<SecretBytes> {
        panic!("a refused proof decides nothing")
    };
    let (_, said) = proving_checked(&desk, (None, address(10)), &refused, &never, |stream| {
        let greeting = joiner::greeting(stream)?;
        joiner::prove(stream, &greeting, &rebuilt, &sign_node, &mut false)
    });
    assert!(said.is_err(), "the proof is refused");
    assert_eq!(ask(&desk, address(10), &rebuilt), Answer::Asking);
    assert_eq!(
        ask(&desk, address(11), &joining(&namer_device, node, "namer")),
        Answer::Asking
    );
    let rows = desk.lock().unwrap().rows(Instant::now());
    let row = |name: &str| rows.iter().find(|r| r.name == name).unwrap().clone();
    assert_eq!(
        row("m10").moves.map(|kept| kept.label),
        Some("m10".to_owned()),
        "moves: the proven Mac's row names the seat"
    );
    assert!(
        row("namer").moves.is_none(),
        "an unproven name moves nothing"
    );
}
