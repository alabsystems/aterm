// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn reader_capability_distinguishes_files_legacy_and_explicit_bounds() {
    use crate::crossing::RecordsBound;
    assert_eq!(reader(&Line::parse("run")).unwrap(), Reader::Files);
    assert_eq!(
        reader(&Line::parse("run reads=2")).unwrap(),
        Reader::LegacyLog
    );
    let bound = RecordsBound::of(8192, true);
    let line = format!(
        "prove reads=2 records-bound={}",
        wire::escape(&bound.value())
    );
    assert_eq!(
        reader(&Line::parse(&line)).unwrap(),
        Reader::BoundedLog(bound)
    );
    assert!(!Reader::LegacyLog.can_take(RecordsBound::of(4096, true)));
    assert!(Reader::LegacyLog.can_take(RecordsBound::of(8192, false)));
    assert!(!Reader::BoundedLog(RecordsBound::of(4096, false)).can_take(bound));
    for bad in [
        "run records-bound=1%204096%20uncrossed",
        "run reads=2 records-bound=1%200%20uncrossed",
        "run reads=2 records-bound=1%2004096%20uncrossed",
        "run reads=2 records-bound=1%204096%20maybe",
        "run reads=2 records-bound=1%204096%20crossed records-bound=1%204096%20crossed",
    ] {
        assert!(reader(&Line::parse(bad)).is_err(), "{bad}");
    }
}

/// A member's refusal of a history its reader cannot take, and a new Mac's of a member's
/// bundle, name the Mac whose aterm is short.
///
/// NEGATIVE CONTROL (in the test): histories both sides take carry no refusal.
#[test]
fn a_capacity_refusal_names_the_mac_whose_aterm_is_short() {
    use super::super::bundle::source_refusal;
    use crate::crossing::RecordsBound;
    let (legacy, larger) = (RecordsBound::of(4096, false), RecordsBound::of(8192, false));
    let (crossed, larger_crossed) = (RecordsBound::of(4096, true), RecordsBound::of(8192, true));
    for (reader, source, update) in [
        (Reader::Files, legacy, "on the new Mac"),
        (Reader::LegacyLog, crossed, "on the new Mac"),
        (Reader::BoundedLog(legacy), larger_crossed, "on the new Mac"),
        (
            Reader::BoundedLog(larger_crossed),
            legacy,
            "on the allowing Mac",
        ),
    ] {
        assert!(!reader.can_take(source), "{reader:?} {source:?}");
        let why = reader.refusal(source);
        assert!(why.contains(&format!("update aterm {update}")), "{why}");
    }
    assert!(
        Reader::LegacyLog
            .refusal(crossed)
            .contains("an older aterm")
    );
    for (local, source, update) in [
        (legacy, Some(larger_crossed), "on this Mac"),
        (larger_crossed, Some(legacy), "on the allowing Mac"),
        (crossed, None, "on the allowing Mac"),
    ] {
        let why = source_refusal(local, source).expect("refused");
        assert!(why.contains(&format!("update aterm {update}")), "{why}");
    }
    // Control: histories both sides hold.
    assert!(Reader::LegacyLog.can_take(larger));
    assert!(Reader::BoundedLog(larger_crossed).can_take(larger_crossed));
    assert_eq!(source_refusal(larger, Some(legacy)), None);
    assert_eq!(source_refusal(legacy, None), None);
    assert_eq!(source_refusal(larger_crossed, Some(larger_crossed)), None);
}

const TRUST: Trust = Trust {
    network: [1; 16],
    authority: [2; 32],
    genesis: [3; 32],
};

fn device(root: &std::path::Path, name: &str) -> Device {
    let paths = client::Paths::for_home(&root.join(name));
    client::machine_identity(&paths).unwrap();
    Device::open(&paths).unwrap().unwrap()
}

