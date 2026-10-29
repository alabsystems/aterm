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
    let got = std::thread::scope(|scope| {
        let bundle = bundle_of(&value);
        let served = scope.spawn(move || desk::serve(&m1, &mut window, address(9), &bundle));
        let relayed = scope.spawn(move || super::relay(&mut node_end, &mut window_end));
        let greeting = joiner::greeting(&mut joiner_end).unwrap();
        let got = joiner::run(&mut joiner_end, &greeting, &j, &phrase);
        assert!(served.join().unwrap().unwrap().is_some());
        relayed.join().unwrap().unwrap();
        got
    });
    let bundle = got.expect("the bundle through the relay");
    assert_eq!(
        text::parse_locker(bundle.files().expect("files").as_slice()).unwrap()[0]
            .value
            .as_slice(),
        value.as_bytes()
    );
}
