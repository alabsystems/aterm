// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
const LOCATION: Cell<'static> = Cell {
    table: "alab/registry",
    collection: "mailbox",
    row: "mb-fixture",
    column: "password",
};

#[test]
fn formats_mask_and_old_readers_get_only_a_seal() {
    let value = "A!b?C#d$E%f^G&h*J(k)";
    let secret = Secret::new(value.into());
    for shown in [
        format!("{secret}"),
        format!("{secret:?}"),
        aterm_json::to_string(&secret).unwrap(),
    ] {
        assert!(!shown.contains(value));
        assert!(shown.contains(MASK));
    }
    let token = secret.seal(&[42; 32], &[7; 32], LOCATION).unwrap();
    assert!(is_sealed(&token));
    for part in value.as_bytes().windows(4) {
        assert!(!token.as_bytes().windows(4).any(|word| word == part));
    }
    assert_eq!(
        open(&token, &[42; 32], LOCATION).unwrap().plaintext(),
        value
    );
    assert!(open(&token, &[41; 32], LOCATION).is_err());
    assert!(
        open(
            &token,
            &[42; 32],
            Cell {
                row: "mb-other",
                ..LOCATION
            }
        )
        .is_err()
    );
    assert!(!is_sealed(value)); // Negative control: plaintext cannot enter a Secret cell.
}

#[test]
fn secret_guard_exemption_and_ordinary_notes() {
    use crate::typed::{Ty, cell, guard};
    let value = concat!("gh", "p_syntheticCredentialForSecretTest1234567890");
    let token = Secret::new(value.into())
        .seal(&[42; 32], &[7; 32], LOCATION)
        .unwrap();
    assert!(cell::admissible(
        &Ty::Secret,
        &aterm_json::Value::String(token)
    ));
    assert!(!cell::admissible(
        &Ty::Secret,
        &aterm_json::Value::String(value.into())
    ));
    assert!(guard::text(value, 512).is_err());
    assert!(guard::text("+1 (202) 555-0149", 512).is_err());
    assert!(guard::note_cell("Annual plan renewed 2026-11-01; used for the design team. Check billing next November.", 512).is_ok());
}

#[test]
fn typed_and_raw_outputs_mask_even_when_addresses_are_revealed() {
    use crate::typed::{Ty, grid};
    let token = Secret::new("synthetic-password".into())
        .seal(&[42; 32], &[7; 32], LOCATION)
        .unwrap();
    let value = aterm_json::Value::String(token);
    for show in [false, true] {
        assert_eq!(
            grid::show(
                &Ty::Secret,
                &aterm_json::Value::String("short-plain".into()),
                show
            ),
            MASK
        );
        assert_eq!(grid::show(&Ty::Secret, &value, show), MASK);
        assert_eq!(
            grid::raw_masked(&value, show),
            aterm_json::Value::String(MASK.into())
        );
    }
}

#[test]
#[cfg(unix)]
fn secret_reads_on_another_mac_and_after_audience_rotation() {
    use crate::table::{Session, tests::Lab};
    use aterm_secrets::commons::roster::Role;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let scope = session.scope().unwrap();
    let basis = session.secret_basis();
    let token = Secret::new("isolated-fixture".into())
        .seal(
            &session.secret_key(scope, &basis).unwrap(),
            &basis,
            LOCATION,
        )
        .unwrap();
    drop(session);
    drop(bootstrap);
    let read = || {
        let bootstrap = lab.bootstrap(1);
        let session = Session::open(&bootstrap).unwrap();
        let key = session.secret_key(scope, &basis).unwrap();
        assert_eq!(
            open(&token, &key, LOCATION).unwrap().plaintext(),
            "isolated-fixture"
        );
    };
    read();
    lab.rotate(&[Role::Writer, Role::Writer]);
    read();
}