fn ask(desk: &mut Desk, device: &Device, name: &str, from: IpAddr, now: Instant) -> Request {
    let request = device.request(TRUST.network, TRUST.authority).unwrap();
    let node = [7; 32];
    let signature = super::super::joiner::ask_signature(
        device,
        desk.local.key(),
        &node,
        name,
        "Mac",
        request.bytes(),
    )
    .unwrap();
    desk.hear(
        &Asking {
            wire: request.bytes(),
            node,
            name,
            model: "Mac",
            signature: &signature,
        },
        from,
        now,
    )
    .unwrap();
    request
}

/// Only a proof from another host on this member's home ALab LAN is from the home ALab LAN
/// (D1, [`Proving::lan`]): one from another private network the member also reaches (a
/// second interface's `10.0.0.0/8`, `172.16.0.0/12`, a VM bridge, a phone hotspot's
/// `172.20.10.0/28`), from this member's own address there, or from a public or IPv6 address
/// is not; a member on no home ALab LAN marks none, even from an address its old home held;
/// and a personal vault's member marks none wherever it is.
///
/// NEGATIVE CONTROLS: `Local::automatically_admits` taking every private source while the
/// member has a home (an IPv4 or IPv4-mapped private address, in place of `home.holds`): the
/// VM bridge's `192.168.64.3` reads as home. Taking any source while the member has none
/// (`self.home.is_none_or(…)`): "no home: 192.168.64.3" fails.
#[test]
fn only_a_host_on_the_members_home_lan_is_from_the_home_lan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let member = || Local::with(device(root.path(), "member"), TRUST, true, "member").unwrap();
    let ip = |text: &str| -> IpAddr { text.parse().unwrap() };
    let home = member().with_automatic_lan([]);
    assert!(home.automatically_admits(ip("192.168.1.45")));
    assert!(home.automatically_admits(ip("::ffff:192.168.1.46")));
    let elsewhere = [
        "192.168.64.3",
        "172.20.10.5",
        "10.0.0.9",
        "172.20.0.5",
        "192.168.2.45",
        "192.168.1.20",
        "93.184.216.34",
        "fe80::1",
    ];
    for from in elsewhere {
        assert!(!home.automatically_admits(ip(from)), "{from}");
    }
    let nowhere = member().with_automatic_lan([]).with_home(None);
    let personal = member().with_home(Some(crate::home_lan::HomeNet::test()));
    for from in elsewhere.into_iter().chain(["192.168.1.45"]) {
        assert!(!nowhere.automatically_admits(ip(from)), "no home: {from}");
        assert!(!personal.automatically_admits(ip(from)), "personal: {from}");
    }
}

/// A plain ask, even from this member's home ALab LAN on the pinned ALab vault, is a row that
/// waits for a person's Allow: nothing lets a Mac in by its asking alone.
#[test]
fn a_plain_ask_from_the_home_lan_waits_for_a_person() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut desk = Desk::new(
        Local::with(device(root.path(), "member"), TRUST, true, "member")
            .unwrap()
            .with_automatic_lan([]),
    );
    let now = Instant::now();
    ask(
        &mut desk,
        &device(root.path(), "new"),
        "new",
        "192.168.1.45".parse().unwrap(),
        now,
    );
    let rows = desk.rows(now);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, RowState::Asking);
}

#[test]
fn confirmed_admission_closes_existing_and_repeated_approval_rows() {
    let root = aterm_tempfile::tempdir().unwrap();
    let local = || Local::with(device(root.path(), "member"), TRUST, true, "member").unwrap();
    let mut desk = Desk::new(local().with_automatic_lan([]));
    let now = Instant::now();
    let joining = device(root.path(), "new");
    let request = ask(
        &mut desk,
        &joining,
        "m110",
        "192.168.1.45".parse().unwrap(),
        now,
    );
    desk.refresh(local().with_automatic_lan([request.binding().recipient().to_bech32()]));
    assert_eq!(desk.rows(now)[0].state, RowState::Closed);
    ask(
        &mut desk,
        &joining,
        "m110",
        "192.168.1.45".parse().unwrap(),
        now,
    );
    assert_eq!(desk.rows(now)[0].state, RowState::Closed);
}
