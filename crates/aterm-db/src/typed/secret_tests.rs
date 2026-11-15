// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
const LOCATION: Cell<'static> = Cell {
    table: "alab/registry",
    collection: "mailbox",
    row: "mb-fixture",
    column: "password",
};

/// A sealed token shows as the mask wherever a table shows a cell: by its column's type and, for
/// a reader that does not know the column, by the token's own shape.
#[test]
fn typed_and_raw_outputs_mask_a_sealed_value() {
    use crate::typed::{Ty, grid};
    let token = Secret::new("synthetic-password".into())
        .seal(&[42; 32], &[7; 32], LOCATION)
        .unwrap();
    let value = aterm_json::Value::String(token);
    assert_eq!(
        grid::show(
            &Ty::Secret,
            &aterm_json::Value::String("short-plain".into())
        ),
        MASK
    );
    assert_eq!(grid::show(&Ty::Secret, &value), MASK);
    assert_eq!(
        grid::raw_shown(&value),
        aterm_json::Value::String(MASK.into())
    );
}

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
    assert!(!is_sealed(value)); // Negative control: plaintext is no sealed token, so no roll opens it.
}

#[test]
fn a_secret_meets_a_table_only_as_the_name_of_its_store_entry() {
    use crate::typed::{Ty, cell, guard};
    let value = concat!("gh", "p_syntheticCredentialForSecretTest1234567890");
    let store = Memory::default();
    let held = store_cell(
        &store,
        "mailbox",
        "totp",
        "mb-fixture",
        &Secret::new(value.into()),
    )
    .unwrap();
    let entry = entry_of(&held).unwrap();
    assert!(bound("mailbox", "totp", "mb-fixture", entry), "{entry}");
    assert!(
        entry.starts_with("alab-mailbox-totp-mb-fixture."),
        "{entry}"
    );
    assert!(cell::admissible(&Ty::SecretRef, &held));
    assert!(!aterm_json::to_string(&held).unwrap().contains(value));
    assert_eq!(store.held(entry).as_deref(), Some(value));
    // Negative controls: the value itself, or an earlier build's sealed token, is no reference.
    assert!(!cell::admissible(
        &Ty::SecretRef,
        &aterm_json::Value::String(value.into())
    ));
    let token = Secret::new(value.into())
        .seal(&[42; 32], &[7; 32], LOCATION)
        .unwrap();
    assert!(!cell::admissible(
        &Ty::SecretRef,
        &aterm_json::Value::String(token)
    ));
    // A has-password flag never reaches the store.
    assert!(
        store_cell(
            &store,
            "address",
            "password",
            "ad-x",
            &Secret::new("yes".into()),
        )
        .is_err()
    );
    assert_eq!(store.every().len(), 1);
    assert!(guard::text(value, 512).is_err());
    assert!(guard::note_cell("Annual plan renewed 2026-11-01; used for the design team. Check billing next November.", 512).is_ok());
}

