// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn the_alab_signer_is_the_ed25519_key_of_the_vault_m22_made() {
    let key = crate::b64::decode_padded(super::ALAB_SIGNER).expect("base64");
    let key = <[u8; 32]>::try_from(key.as_slice()).expect("32 bytes");
    assert_eq!(
        crate::envelope::fingerprint(&key),
        "fed4-97da-a265-0615-9f56"
    );
}

#[test]
fn every_retired_alab_key_is_a_key_and_none_is_the_signer() {
    for text in super::ALAB_RETIRED {
        let key = crate::b64::decode_padded(text).expect("canonical padded base64");
        assert_eq!(key.len(), 32, "{text}");
        assert_ne!(*text, super::ALAB_SIGNER, "the current key is retired");
    }
    let compiled = crate::envelope::Retired::compiled();
    assert_eq!(compiled.keys.len(), super::ALAB_RETIRED.len());
    assert_eq!(
        compiled.current.map(|key| crate::b64::encode_padded(&key)),
        Some(super::ALAB_SIGNER.to_string())
    );
}

#[test]
fn the_compiled_alab_genesis_is_a_digest_when_there_is_one() {
    let compiled = crate::membership_log::genesis::digest_compiled().expect("64 hex digits");
    assert_eq!(compiled.is_some(), super::ALAB_GENESIS.is_some());
}

/// A12's predicate: each fact alone marks an identity, the first that holds names it, and
/// only a release build on a Mac's own hardware makes an unmarked one.
#[test]
fn each_lab_fact_marks_an_identity_and_a_release_build_on_hardware_none() {
    use super::{LabMark, Making, lab_identity};
    assert_eq!(lab_identity(Making::default()), None);
    let one = |fact: fn(&mut Making)| {
        let mut making = Making::default();
        fact(&mut making);
        lab_identity(making)
    };
    assert_eq!(one(|m| m.test_binary = true), Some(LabMark::TestBinary));
    assert_eq!(one(|m| m.debug_build = true), Some(LabMark::DebugBuild));
    assert_eq!(one(|m| m.lab_lane = true), Some(LabMark::LabLane));
    assert_eq!(
        one(|m| m.virtual_machine = true),
        Some(LabMark::VirtualMachine)
    );
    let all = Making {
        test_binary: true,
        debug_build: true,
        lab_lane: true,
        virtual_machine: true,
    };
    assert_eq!(lab_identity(all), Some(LabMark::TestBinary));
    for mark in [
        LabMark::TestBinary,
        LabMark::DebugBuild,
        LabMark::LabLane,
        LabMark::VirtualMachine,
    ] {
        assert_eq!(LabMark::of_word(mark.word()), mark);
    }
    assert_eq!(LabMark::of_word("a-later-reason"), LabMark::Unnamed);
}

#[test]
fn the_server_advertises_the_name_and_port_the_default_url_reaches() {
    let label = super::alab_advertised();
    assert_eq!(label, "alab-secrets");
    assert!(crate::envelope::valid_name(label));
    assert_eq!(
        super::ALAB_DEFAULT_URLS.first().copied(),
        Some(super::ALAB_DEFAULT_URL)
    );
    assert!(
        super::ALAB_DEFAULT_URL
            .starts_with(&format!("http://{label}.local:{}/v1/", super::DEFAULT_PORT)),
        "{}",
        super::ALAB_DEFAULT_URL
    );
}
