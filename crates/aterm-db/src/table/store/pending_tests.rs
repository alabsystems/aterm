// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn write(shape: Shape, byte: u8) -> Pending {
    Pending {
        shape,
        epoch: [byte; 32],
        payload: Zeroizing::new(vec![byte; 3]),
    }
}

#[test]
fn a_pending_set_round_trips_and_opens_only_under_its_key() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().to_path_buf();
    let writes = vec![write(Shape::Catalog, 1), write(Shape::Own, 2)];
    save(&dir, &[7; 32], &writes).unwrap();
    assert_eq!(load(&dir, &[7; 32]).unwrap(), writes);
    assert!(
        load(&dir, &[8; 32]).is_err(),
        "another Mac's key opens nothing"
    );
    save(&dir, &[7; 32], &[]).unwrap();
    assert!(load(&dir, &[7; 32]).unwrap().is_empty());
    assert!(!path(&dir).exists());
}

#[test]
fn the_set_is_bounded() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().to_path_buf();
    let writes = vec![write(Shape::Liability, 3); MAX_PENDING + 1];
    assert!(save(&dir, &[7; 32], &writes).is_err());
    assert!(save(&dir, &[7; 32], &writes[..MAX_PENDING]).is_ok());
}
