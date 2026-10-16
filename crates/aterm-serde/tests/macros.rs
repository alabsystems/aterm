// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde 1.0.228 test_suite/tests/macros/mod.rs (git tag v1.0.228)
// and modified by the aterm project in 2026.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

macro_rules! btreemap {
    () => {
        BTreeMap::new()
    };
    ($($key:expr => $value:expr),+) => {{
        let mut map = BTreeMap::new();
        $(map.insert($key, $value);)+
        map
    }};
}
