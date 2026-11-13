// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn hint(addr: &str) -> aterm_secrets::discover::Advert {
    aterm_secrets::discover::Advert {
        instance: "opaque".into(),
        host: addr.into(),
        port: 7100,
        txt: vec![
            ("v".into(), "2".into()),
            ("id".into(), "n-0123456789abcdef".into()),
            ("port".into(), "7100".into()),
        ],
    }
}

#[test]
fn one_browse_owned_by_service_and_none_with_mesh_off() {
    let mut service = MeshDiscovery::new(true);
    let starts = std::cell::Cell::new(0);
    let start =
        |service: &'static str, bell: &std::sync::Arc<aterm_secrets::wake::Bell>, own: &[&str]| {
            starts.set(starts.get() + 1);
            aterm_secrets::discover::Watch::idle_for(service, bell, own)
        };
    let first = service.start_with("own", start).unwrap();
    let second = service.start_with("own", start).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    assert_eq!(starts.get(), 1);
    assert!(
        MeshDiscovery::new(false)
            .start_with("own", |_, _, _| panic!("mesh off"))
            .is_none()
    );
}

#[test]
fn address_changes_follow_one_injected_period_and_unproved_hints_never_enter_cache() {
    let mut service = MeshDiscovery::new(true);
    let now = Instant::now();
    let node = "n-0123456789abcdef";
    let first = "127.0.0.1:7100".parse().unwrap();
    let second = "127.0.0.2:7100".parse().unwrap();
    let hints = || vec![hint("peer.local")];
    assert!(service.refresh(now, hints, |_| Some((node.into(), first))));
    assert!(!service.refresh(
        now + DISCOVERY_PERIOD / 2,
        || panic!("not due"),
        |_| panic!("not due")
    ));
    assert!(service.refresh(now + DISCOVERY_PERIOD, hints, |_| Some((
        node.into(),
        second
    ))));
    assert_eq!(service.addresses.get(node), Some(&second));
    assert!(!service.refresh(now + DISCOVERY_PERIOD * 2, hints, |_| None));
    assert_eq!(service.addresses.get(node), Some(&second));
    let scratch = aterm_tempfile::tempdir().unwrap();
    aterm_uds::private_file::ensure_private_dir(scratch.path()).unwrap();
    service.save(scratch.path()).unwrap();
    assert_eq!(mesh_addresses(scratch.path()), vec![(node.into(), second)]);
    let mut restarted = MeshDiscovery::new(true);
    restarted.restore(scratch.path());
    assert_eq!(restarted.addresses, service.addresses);
    let mut legacy = MeshDiscovery::new(false);
    assert!(!legacy.refresh(
        now,
        || panic!("legacy resolves no mesh hints"),
        |_| panic!("legacy proves no mesh hints")
    ));
    assert!(legacy.addresses.is_empty());
}

#[test]
fn mesh_txt_has_only_protocol_node_and_port() {
    let advert = mesh_advert("opaque", "n-0123456789abcdef", 7123);
    assert_eq!(
        (advert.name.as_str(), advert.service.as_str(), advert.port),
        ("opaque", "_alab._tcp", 7123)
    );
    assert_eq!(
        advert.txt,
        [
            b"v=2".to_vec(),
            b"id=n-0123456789abcdef".to_vec(),
            b"port=7123".to_vec()
        ]
    );
    assert_eq!((advert.host, advert.addresses), (None, Vec::new()));
}

