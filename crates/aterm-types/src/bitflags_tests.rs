// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    struct TestFlags: u8 {
        const A = 1 << 0;
        const B = 1 << 1;
        const C = 1 << 2;
        const AB = Self::A.bits() | Self::B.bits();
    }
}

#[test]
fn operations_produce_expected_bits() {
    use TestFlags as F;
    let assigned = |mut f: F, op: fn(&mut F)| {
        op(&mut f);
        f
    };
    for (what, flags, bits) in [
        ("empty", F::empty(), 0),
        ("A", F::A, 1),
        ("B", F::B, 2),
        ("C", F::C, 4),
        ("AB", F::AB, 3),
        ("union", F::A.union(F::C), 5),
        ("intersection", (F::A | F::B).intersection(F::AB), 3),
        ("difference", F::AB.difference(F::A), 2),
        ("not A", !F::A, 0b0000_0110),
        ("not empty", !F::empty(), 0b0000_0111),
        ("from_bits_truncate", F::from_bits_truncate(0xFF), 0x07),
        ("all", F::all(), 0x07),
        ("from_bits_retain", F::from_bits_retain(0xF1), 0xF1),
        ("|=", assigned(F::A, |f| *f |= F::B), 3),
        ("&=", assigned(F::AB, |f| *f &= F::A), 1),
        ("^", F::AB ^ F::A, 2),
        ("-", F::AB - F::A, 2),
        ("-=", assigned(F::AB, |f| *f -= F::A), 2),
    ] {
        assert_eq!(flags.bits(), bits, "{what}");
    }
    for (what, holds) in [
        ("empty is empty", F::empty().is_empty()),
        ("not empty is all", (!F::empty()).is_all()),
        ("A|B|C is all", (F::A | F::B | F::C).is_all()),
        ("all is all", F::all().is_all()),
        (
            "a retained unknown bit is not all",
            !F::from_bits_retain(0x08).is_all(),
        ),
        ("from_bits keeps defined bits", F::from_bits(0x07).is_some()),
        (
            "from_bits rejects unknown bits",
            F::from_bits(0xFF).is_none(),
        ),
    ] {
        assert!(holds, "{what}");
    }
}

#[test]
fn predicates_and_mutators() {
    use TestFlags as F;
    let f = F::A | F::B;
    assert!(f.contains(F::A) && f.contains(F::B) && f.contains(F::AB));
    assert!(!f.contains(F::C));
    let f = F::A | F::C;
    assert!(f.intersects(F::A) && f.intersects(F::AB));
    assert!(!f.intersects(F::B));
    let mut f = F::empty();
    f.insert(F::A);
    assert_eq!(f, F::A);
    f.remove(F::A);
    assert!(f.is_empty());
    f.toggle(F::B);
    assert_eq!(f, F::B);
    f.toggle(F::B);
    assert!(f.is_empty());
    f.set(F::C, true);
    assert_eq!(f, F::C);
    f.set(F::C, false);
    assert!(f.is_empty());
}
