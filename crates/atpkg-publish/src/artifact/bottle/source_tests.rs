// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn gnu_builder_does_not_claim_an_older_runtime_than_its_headers() {
    assert!(admitted_glibc("glibc 2.39\n"));
    assert!(admitted_glibc("glibc 2.38"));
    assert!(!admitted_glibc("glibc 2.40"));
    assert!(!admitted_glibc("musl 1.2.5"));
    assert!(!admitted_glibc("glibc 2.39.injected"));
}