/// A held advert that is still wanted and still registered stays, so a tick registers
/// nothing; a changed one, or one the broker ended, is withdrawn and registered again; an
/// advert no longer wanted is withdrawn; a registration that fails leaves nothing held.
/// NEGATIVE CONTROL: a reconcile that ignores `ended` keeps an advert the broker dropped.
#[test]
fn a_held_advert_is_kept_while_wanted_and_registered_and_renewed_otherwise() {
    struct Held(std::rc::Rc<std::cell::Cell<bool>>);
    impl HeldAdvert for Held {
        fn ended(&mut self) -> bool {
            self.0.get()
        }
    }
    let ended = std::rc::Rc::new(std::cell::Cell::new(false));
    let registered = std::cell::Cell::new(0);
    let register = |_: &aterm_uds::lan::Advert| {
        registered.set(registered.get() + 1);
        Ok(Held(std::rc::Rc::clone(&ended)))
    };
    let first = mesh_advert("opaque", "n-0123456789abcdef", 7123);
    let mut held = None;
    reconcile_advert(&mut held, Some(first.clone()), &register);
    reconcile_advert(&mut held, Some(first.clone()), &register);
    assert_eq!(registered.get(), 1, "a held advert stays");
    ended.set(true);
    reconcile_advert(&mut held, Some(first.clone()), &register);
    assert_eq!(
        registered.get(),
        2,
        "one the broker ended is registered again"
    );
    ended.set(false);
    reconcile_advert(
        &mut held,
        Some(mesh_advert("opaque", "n-0123456789abcdef", 7124)),
        &register,
    );
    assert_eq!(registered.get(), 3, "a changed advert is registered again");
    reconcile_advert(&mut held, None, &register);
    assert!(held.is_none(), "an advert no longer wanted is withdrawn");
    let refused = |_: &aterm_uds::lan::Advert| {
        Err::<Held, _>(aterm_uds::lan::LanError::Unavailable(
            aterm_uds::lan::Unavailable::NotInstalled,
        ))
    };
    reconcile_advert(&mut held, Some(first), &refused);
    assert!(held.is_none());
}

/// A stand-in broker at a fresh socket that registers each advert it is asked for and holds
/// that connection until the test says to drop it: the seam pointing this thread's client
/// there, and the sender that ends the next held registration.
fn registering_broker() -> (
    aterm_uds::lan::Seam,
    aterm_tempfile::TempDir,
    std::sync::mpsc::Sender<()>,
) {
    use aterm_uds::lan::wire::{self, Frame, Reader};
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("broker.sock");
    let listener = aterm_uds::CtlListener::bind(&path).unwrap();
    let (end, ends) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        while let Ok((stream, _)) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            let hello = Frame::Hello {
                protocol: aterm_uds::lan::PROTOCOL,
                build: "stand-in".into(),
            };
            wire::send(&stream, &hello).unwrap();
            let mut reader = Reader::new(0);
            let deadline = Some(Instant::now() + Duration::from_secs(60));
            assert!(matches!(
                reader.frame(&stream, deadline),
                Ok(Some(Frame::Hello { .. }))
            ));
            let Ok(Some(Frame::Request(wire::Request::Register(advert)))) =
                reader.frame(&stream, deadline)
            else {
                panic!("not a registration");
            };
            let registered = aterm_uds::lan::Event::Registered {
                name: advert.name,
                service: advert.service,
                domain: "local.".into(),
            };
            wire::send(&stream, &Frame::Event(registered)).unwrap();
            if ends.recv().is_err() {
                return;
            }
            drop(stream);
        }
    });
    let seam = aterm_uds::lan::Seam {
        socket: Some(path),
        ..aterm_uds::lan::Seam::default()
    };
    (seam, root, end)
}

/// A live registration is not ended; once the broker drops its connection (a restart, an
/// upgrade) it is, and the next reconcile registers the advert again.
/// NEGATIVE CONTROL: `ended` as a zero-wait `next_within` (which returns before it polls)
/// never sees the broker's close, so the wait for it fails at its hang detector.
#[test]
fn a_registration_the_broker_dropped_is_ended_and_registered_again() {
    let (seam, _root, end) = registering_broker();
    aterm_uds::lan::seam_here(seam, || {
        let register = aterm_uds::lan::broker::register;
        let advert = mesh_advert("opaque", "n-0123456789abcdef", 7123);
        let mut held = None;
        reconcile_advert(&mut held, Some(advert.clone()), &register);
        let (registration, _) = held.as_mut().expect("registered");
        assert!(!registration.ended(), "a live registration is held");
        end.send(()).unwrap();
        let hang = Instant::now() + Duration::from_secs(60);
        let (registration, _) = held.as_mut().unwrap();
        while !registration.ended() {
            assert!(Instant::now() < hang, "the broker's close was never seen");
            std::thread::sleep(Duration::from_millis(10));
        }
        reconcile_advert(&mut held, Some(advert), &register);
        let (registration, _) = held.as_mut().expect("registered again");
        assert!(!registration.ended(), "the renewed registration is live");
    });
}
