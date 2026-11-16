// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn a_mark_whose_parts_disagree_is_refused() {
    let one: &[u32] = &[0x11_2233];
    let ok = check(
        "kimi",
        None,
        [one, one],
        [one, one],
        b"Kk",
        &[Texel::Ink(0), Texel::Half(0)],
    );
    assert_eq!(ok, Ok(()));
    assert!(
        check(
            "kimi",
            None,
            [one, one],
            [one, one],
            b"K",
            &[Texel::Ink(0), Texel::Half(0)]
        )
        .is_err()
    );
    assert!(
        check(
            "kimi",
            None,
            [one, one],
            [one, one],
            b"Kk",
            &[Texel::Ink(0), Texel::Ink(1)]
        )
        .is_err()
    );
    assert!(
        check(
            "kimi",
            None,
            [one, one],
            [one, one],
            b"KK",
            &[Texel::Ink(0), Texel::Half(0)]
        )
        .is_err()
    );
    assert!(
        check(
            "kimi",
            None,
            [one, one],
            [one, one],
            b"K.",
            &[Texel::Ink(0), Texel::Half(0)]
        )
        .is_err()
    );
    assert!(
        check(
            "kimi",
            None,
            [one, one],
            [one, one],
            b"Kk",
            &[Texel::Ink(0), Texel::Hole(0x100_0000)]
        )
        .is_err()
    );
    let two: &[u32] = &[1, 2];
    assert!(
        check(
            "kimi",
            None,
            [two, one],
            [one, one],
            b"Kk",
            &[Texel::Ink(0), Texel::Half(0)]
        )
        .is_err()
    );
    assert!(
        check(
            "",
            None,
            [one, one],
            [one, one],
            b"Kk",
            &[Texel::Ink(0), Texel::Half(0)]
        )
        .is_err()
    );
    let three: &[u32] = &[1, 2, 3];
    assert!(
        check(
            "agy",
            None,
            [one, one],
            [three, three],
            b"ABC",
            &[Texel::Ink(0), Texel::Ink(2), Texel::Ink(1)]
        )
        .is_err()
    );
    assert_eq!(
        check(
            "agy",
            None,
            [one, one],
            [three, three],
            b"ABC",
            &[Texel::Ink(0), Texel::Ink(1), Texel::Ink(2)]
        ),
        Ok(())
    );
}

#[test]
fn a_drawing_is_rectangular_and_uses_only_its_characters() {
    assert_eq!(check_rows(&["K.", ".k"], b"Kk"), Ok(()));
    assert!(check_rows(&["K.", "."], b"Kk").is_err());
    assert!(check_rows(&["Kx"], b"Kk").is_err());
    assert!(check_rows::<&str>(&[], b"Kk").is_err());
    let wide = "K".repeat(MAX_MARK_SIDE + 1);
    assert!(check_rows(&[wide.as_str()], b"K").is_err());
}
