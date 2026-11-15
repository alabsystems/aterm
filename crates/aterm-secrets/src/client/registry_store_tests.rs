// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    age::Identity,
    commons::{
        binding::Request,
        hearing::{RECENT, Said, Version},
        journal::Trust,
    },
};

const TRUST: Trust = Trust {
    network: [7; 16],
    authority: [11; 32],
    genesis: [13; 32],
};
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const NOW: u64 = 400 * DAY_MS;
const RELEASE: Version = Version(0, 210, 0);

fn member(n: u8) -> Binding {
    Request::create(
        TRUST.network,
        TRUST.authority,
        [n + 41; 16],
        [n + 47; 16],
        &Identity::from_secret_bytes([n + 19; 32]),
        &[n + 31; 32],
    )
    .unwrap()
    .binding()
    .clone()
}

fn readers(of: &[&Binding]) -> BTreeSet<Recipient> {
    of.iter().map(|binding| binding.recipient()).collect()
}

/// A ledger that heard each of `members` report `version` a day ago.
fn reporting(members: &[&Binding], version: Version) -> Ledger {
    let mut ledger = Ledger::new(TRUST);
    for member in members {
        ledger.record(member.actor(), NOW - DAY_MS, Said::Version(version));
    }
    ledger
}

/// The registry's floor fails closed: a put waits while any current roster member reports an
/// older aterm, reports nothing, or was not heard within 7 days (however long this device
/// watched), while the vault has a reader no member accounts for, and, whatever every Mac
/// reports, while no release is named as the first that writes no secret file. The refusal
/// counts each kind and names no value.
/// NEGATIVE CONTROLS: with `hearing::in_place` letting an unheard member pass, a member never
/// heard through a 30-day watch passes: `` left: 0 right: 1 ``; with `holding` ignoring the
/// readers, the outside reader passes: `` left: 0 right: 1 ``.
#[test]
fn a_registry_put_waits_for_every_mac_reading_the_vault_to_write_no_secret_file() {
    let (a, b, c) = (member(1), member(2), member(3));
    let roster = [&a, &b, &c];
    let all = reporting(&roster, RELEASE);
    let judge = |ledger: &Ledger, readers: &BTreeSet<Recipient>, release| {
        holding(ledger, roster, readers, NOW, release)
    };
    assert_eq!(judge(&all, &readers(&roster), Some(RELEASE)).macs(), 0);
    assert!(floor(&judge(&all, &readers(&roster), Some(RELEASE))).is_ok());
    assert_eq!(
        judge(
            &reporting(&roster, Version(0, 211, 0)),
            &readers(&roster),
            Some(RELEASE)
        )
        .macs(),
        0
    );
    // No release named: every member holds the put, whatever it reports.
    let none = judge(&all, &readers(&roster), None);
    assert_eq!(none.older, 3, "{none:?}");
    let error = floor(&none).unwrap_err().to_string();
    assert!(error.contains("none is named yet"), "{error}");

    let mut older = all.clone();
    older.record(b.actor(), NOW, Said::Version(Version(0, 200, 0)));
    let error = floor(&judge(&older, &readers(&roster), Some(RELEASE)))
        .unwrap_err()
        .to_string();
    assert!(error.contains("1 Mac run an older aterm"), "{error}");

    let mut nothing = all.clone();
    nothing.record(b.actor(), NOW, Said::Nothing);
    assert_eq!(judge(&nothing, &readers(&roster), Some(RELEASE)).silent, 1);

    // Never heard, through a watch long past the 0.145 gate's 7 days: still held.
    let mut watched = Ledger::watched(TRUST, NOW - 30 * DAY_MS, NOW);
    watched.record(a.actor(), NOW - DAY_MS, Said::Version(RELEASE));
    watched.record(b.actor(), NOW - DAY_MS, Said::Version(RELEASE));
    assert_eq!(judge(&watched, &readers(&roster), Some(RELEASE)).unheard, 1);
    // A report 7 days old counts for nothing.
    assert_eq!(
        holding(&all, roster, &readers(&roster), NOW + RECENT, Some(RELEASE)).macs(),
        3
    );

    // A reader no roster member accounts for holds it; one gone from the roster does not.
    let stranger = member(9);
    let outside = judge(&all, &readers(&[&a, &b, &c, &stranger]), Some(RELEASE));
    assert_eq!(outside.outside, 1);
    let error = floor(&outside).unwrap_err().to_string();
    assert!(
        error.contains("1 Mac read the vault from outside the Commons roster"),
        "{error}"
    );
    assert_eq!(
        holding(&all, [&a, &b], &readers(&[&a, &b]), NOW, Some(RELEASE)).macs(),
        0
    );
}

