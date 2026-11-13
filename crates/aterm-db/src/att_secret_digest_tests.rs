// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::database::{Candidate, Cell as Stored};
use crate::typed::{
    Column, Derive, Kind, Ty, Writers,
    secret::{Cell, Secret},
};

const COLLECTION: Collection = Collection {
    name: "mailbox",
    version: 1,
    kind: Kind::Cells,
    writers: Writers::Owner,
    prefix: "mb",
    derive: Derive::Random,
    columns: &[
        Column::new("row", Ty::Enum(&["live", "ended"])),
        Column::new("password", Ty::Secret).null(),
    ],
    unique: &[],
    max_row: 1024,
};

fn cells(password: &str, epoch: Digest, key: [u8; 32]) -> Cells {
    let token = Secret::new(password.into())
        .seal(
            &key,
            &epoch,
            Cell {
                table: "alab/registry",
                collection: "mailbox",
                row: "mb-test",
                column: "password",
            },
        )
        .unwrap();
    BTreeMap::from([(
        ("mailbox".into(), "mb-test.password".into()),
        Stored {
            stopped: false,
            candidates: vec![Candidate {
                operation: [1; 32],
                value: cell::CellValue::owner(1, Value::String(token)).encode(),
            }],
        },
    )])
}

#[test]
fn digest_compares_logical_values_and_names_its_epoch() {
    let epoch = [2; 32];
    let key = [3; 32];
    let digest = |cells: &Cells, epoch: &Digest, key: &[u8; 32]| {
        secret_digest("alab/registry", &COLLECTION, cells, epoch, key, |_| {
            Ok(zeroize::Zeroizing::new([3; 32]))
        })
        .unwrap()
    };
    let first = cells("fixture-one", epoch, key);
    let resealed = cells("fixture-one", epoch, key);
    assert_ne!(first, resealed);
    let one = digest(&first, &epoch, &key);
    assert_eq!(one, digest(&resealed, &epoch, &key));
    assert_ne!(
        one.digest,
        digest(&cells("fixture-two", epoch, key), &epoch, &key).digest
    );
    let rotated = digest(&first, &[4; 32], &[5; 32]);
    assert_ne!(one.digest, rotated.digest);
    assert_ne!(one.epoch, rotated.epoch);
    assert_eq!(one.compare(&rotated), DigestComparison::DifferentEpoch);
    assert_eq!((one.rows, one.with_secret), (1, 1));
    assert!(one.to_string().starts_with(&format!("epoch={}", one.epoch)));
    assert!(!one.to_string().contains("fixture-one"));
}
