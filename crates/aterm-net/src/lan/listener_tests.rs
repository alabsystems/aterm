// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn source_selection_refuses_ambiguous_virtual_and_unscoped_addresses() {
    let mut a = Attachment {
        index: 3,
        name: "test-physical".into(),
        address: "192.0.2.1".parse().unwrap(),
        netmask: "255.255.255.0".parse().unwrap(),
        flags: super::super::REQUIRED_FLAGS,
        physical: true,
    };
    let local = address(&a);
    assert_eq!(select(local, std::slice::from_ref(&a)).unwrap(), a);
    let mut another = a.clone();
    another.index += 1;
    assert!(select(local, &[a.clone(), another.clone()]).is_err());
    another.physical = false;
    assert!(
        select(local, &[a.clone(), another.clone()]).is_err(),
        "a virtual duplicate cannot be hidden by eligibility filtering"
    );
    another.netmask = "0.0.0.0".parse().unwrap();
    assert!(
        select(local, &[a.clone(), another]).is_err(),
        "an unknown prefix does not hide a duplicate address"
    );
    a.physical = false;
    assert!(select(local, std::slice::from_ref(&a)).is_err());
    a.physical = true;
    a.flags = 0;
    assert!(select(local, std::slice::from_ref(&a)).is_err());
    a.flags = super::super::REQUIRED_FLAGS;
    a.address = "fe80::1".parse().unwrap();
    a.netmask = "ffff:ffff:ffff:ffff::".parse().unwrap();
    assert!(select("[fe80::1]:0".parse().unwrap(), std::slice::from_ref(&a)).is_err());
    assert_eq!(select(address(&a), std::slice::from_ref(&a)).unwrap(), a);
    a.netmask = "::".parse().unwrap();
    assert!(select(address(&a), &[a]).is_err());
}

