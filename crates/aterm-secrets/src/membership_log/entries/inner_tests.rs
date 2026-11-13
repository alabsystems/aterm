// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeMap;

/// Epoch keys by id, as a reader holds them.
type Keys = BTreeMap<RecordId, Zeroizing<[u8; 32]>>;

const TOKEN: &[u8] = concat!("sk-ant-", "oat01-inner-fixture_value").as_bytes();

#[test]
fn a_sealed_value_opens_under_its_epoch_and_names_it_elsewhere() {
    let (network, epoch, other) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    let epoch_key = Zeroizing::new([7u8; 32]);
    let stored = store(&network, &epoch, &epoch_key, "n", TOKEN).unwrap();
    assert!(stored.starts_with(&MAGIC));
    assert!(
        !stored.windows(8).any(|w| TOKEN.windows(8).any(|t| t == w)),
        "the stored value holds no run of the token"
    );
    let mut keys = Keys::new();
    keys.insert(epoch, epoch_key.clone());
    assert_eq!(
        load(&network, |e| keys.get(e), "n", &stored).as_slice(),
        TOKEN
    );
    // Another name, network or key opens nothing: it reads unsealed.
    for (network, name) in [(network, "m"), ([9u8; 32], "n")] {
        let loaded = load(&network, |e| keys.get(e), name, &stored);
        assert_eq!(of(&loaded), Form::Unsealed);
    }
    // Without the sealing epoch's key the bytes are carried, and read unreadable.
    let none = Keys::new();
    let carried = load(&network, |e| none.get(e), "n", &stored);
    assert_eq!(of(&carried), Form::Unreadable(epoch));
    assert_eq!(
        store(&network, &other, &epoch_key, "n", &carried)
            .unwrap()
            .as_slice(),
        stored.as_slice(),
        "a carried seal is written verbatim"
    );
    // A raw value is tagged unsealed, and written back without the tag.
    let raw = load(&network, |e| keys.get(e), "n", TOKEN);
    assert_eq!(of(&raw), Form::Unsealed);
    assert_eq!(
        store(&network, &epoch, &epoch_key, "n", &raw)
            .unwrap()
            .as_slice(),
        TOKEN
    );
    #[cfg(unix)]
    assert_eq!(stored_len(TOKEN), stored.len());
}
