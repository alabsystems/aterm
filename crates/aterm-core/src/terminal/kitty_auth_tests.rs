// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

const NON_DIRECT: [KittyMedium; 3] = [
    KittyMedium::File,
    KittyMedium::TempFile,
    KittyMedium::SharedMemory,
];

#[test]
fn nothing_mints_without_a_resolver() {
    let slot = KittyResolverSlot::new();
    for medium in NON_DIRECT {
        assert!(slot.try_mint(medium).is_none(), "{medium:?}");
    }
}

#[test]
fn an_uninstalled_resolver_mints_nothing() {
    let mut slot = KittyResolverSlot::new();
    slot.install(Box::new(|_, _| Some(vec![1])));
    slot.uninstall();
    assert!(!slot.installed());
    for medium in NON_DIRECT {
        assert!(slot.try_mint(medium).is_none(), "{medium:?}");
    }
}

#[test]
fn a_direct_medium_never_mints() {
    let mut slot = KittyResolverSlot::new();
    slot.install(Box::new(|_, _| Some(vec![1])));
    assert!(slot.try_mint(KittyMedium::Direct).is_none());
}

#[test]
fn a_minted_capability_reaches_the_resolver() {
    let mut slot = KittyResolverSlot::new();
    slot.install(Box::new(|medium, name| {
        Some(format!("{medium:?}:{name}").into_bytes())
    }));
    for medium in NON_DIRECT {
        let cap = slot.try_mint(medium).expect("minted");
        assert_eq!(
            slot.resolve(cap, medium, "n"),
            Some(format!("{medium:?}:n").into_bytes())
        );
    }
}