#[test]
fn a_phone_number_is_ordinary_data_in_its_own_column_only() {
    use crate::typed::{Ty, cell, guard};
    // The stored form: digits only, 4 to 15 of them.
    for (text, admitted) in [
        ("2025550149", true),
        ("0149", true),
        ("123456789012345", true),
        ("1234567890123456", false),
        ("149", false),
        ("+12025550149", false),
        ("202-555", false),
        ("(202) 555-0149", false),
        ("", false),
    ] {
        assert_eq!(
            cell::admissible(&Ty::Phone, &aterm_json::Value::String(text.into())),
            admitted,
            "{text:?}"
        );
    }
    // Free text still keeps a number out: it belongs in the phone row.
    assert!(guard::text("+1 (202) 555-0149", 512).is_err());
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

/// A cell names its own entry or that name with a 16-hex tag; a write's tag is fresh randomness,
/// so two writes never share an entry.
/// NEGATIVE CONTROLS: another row's, column's or collection's entry, a dense `.<n>` suffix, a tag
/// of the wrong length or case, and an entry of no cell are never this cell's.
#[test]
fn a_secret_cell_names_only_its_own_entry_and_its_tagged_ones() {
    let coords = ("address", "password", "ad-x");
    let own = entry_name("address", "password", "ad-x").unwrap();
    assert!(bound("address", "password", "ad-x", &own));
    let tagged = tagged_entry(coords, [0xab; 8]).unwrap();
    assert_eq!(tagged, format!("{own}.abababababababab"));
    assert!(bound("address", "password", "ad-x", &tagged));
    let store = Memory::default();
    let (one, two) = (
        fresh_entry(&store, coords).unwrap(),
        fresh_entry(&store, coords).unwrap(),
    );
    assert!(
        bound("address", "password", "ad-x", &one) && one != two,
        "{one} {two}"
    );
    for other in [
        entry_name("address", "password", "ad-y").unwrap(),
        entry_name("address", "password", "ad-x-2").unwrap(),
        entry_name("mailbox", "totp", "ad-x").unwrap(),
        entry_name("address", "note", "ad-x").unwrap(),
        format!("{own}.2"),
        format!("{own}.0123456789abcde"),
        format!("{own}.0123456789ABCDEF"),
        format!("{own}.0123456789abcdef0"),
        format!("{own}-2"),
        format!("{own}x"),
        "gh_access_token".to_owned(),
        "release-credentials".to_owned(),
    ] {
        assert!(!bound("address", "password", "ad-x", &other), "{other}");
    }
    // A row key holds `-`, so a tagged entry of `ad-x` is never `ad-x-2`'s.
    assert!(!bound("address", "password", "ad-x-2", &tagged));
}

#[test]
fn a_reader_refuses_a_reference_to_another_vault_or_another_cells_entry() {
    let cell = Cell {
        table: "alab/registry",
        collection: "address",
        row: "ad-x",
        column: "password",
    };
    let own = entry_name("address", "password", "ad-x").unwrap();
    let good = reference(Memory::VAULT, &own);
    assert_eq!(bound_entry(&good, cell, Memory::VAULT).unwrap(), own);
    // Negative controls: the same entry in another vault, and a credential's entry in this one.
    assert!(bound_entry(&reference("other-vault", &own), cell, Memory::VAULT).is_err());
    let stolen = reference(Memory::VAULT, "gh_access_token");
    let refused = bound_entry(&stolen, cell, Memory::VAULT)
        .unwrap_err()
        .to_string();
    assert!(!refused.contains("gh_access_token"));
    assert!(
        bound_entry(
            &aterm_json::json!({"vault": Memory::VAULT}),
            cell,
            Memory::VAULT
        )
        .is_err()
    );
}

/// A retire removes exactly the entries it is given and pays each; one whose removal fails stays
/// owed. A sweep never touches an entry a live run owes (another run's put, not yet committed),
/// and once that run has ended removes each owed entry no cell names and pays, keeping, each one
/// a cell names. Neither touches an entry never owed.
/// NEGATIVE CONTROLS: the entry another writer put and never owed here survives both; with
/// `sweep` reading every owed entry instead of `orphaned`, `` every owing run is live `` fails;
/// with `store_cell` not owing its entry, the last sweep removes one entry, not two.
#[test]
fn a_retire_removes_exactly_its_entries_and_a_sweep_only_what_ended_runs_left() {
    use std::collections::BTreeSet;
    let store = Memory::default();
    let uncommitted = store_cell(
        &store,
        "address",
        "password",
        "ad-x",
        &Secret::new("uncommitted-value-1".into()),
    )
    .unwrap();
    let uncommitted = entry_of(&uncommitted).unwrap().to_owned();
    let landed = store_cell(
        &store,
        "address",
        "password",
        "ad-y",
        &Secret::new("landed-value-2".into()),
    )
    .unwrap();
    let landed = entry_of(&landed).unwrap().to_owned();
    let replaced = test_entry(("address", "password", "ad-y"), 2);
    store
        .put(&replaced, &Secret::new("replaced-value-3".into()))
        .unwrap();
    store.owe(std::slice::from_ref(&replaced)).unwrap();
    store.stick(&replaced);
    let foreign = test_entry(("address", "password", "ad-z"), 1);
    store
        .put(&foreign, &Secret::new("foreign-value-4".into()))
        .unwrap();
    let named = BTreeSet::from([landed.clone()]);

    let retired = retire(&store, &BTreeSet::from([replaced.clone()])).unwrap();
    assert_eq!(
        retired,
        Settled {
            removed: 0,
            left: 1
        }
    );
    assert_eq!(store.held(&replaced).as_deref(), Some("replaced-value-3"));
    assert!(store.owed().contains(&replaced));
    assert_eq!(
        sweep(&store, || Ok(Some(named.clone()))).unwrap(),
        Settled::default(),
        "every owing run is live"
    );
    assert_eq!(
        store.held(&uncommitted).as_deref(),
        Some("uncommitted-value-1"),
        "a live run's put survives"
    );

    store.end_runs();
    store.unstick(&replaced);
    assert_eq!(
        sweep(&store, || Ok(None)).unwrap(),
        Settled::default(),
        "a sweep the generations stop collects nothing"
    );
    assert_eq!(
        store.held(&uncommitted).as_deref(),
        Some("uncommitted-value-1")
    );
    assert_eq!(
        sweep(&store, || Ok(Some(named.clone()))).unwrap(),
        Settled {
            removed: 2,
            left: 0
        }
    );
    assert_eq!(
        store.held(&uncommitted),
        None,
        "an uncommitted put is collected"
    );
    assert_eq!(store.held(&replaced), None);
    assert_eq!(store.held(&landed).as_deref(), Some("landed-value-2"));
    assert!(store.owed().is_empty());
    assert_eq!(store.held(&foreign).as_deref(), Some("foreign-value-4"));
}
