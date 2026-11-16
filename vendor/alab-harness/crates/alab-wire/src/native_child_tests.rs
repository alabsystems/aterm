// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::NativeChild;

#[test]
fn native_child_validates_pid_and_birth_bounds_without_observing_a_process() {
    for pid in [0, 1, i32::MAX as u32 + 1, u32::MAX] {
        assert_eq!(NativeChild::from_native(pid, 1), None, "pid {pid}");
    }
    for pid in [2, 4242, i32::MAX as u32] {
        assert_eq!(NativeChild::from_native(pid, 0), None);
        for birth in [1, 12_345, u64::MAX] {
            let child = NativeChild::from_native(pid, birth).expect("valid native scalar");
            assert_eq!(child.pid(), pid);
            assert_eq!(child.birth(), birth);
        }
    }
}

#[test]
fn native_child_equality_keeps_pid_and_exact_birth_independent() {
    let original = NativeChild::from_native(4242, 12_345).unwrap();
    assert_eq!(original, NativeChild::from_native(4242, 12_345).unwrap());
    assert_ne!(original, NativeChild::from_native(4242, 12_346).unwrap());
    assert_ne!(original, NativeChild::from_native(4243, 12_345).unwrap());
}