#[test]
fn wildcard_and_scope_confusion_never_create_a_listener() {
    for address in [
        "0.0.0.0:0",
        "[::]:0",
        "[::ffff:127.0.0.1]:0",
        "[::1%3]:0",
        "224.0.0.1:0",
        "[ff02::1%3]:0",
    ] {
        for scope in [Scope::Loopback, Scope::Lan] {
            assert!(
                Listener::bind(address.parse().unwrap(), scope).is_err(),
                "{address} {scope:?}"
            );
        }
    }
    assert!(Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Lan).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn unsupported_ipv6_lan_listener_is_withheld_before_bind() {
    let error = Listener::bind("[2001:db8::1]:0".parse().unwrap(), Scope::Lan)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(!remote_supported("[fe80::1%3]:0".parse().unwrap()));
    assert!(remote_supported("192.0.2.1:0".parse().unwrap()));
}

/// The only clock an idle listener keeps. A loopback listener has none. A LAN listener on macOS
/// must look again when the read of the attachments it judges by is [`ATTACHMENTS_REREAD`] old
/// (by when the read was made, however often it was judged by since), because Darwin drops a
/// routing message from a full socket and says nothing; where no read is kept it looks again
/// one [`ACCEPT_POLL`] later, the cadence before. On Linux it has none: a lost netlink message
/// is an error on its change source, which withdraws it. The attachment and its kept reads are
/// stand-ins on a clock this test holds; nothing is enumerated and the listener is never
/// rechecked.
///
/// NEGATIVE CONTROL (2026-11-12): with `unheard_look` giving every listener
/// `attachments_clock() + ACCEPT_POLL` (the timer before), the kept read's assert fails on macOS
/// (`left: Some(start + 200ms)`, `right: Some(start + 10s)`).
#[test]
fn an_idle_listeners_only_clock_is_the_expiry_of_the_read_it_judges_by() {
    use super::super::{ATTACHMENTS_REREAD, FIXED_NOW, kept_attachments};
    use std::time::Duration;

    let mut listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    assert_eq!(listener.validity_deadline(), None, "loopback");

    let start = Instant::now();
    FIXED_NOW.set(Some(start));
    listener.attachment = Some(Attachment {
        index: 3,
        name: "test-physical".into(),
        address: listener.local.ip(),
        netmask: "255.255.255.0".parse().unwrap(),
        flags: super::super::REQUIRED_FLAGS,
        physical: true,
    });
    let local = Some(listener.local);
    let judge = |at: Duration| {
        kept_attachments(|memo| memo.read(Some(7), start + at, local, None, || Ok(Vec::new())))
            .unwrap();
    };
    let on_macos = |at: Instant| cfg!(target_os = "macos").then_some(at);
    assert_eq!(
        listener.validity_deadline(),
        on_macos(start + ACCEPT_POLL),
        "no kept read"
    );
    judge(Duration::ZERO);
    judge(Duration::from_secs(3));
    assert_eq!(
        listener.validity_deadline(),
        on_macos(start + ATTACHMENTS_REREAD),
        "a kept read, judged by again 3 s on"
    );
    judge(ATTACHMENTS_REREAD);
    assert_eq!(
        listener.validity_deadline(),
        on_macos(start + 2 * ATTACHMENTS_REREAD),
        "read again at its expiry"
    );
    forget_attachments();
    assert_eq!(
        listener.validity_deadline(),
        on_macos(start + ACCEPT_POLL),
        "forgotten"
    );
    FIXED_NOW.set(None);
}

#[test]
fn withdrawal_is_permanent_and_service_drops_the_listening_socket() {
    let mut listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    assert!(listener.changes_fd().is_none());
    assert!(!listener.drain_changes().unwrap());
    let withdrawn = Arc::clone(&listener.withdrawn);
    let ready = || {
        aterm_uds::ready::any_interests_now(&[aterm_uds::ready::Watch::new(
            withdrawn.bell().unwrap(),
            aterm_uds::ready::Interest::Read,
        )])
        .unwrap()
    };
    assert!(ready().is_empty());
    let address = listener.local_addr().unwrap();
    listener.local.set_port(0);
    assert!(listener.recheck().is_err());
    assert_eq!(ready(), vec![0]);
    listener.local = address;
    assert!(
        listener.recheck().is_err(),
        "restoring an observation cannot restore a withdrawn listener"
    );
    assert_eq!(
        ready(),
        vec![0],
        "another observer cannot consume withdrawal"
    );
    let stopped = std::sync::Arc::new(AtomicBool::new(false));
    let event = stopped.clone();
    crate::fleet::serve_lan(
        listener,
        &crate::fleet::Listening {
            secret: crate::fleet::FleetSecret::new(&[7; 32]).unwrap(),
            scope: crate::fleet::FleetScope::Facts,
            node: None,
            serves: &[crate::fleet::FleetOp::Survey],
            slots: 2,
            per_source: 1,
            lifetime: aterm_spec::wait::MUST_HAPPEN,
        },
        &Stop::new(),
        |_, _| panic!("withdrawn listener must not admit a peer"),
        move |e| {
            if matches!(e, crate::fleet::FleetEvent::Stopped(_)) {
                assert!(
                    std::net::TcpStream::connect_timeout(&address, aterm_spec::wait::MUST_HAPPEN)
                        .is_err(),
                    "Stopped is delivered only after the kernel listener closes"
                );
                event.store(true, Ordering::Release);
            }
        },
    );
    assert!(stopped.load(Ordering::Acquire));
    assert_eq!(ready(), vec![0]);
    assert!(std::net::TcpStream::connect_timeout(&address, aterm_spec::wait::MUST_HAPPEN).is_err());
}

#[test]
fn explicit_loopback_accept_returns_a_rechecked_connection() {
    for address in ["127.0.0.1:0", "[::1]:0"] {
        let listener = Listener::bind(address.parse().unwrap(), Scope::Loopback).unwrap();
        let _peer = std::net::TcpStream::connect_timeout(
            &listener.local_addr().unwrap(),
            aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert!(
            crate::slots::listener_pending_within(&listener.tcp, aterm_spec::wait::MUST_HAPPEN)
                .unwrap()
        );
        listener.accept().unwrap().into_parts().1.recheck().unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    }
}

#[test]
fn dropping_a_listener_withdraws_its_workers_without_retaining_the_socket() {
    let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    let address = listener.local_addr().unwrap();
    let witness = listener.witness();
    witness.recheck().unwrap();
    let withdrawn = Arc::clone(&listener.withdrawn);
    let bell = withdrawn.bell().unwrap();
    drop(listener);
    assert_eq!(
        aterm_uds::ready::any_interests_now(&[aterm_uds::ready::Watch::new(
            bell,
            aterm_uds::ready::Interest::Read,
        )])
        .unwrap(),
        vec![0]
    );
    assert!(witness.recheck().is_err());
    assert!(std::net::TcpStream::connect_timeout(&address, aterm_spec::wait::MUST_HAPPEN).is_err());
}

#[test]
fn accepted_connection_cannot_outlive_its_listener_authority() {
    let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
    let _peer = std::net::TcpStream::connect_timeout(
        &listener.local_addr().unwrap(),
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert!(
        crate::slots::listener_pending_within(&listener.tcp, aterm_spec::wait::MUST_HAPPEN)
            .unwrap()
    );
    let (_, proof) = listener.accept().unwrap().into_parts();
    proof.recheck().unwrap();
    drop(listener);
    assert!(proof.recheck().is_err());
}
