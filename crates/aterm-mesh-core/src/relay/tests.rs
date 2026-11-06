// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rust folds held to the island: the answer sheet over the island's grid, the replaced rules
//! the grid must see, and the island's concrete views read back in Rust.

use super::*;

/// The island file, read as text on every target (stock and cross builds included).
const ISLAND: &str = include_str!("../relay_island.rs");

/// The island's grid size (`sheetFrom 0 128`, eight views to a word).
const GRID: usize = 1024;

/// The island's `gridView k`: this Mac's key (0) and two others.
fn grid(k: usize) -> Vec<Key> {
    let flag = |b: usize| (k / b) % 2 == 1;
    vec![
        Key {
            class: 0,
            principal: 0,
            current: flag(1),
            gone: flag(2),
        },
        Key {
            class: (k / 16) % 2,
            principal: (k / 32) % 2,
            current: flag(4),
            gone: flag(8),
        },
        Key {
            class: (k / 256) % 2,
            principal: 1 + (k / 512) % 2,
            current: flag(64),
            gone: flag(128),
        },
    ]
}

/// What one answer sheet reads of a view: per key 1 and 2 whether it is an origin and revoked,
/// per principal 1 and 2 whether it is revoked, and per key whether it has a rival.
struct Reading {
    origin: fn(&[Key], &Verdict, usize) -> bool,
    revoked: fn(&[Key], &Verdict, usize) -> bool,
    principal_revoked: fn(&[Key], &Verdict, usize) -> bool,
}

const DECISION: Reading = Reading {
    origin: |_, v, k| v.is_origin(k),
    revoked: |_, v, k| v.is_revoked(k),
    principal_revoked: |_, v, p| v.is_principal_revoked(p),
};

/// The island's `gridByte` of view `k`, under `reading`.
fn grid_byte(k: usize, reading: &Reading) -> u8 {
    let keys = grid(k);
    let v = Verdict::of(&keys, 0, 3);
    let bits = [
        (reading.origin)(&keys, &v, 1),
        (reading.origin)(&keys, &v, 2),
        (reading.revoked)(&keys, &v, 1),
        (reading.revoked)(&keys, &v, 2),
        (reading.principal_revoked)(&keys, &v, 1),
        (reading.principal_revoked)(&keys, &v, 2),
        v.has_rival(1),
        v.has_rival(2),
    ];
    bits.iter()
        .enumerate()
        .fold(0, |byte, (n, &on)| byte | (u8::from(on) << n))
}

/// The island's `sheet`: eight views to a word, view `8w`'s byte first.
fn sheet(reading: &Reading) -> Vec<u64> {
    (0..GRID / 8)
        .map(|w| {
            (0..8).fold(0, |word, n| {
                (word << 8) | u64::from(grid_byte(8 * w + n, reading))
            })
        })
        .collect()
}

/// The words the island proves `sheet` equal to, by `rfl`.
fn island_sheet() -> Option<Vec<u64>> {
    const STATED: &str = "theorem sheet_is : sheet =";
    let rest = &ISLAND[ISLAND.find(STATED)? + STATED.len()..];
    let list = &rest[rest.find('[')? + 1..rest.find(']')?];
    list.split(',')
        .map(|word| u64::from_str_radix(word.trim().strip_prefix("0x")?, 16).ok())
        .collect()
}

#[test]
fn the_island_states_its_grid_size() {
    assert!(ISLAND.contains(&format!("def sheet : List Nat := sheetFrom 0 {}", GRID / 8)));
}

#[test]
fn answer_sheet_matches_the_island() {
    let computed = sheet(&DECISION);
    let words: Vec<String> = computed.iter().map(|w| format!("0x{w:016x}")).collect();
    assert!(
        island_sheet().as_ref() == Some(&computed),
        "the Rust folds answer otherwise than the island on the grid; the words they compute are\n{}",
        words.join(", ")
    );
}

/// Before PF5: an origin is any current key, its class unread.
fn origin_blind_to_class(keys: &[Key], _: &Verdict, k: usize) -> bool {
    k == 0 || keys[k].current
}

/// Before PF5: a key is revoked when gone, a contested one never.
fn revoked_when_gone(keys: &[Key], _: &Verdict, k: usize) -> bool {
    k != 0 && keys[k].gone
}

/// Before PF5: a principal is revoked with any revoked key of it, whoever holds it.
fn principal_revoked_held_or_not(keys: &[Key], v: &Verdict, p: usize) -> bool {
    (0..keys.len()).any(|k| keys[k].principal == p && v.is_revoked(k))
}

#[test]
fn the_grid_sees_every_replaced_rule() {
    let island = island_sheet().expect("the island states its answer sheet");
    for (name, mutant) in [
        (
            "an origin blind to its class",
            Reading {
                origin: origin_blind_to_class,
                ..DECISION
            },
        ),
        (
            "a contested key left unrevoked",
            Reading {
                revoked: revoked_when_gone,
                ..DECISION
            },
        ),
        (
            "a principal revoked though an origin holds it",
            Reading {
                principal_revoked: principal_revoked_held_or_not,
                ..DECISION
            },
        ),
    ] {
        assert_ne!(
            sheet(&mutant),
            island,
            "the grid cannot tell the rule of {name} apart"
        );
    }
}

const fn key(class: usize, principal: usize, current: bool, gone: bool) -> Key {
    Key {
        class,
        principal,
        current,
        gone,
    }
}

/// The island's `collide`: keys 1 and 2 current and of one class, key 3 removed under key 1's
/// principal. Neither colliding key is an origin; both are revoked, and so is their principal.
#[test]
fn the_islands_colliding_view_takes_neither() {
    let keys = [
        key(0, 0, false, false),
        key(5, 1, true, false),
        key(5, 2, true, false),
        key(5, 1, false, true),
    ];
    let v = Verdict::of(&keys, 0, 4);
    assert!(
        v.is_origin(0) && !v.is_revoked(0),
        "this Mac is its own origin"
    );
    for k in [1, 2] {
        assert!(!v.is_origin(k) && v.is_revoked(k), "key {k} is contested");
    }
    assert!(v.is_principal_revoked(1) && v.is_principal_revoked(2));
    assert!(!v.is_principal_revoked(0));
}

/// The island's `underOrigin`: key 1 alone in its class is an origin, and the removed key 3 that
/// shares its principal is revoked by its key alone.
#[test]
fn the_islands_origin_keeps_its_principal() {
    let keys = [
        key(0, 0, false, false),
        key(1, 1, true, false),
        key(2, 2, true, false),
        key(3, 1, false, true),
    ];
    let v = Verdict::of(&keys, 0, 4);
    assert!(v.is_origin(1) && !v.is_revoked(1));
    assert!(v.is_revoked(3), "the removed key is revoked by its key");
    assert!(
        !v.is_principal_revoked(1),
        "the principal an origin holds is not"
    );
}
