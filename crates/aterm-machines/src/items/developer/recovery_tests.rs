// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::Fake;

#[test]
fn a_replacement_owner_cannot_witness_an_unverifiable_original_generation() {
    let sys = Fake::root();
    let own = std::process::id();
    let other = own.saturating_add(1);
    sys.born(own, 123).born(other, 456).running(&[]);
    let placeholder = CommandLineTools::hold_placeholder(&sys).unwrap().unwrap();
    let custody = Custody {
        placeholder,
        network: None,
    };
    sys.on_owned_read(move |sys, path| {
        if path == std::path::Path::new(super::super::PLACEHOLDER) {
            sys.file(
                path.to_str().unwrap(),
                aterm_types::macos_setup::placeholder_stamp(other, 456).as_bytes(),
            );
            // The original generation is no longer verifiable. The replacement is live.
            sys.born(own, 0);
        }
    });
    assert!(
        matches!(
            custody.check(),
            Err(Failure {
                class: Unfinished::Unread,
                ..
            })
        ),
        "a replacement owner's liveness must not vouch for the original custodian"
    );
    drop(custody);
    assert_eq!(
        sys.written(super::super::PLACEHOLDER).unwrap(),
        aterm_types::macos_setup::placeholder_stamp(other, 456).into_bytes()
    );
}
