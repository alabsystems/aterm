// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{fleet_of, may_dial, members_of};
use std::net::{IpAddr, Ipv4Addr};

/// A download lets in no member's address while this Mac's deny list cannot be read (a failed
/// read, or a sync applying), only once it reads; a process under a private state root reads
/// no vault, and so names no one.
///
/// NEGATIVE CONTROL (2026-11-01): with `members_of` reading an unread deny list as one that
/// names no one (`.unwrap_or_default()`, the shape before), this fails (`no member while the
/// deny list is unread`).
#[test]
fn a_download_lets_in_no_member_while_the_deny_list_is_unread() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    let member = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30));
    let addrs = |denied: &[String]| {
        assert!(denied.is_empty());
        vec![member]
    };
    for unread in [Err(aterm_secrets::Error::new("unreadable")), Ok(None)] {
        assert!(
            members_of(Some(&paths), |_| unread, addrs).is_empty(),
            "no member while the deny list is unread"
        );
    }
    assert_eq!(
        members_of(Some(&paths), |_| Ok(Some(Vec::new())), addrs),
        vec![member]
    );
    assert_eq!(
        members_of(
            None,
            |_| panic!("a private state root reads no vault"),
            addrs
        ),
        vec![member]
    );
}

/// A download reads the vault the window's fleet readers read, by where this process's state
/// root resolved (`aterm_gui::vault_here`, ATERM-KI-1105-10): this test binary runs under its
/// scratch root, a private one, so it reads none, with `CI` set as without. Here aterm-gui
/// builds as a library, so its predicate decides alone; in aterm-gui's own tests `cfg!(test)`
/// counts every run as private, and none of them can hold it.
///
/// NEGATIVE CONTROL (2026-11-05): aterm-gui's `state_root_is_private` put back to the reading
/// before 2026-11-05 (`cfg!(test)`, `ATERM_STATE_HOME` or `CI` set) fails with `CI` unset,
/// on the vault read's own panic ("a private state root reads no vault"), so no vault is read.
#[test]
fn a_download_reads_the_vault_its_state_root_resolved_to() {
    assert!(
        aterm_types::dirs::runs_under_private_state_root(),
        "PRECONDITION: a test binary runs under its scratch root"
    );
    let reads = || aterm_gui::vault_reading(|| panic!("a private state root reads no vault"));
    assert_eq!(
        aterm_log::env::scoped_unset("CI", reads),
        Ok(None),
        "a test binary's scratch root reads no vault, `CI` unset"
    );
    assert_eq!(
        aterm_log::env::scoped("CI", "1", reads),
        Ok(None),
        "nor with `CI` set"
    );
}

/// A download dials fleet members only while `[swarm] enabled` reads on: sharing off, or a
/// switch that cannot be read, dials none and says why.
/// NEGATIVE CONTROL: hold the dials only on `Ok(false)`: the unreadable switch dials.
#[test]
fn a_download_dials_no_member_unless_sharing_reads_on() {
    let members = [IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30))];
    let on = fleet_of(Ok(true), &members, |_| true, |_| true);
    assert_eq!(on.dial.len(), 1, "sharing on: the member is dialed");
    assert_eq!(on.held, None);
    for (sharing, says) in [
        (Ok(false), "sharing is off"),
        (
            Err("aterm.toml: not UTF-8 text".to_owned()),
            "cannot read its settings file",
        ),
    ] {
        let held = fleet_of(sharing.clone(), &members, |_| true, |_| true);
        assert!(held.dial.is_empty(), "{sharing:?}: no member dialed");
        assert_eq!(held.members, 1, "{sharing:?}: the server's count stays");
        assert!(
            held.held.as_deref().is_some_and(|why| why.contains(says)),
            "{sharing:?}: {:?}",
            held.held
        );
    }
}

/// A private aterm (this Cargo test binary) races none of its fleet members for a toolset: the
/// guard [`super::fleet`] dials by refuses each member's toolset server, at the person's port,
/// loopback or LAN, while the server's count stays and nothing claims the network holds it.
///
/// NEGATIVE CONTROL (2026-11-03): with the guard refusing the fleet's port alone, the shape
/// before, both members are dialed and this fails.
#[test]
fn a_private_aterm_dials_no_member_at_the_persons_toolset_port() {
    let members = [
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30)),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    ];
    let private = fleet_of(Ok(true), &members, |_| true, may_dial);
    assert!(private.dial.is_empty(), "dialed {:?}", private.dial);
    assert_eq!((private.members, private.held), (2, None));
    let control = fleet_of(Ok(true), &members, |_| true, |_| true);
    assert_eq!(control.dial.len(), 2, "the control: {:?}", control.dial);
}

/// Away from the ALab LAN a download races only the fleet members this network reaches;
/// with none it races no member and says the network holds it.
/// NEGATIVE CONTROL: dropping the `reaches` filter dials the member behind the ALab LAN, and
/// "only the member this network reaches" fails.
#[test]
fn away_a_download_races_only_the_members_this_network_reaches() {
    let home = IpAddr::V4(Ipv4Addr::new(192, 168, 4, 30));
    let here = IpAddr::V4(Ipv4Addr::new(172, 20, 10, 5));
    let away = |ip: IpAddr| ip == here;
    let fleet = fleet_of(Ok(true), &[home, here], away, |_| true);
    assert_eq!(
        fleet.dial,
        [format!("{here}:{}", atpkg::peers::DEFAULT_PORT)],
        "only the member this network reaches"
    );
    assert_eq!((fleet.members, fleet.held), (2, None));
    let none = fleet_of(Ok(true), &[home], away, |_| true);
    assert!(none.dial.is_empty());
    assert_eq!(none.members, 1);
    assert!(none.held.is_some(), "the network holds it");
}
