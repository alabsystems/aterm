// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_boolean_is_satisfied_by_the_integer_macos_reads_for_it() {
    assert!(PValue::Int(1).same(&PValue::Bool(true)));
    assert!(PValue::Int(0).same(&PValue::Bool(false)));
    assert!(!PValue::Int(2).same(&PValue::Bool(true)));
    assert!(!PValue::Int(1).same(&PValue::Bool(false)));
}

#[test]
fn dictionaries_compare_as_key_sets() {
    let a = PValue::Dict(vec![
        ("A".into(), PValue::Int(1)),
        ("B".into(), PValue::Int(2)),
    ]);
    let b = PValue::Dict(vec![
        ("B".into(), PValue::Int(2)),
        ("A".into(), PValue::Int(1)),
    ]);
    assert!(a.same(&b));
    let c = PValue::Dict(vec![("A".into(), PValue::Int(1))]);
    assert!(!a.same(&c));
    assert!(!c.same(&a));
}
