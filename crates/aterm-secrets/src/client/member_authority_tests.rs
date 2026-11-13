// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::log_entries_tests::two_macs;
use crate::client::{Stored, load_records, load_state, save_state};
use crate::crossing::RecordsBound;
use crate::members::bundle::Reader;
use crate::text::Audience;
use std::cell::Cell;

#[test]
fn authority_is_public_current_bounded_and_read_only() {
    let macs = two_macs(&[(
        Audience::Everyone,
        "private-metadata",
        b"private-vault-value",
    )]);
    let log = load_records(&macs.m22).unwrap().log().unwrap().clone();
    let Body::Genesis { root, .. } = log.genesis().body() else {
        panic!("fixture genesis");
    };
    let device = Device::open(&macs.m7).unwrap().unwrap();
    let request = device.request(root.network, root.authority).unwrap();
    let fresh = Cell::new(false);
    let mut proving = Proving {
        request: &request,
        node: [7; 32],
        name: "joining",
        reader: Reader::BoundedLog(RecordsBound::uncrossed()),
        fleet: false,
        lan: true,
        lab: false,
        fresh: &fresh,
    };
    let before = std::fs::read(macs.m22.state_file()).unwrap();
    let records = std::fs::read(macs.m22.records_file()).unwrap();
    let witness = member_authority(&macs.m22, &proving).unwrap();
    assert!(!witness.is_empty());
    assert!(witness.len() <= MAX_RECORDS_STREAM);
    assert!(
        !witness
            .windows(b"private-vault-value".len())
            .any(|part| part == b"private-vault-value")
    );
    assert!(
        !witness
            .windows(b"private-metadata".len())
            .any(|part| part == b"private-metadata")
    );
    let mut verified = crate::membership_log::Store::with_compiled(Some(*log.network()));
    verified.ingest(&witness).unwrap();
    assert_eq!(verified.log().unwrap().heads(), log.heads());
    assert_eq!(std::fs::read(macs.m22.state_file()).unwrap(), before);
    assert_eq!(std::fs::read(macs.m22.records_file()).unwrap(), records);
    assert!(!fresh.get(), "public evidence signs no admission");

    proving.lab = true;
    let refused = crate::client::with_live_genesis(true, || member_authority(&macs.m22, &proving))
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains(crate::members::desk::LAB_REFUSED)
    );
    proving.lab = false;
    let foreign = device.request([8; 16], [9; 32]).unwrap();
    proving.request = &foreign;
    assert!(member_authority(&macs.m22, &proving).is_err());
    proving.request = &request;
    proving.reader = Reader::Files;
    assert!(member_authority(&macs.m22, &proving).is_err());
    proving.reader = Reader::BoundedLog(RecordsBound::uncrossed());
    let held = StateLock::take(&macs.m22).unwrap();
    assert!(member_authority(&macs.m22, &proving).is_err());
    drop(held);
    let mut state = load_state(&macs.m22).unwrap();
    state.failed = true;
    save_state(&macs.m22, &state).unwrap();
    assert!(member_authority(&macs.m22, &proving).is_err());
    state.failed = false;
    state.membership = Stored::Pending;
    save_state(&macs.m22, &state).unwrap();
    assert!(member_authority(&macs.m22, &proving).is_err());
    macs.server.stop();
}
