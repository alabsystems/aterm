// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{labels_distinct, labels_wire_safe};

label_enum! {
    enum Probe {
        First => "first",
        Second => "second_2",
        Third => "third",
    }
}

/// The negative control for the assertions every `label_enum!` declaration
/// const-evaluates: the same functions, run on tables that must fail.
#[test]
fn label_checks_reject_duplicates_and_unsafe_labels() {
    assert!(labels_distinct(&["a", "b"]));
    assert!(labels_distinct(&["a", "ab"]), "a prefix is not a duplicate");
    assert!(!labels_distinct(&["a", "b", "a"]));
    assert!(!labels_distinct(&["ab", "b", "ab"]));
    assert!(labels_wire_safe(&["cursor_effect", "adopt0", "press"]));
    for unsafe_label in ["a b", "a:b", "a,b", "a/b", "a=b", "a\"b", "A", "a-b", ""] {
        assert!(
            !labels_wire_safe(&["ok", unsafe_label]),
            "{unsafe_label:?} must be refused"
        );
    }
}

#[test]
fn a_declared_table_is_indexed_densely_in_declaration_order() {
    assert_eq!(Probe::COUNT, 3);
    assert_eq!(Probe::ALL, [Probe::First, Probe::Second, Probe::Third]);
    for (i, probe) in Probe::ALL.into_iter().enumerate() {
        assert_eq!(probe.index(), i);
        assert_eq!(Probe::from_index(i), Some(probe));
    }
    assert_eq!(Probe::from_index(Probe::COUNT), None);
    assert_eq!(Probe::Second.as_str(), "second_2");
    assert_eq!(Probe::Second.ident(), "Second");
}
