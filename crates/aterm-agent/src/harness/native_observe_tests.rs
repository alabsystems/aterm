// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn process(pid: u32) -> Process {
    Process {
        pid,
        birth: u64::from(pid) * 10,
        parent: 22,
        group: pid,
        session: 22,
        image: Image {
            path: b"/private/vendor-\xff".to_vec(),
            device: 7,
            inode: 8,
        },
    }
}

fn registration() -> Registration {
    Registration {
        id: "1".repeat(32),
        host_epoch: "2".repeat(32),
        vendor: process(33),
        shell: process(22),
    }
}

fn binding() -> ObservationBinding {
    ObservationBinding {
        id: "1".repeat(32),
        host_epoch: "2".repeat(32),
        capability: "3".repeat(64),
    }
}

#[test]
fn observation_codec_preserves_exact_processes_and_unix_bytes() {
    for request in [
        Request::Register(registration()),
        Request::Refresh(binding()),
    ] {
        let encoded = request.encode().unwrap();
        assert_eq!(Request::parse(&encoded), Some(request));
        assert!(super::super::native_act::Request::parse(&encoded).is_none());
    }
    let mut largest = registration();
    largest.vendor.image.path.resize(MAX_PATH, b'x');
    largest.shell.image.path.resize(MAX_PATH, b'y');
    let encoded = Request::Register(largest.clone()).encode().unwrap();
    assert!(encoded.len() < MAX_REQUEST);
    assert_eq!(
        Request::parse(&encoded),
        Some(Request::Register(largest.clone()))
    );
    largest.vendor.image.path.push(b'x');
    assert!(Request::Register(largest).encode().is_none());
}

#[test]
fn saved_native_outcome_is_data_only_canonical_and_bound_to_the_same_registration() {
    let saved = SavedObservation {
        registration: registration(),
        binding: binding(),
    };
    let encoded = saved.encode().unwrap();
    assert_eq!(SavedObservation::parse(&encoded), Some(saved.clone()));
    let receipt = format!(
        "OK native-act state=committed claim=builtin revision=- observation={}\n",
        aterm_codec::hex::encode_lower(encoded.as_bytes())
    );
    assert_eq!(
        super::super::native_act::Reply::parse(&receipt),
        Some(super::super::native_act::Reply::RestartCommitted(Box::new(
            saved.clone()
        )))
    );
    assert!(super::super::native_act::Request::parse(&encoded).is_none());
    assert!(
        super::super::native_act::Reply::parse(&receipt.replace("claim=builtin", "claim=lost"))
            .is_none()
    );
    let mut stale = saved;
    stale.binding.host_epoch = "9".repeat(32);
    assert!(stale.encode().is_none());
    assert!(SavedObservation::parse(&(encoded.clone() + "\n")).is_none());
    assert!(SavedObservation::parse(&(encoded + "\nextra=1")).is_none());
}

#[test]
fn malformed_observations_refuse_the_complete_message() {
    let valid = Request::Register(registration()).encode().unwrap();
    for malformed in [
        format!("{valid} id={}", "1".repeat(32)),
        format!("{valid} native-operation=admitted"),
        format!("{valid}\n"),
        format!("{valid}\0"),
        valid.replace("vendor-pid=33", "vendor-pid=033"),
        valid.replace("vendor-pid=33", "vendor-pid=-33"),
        valid.replace("vendor-birth=330", "vendor-birth=0"),
        valid.replace("vendor-group=33", "vendor-group=1"),
        valid.replace("v=1", "v=2"),
        valid.replace("v=1 ", ""),
        valid.replace("vendor-path=2f", "vendor-path=00"),
        valid.replace("vendor-path=2f", "vendor-path=2F"),
        valid[..valid.len() - 1].to_string(),
        "x".repeat(MAX_REQUEST + 1),
    ] {
        assert!(Request::parse(&malformed).is_none(), "{malformed:?}");
    }
    let mut invalid = registration();
    invalid.vendor.image.path.push(0);
    assert!(Request::Register(invalid).encode().is_none());
    let mut invalid = registration();
    invalid.id.push('0');
    assert!(Request::Register(invalid).encode().is_none());
}

#[test]
fn observation_acknowledgement_is_bound_to_registration_or_exact_refresh() {
    let request = Request::Register(registration());
    let accepted = Reply::Registered(binding()).encode().unwrap();
    assert_eq!(
        acknowledged(&request, &accepted),
        Some(Reply::Registered(binding()))
    );
    assert_eq!(
        acknowledged(&Request::Refresh(binding()), &accepted),
        Some(Reply::Registered(binding()))
    );
    for change in ["id", "epoch", "cap"] {
        let mut other = binding();
        match change {
            "id" => other.id = "a".repeat(32),
            "epoch" => other.host_epoch = "a".repeat(32),
            "cap" => other.capability = "a".repeat(64),
            _ => unreachable!(),
        }
        let line = Reply::Registered(other).encode().unwrap();
        assert!(acknowledged(&Request::Refresh(binding()), &line).is_none());
        if change != "cap" {
            assert!(acknowledged(&request, &line).is_none());
        }
    }
    for malformed in [
        format!("{} cap={}\n", accepted.trim_end(), "3".repeat(64)),
        format!("{} admitted=1\n", accepted.trim_end()),
        format!("{accepted}\n"),
        "OK native-act state=admitted\n".into(),
    ] {
        assert!(Reply::parse(&malformed).is_none());
    }
    assert_eq!(
        acknowledged(&request, "OK native-observe state=unknown v=1\n"),
        Some(Reply::Unknown)
    );
    assert!(!format!("{:?}", binding()).contains(&"3".repeat(64)));
}