/// The store reaches only plain shared entries: fleet configuration, a Claude Code token and an
/// invalid name are refused before any vault is asked.
#[test]
fn the_registry_store_reaches_no_entry_of_another_class() {
    assert!(shared_name("alab-address-password-ann.0123456789abcdef").is_ok());
    for name in [
        crate::entry_class::FLEET_ACCOUNTS,
        "aterm-claude-token-0123",
        "../escape",
    ] {
        assert!(shared_name(name).is_err(), "{name}");
    }
}

/// A put is checked against the readers it reaches where it writes now: every one must be among
/// those the fleet judgment counted, so a Mac seated, wrapped to or added to the published text
/// since the judgment holds the put back until it is judged too. The refusal names no Mac.
/// NEGATIVE CONTROL: with `InPlace::covers` answering Ok whatever it is asked, a put reaching
/// the newcomer passes: `` assertion failed: judged.covers(&with_newcomer).is_err() ``.
#[test]
fn a_put_reaching_a_reader_the_judgment_did_not_count_is_refused() {
    let (a, b, newcomer) = (member(1), member(2), member(3));
    let judged = InPlace::counting(readers(&[&a, &b]));
    assert!(judged.covers(&readers(&[&a, &b])).is_ok());
    assert!(judged.covers(&readers(&[&a])).is_ok());
    let with_newcomer = readers(&[&a, &b, &newcomer]);
    assert!(judged.covers(&with_newcomer).is_err());
    let error = judged.covers(&with_newcomer).unwrap_err().to_string();
    assert!(error.contains("did not count"), "{error}");
}

/// The published text's readers fail closed: a Mac that is no admin adds none, an admin adds
/// each machine of its text, and an admin whose text cannot be read now (its state is changing,
/// its copy does not open), like a Mac that cannot tell whether it is an admin, refuses the
/// judgment instead of leaving those machines out.
/// NEGATIVE CONTROL: with `published_readers` taking an unreadable text as no admin's (the old
/// `if let Ok(admin) = admin_cached(..)`), the transient failure passes:
/// `` assertion failed: unreadable.is_err() ``.
#[test]
fn an_admin_that_cannot_read_its_published_text_refuses_the_judgment() {
    let (a, b) = (member(1), member(2));
    let none = published_readers(Ok(false), || panic!("no admin reads no text"));
    assert_eq!(none.unwrap(), None);
    let some = published_readers(Ok(true), || Ok(readers(&[&a, &b])));
    assert_eq!(some.unwrap(), Some(readers(&[&a, &b])));
    let unreadable = published_readers(Ok(true), || {
        Err(Error::new("vault state is changing; retry enrollment"))
    });
    assert!(unreadable.is_err());
    let error = unreadable.unwrap_err().to_string();
    assert!(error.contains("cannot be read"), "{error}");
    let untold = published_readers(Err(Error::new("its state is being changed")), || {
        panic!("an untold admin reads no text")
    });
    assert!(untold.is_err());
}

/// This Mac holds the puts back while another aterm process here runs a build below the floor,
/// one whose build cannot be told, or while the process table cannot be read: an app an update
/// has not relaunched still writes files, whatever the build asking runs. A bundle's release is
/// its `CFBundleShortVersionString`.
/// NEGATIVE CONTROL: with `local_holding` counting only builds below the floor, the unknown
/// build passes: `` left: 0 right: 1 ``.
#[test]
fn a_registry_put_waits_for_an_older_aterm_on_this_mac() {
    let new = Some(RELEASE);
    let old = Some(Version(0, 202, 0));
    assert_eq!(local_holding(Some(&[]), Some(RELEASE)), 0);
    assert_eq!(local_holding(Some(&[new, new]), Some(RELEASE)), 0);
    assert_eq!(local_holding(Some(&[new, old]), Some(RELEASE)), 1);
    assert_eq!(local_holding(Some(&[None]), Some(RELEASE)), 1);
    assert_eq!(local_holding(None, Some(RELEASE)), 1);
    assert_eq!(local_holding(Some(&[new]), None), 1);
    let held = Holding {
        here: 1,
        ..Holding::default()
    };
    assert!(!held.clear());
    let refusal = floor(&held).unwrap_err().to_string();
    assert!(refusal.contains("1 other aterm process on this Mac runs an older or unknown build"));
    assert!(floor(&Holding::default()).is_ok());
    let plist =
        "<dict>\n\t<key>CFBundleShortVersionString</key>\n\t<string>0.203.0</string>\n</dict>";
    assert_eq!(bundle_release(plist), Some(Version(0, 203, 0)));
    assert_eq!(
        bundle_release("<dict><key>CFBundleVersion</key><string>1</string></dict>"),
        None
    );
}
