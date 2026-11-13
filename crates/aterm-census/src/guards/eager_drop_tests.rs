// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const OWNER: &str = "struct Cf(Ref);\n\
                     impl Cf {\n    \
                         fn adopt(p: Ref) -> Option<Self> {\n        \
                             (!p.is_null()).then_some(Self(p))\n    \
                         }\n\
                     }\n\
                     impl Drop for Cf {\n    \
                         fn drop(&mut self) { release(self.0) }\n\
                     }\n\
                     struct Plain { a: u8 }\n\
                     impl Plain {\n    \
                         fn new(a: u8) -> Option<Self> { (a > 0).then_some(Self { a }) }\n\
                     }\n";

#[test]
fn a_drop_type_built_eagerly_is_found_and_a_lazy_or_plain_one_is_not() {
    let lazy = OWNER.replace("then_some(Self(p))", "then(|| Self(p))");
    let named = "fn f(p: Ref, ok: bool) -> Option<Cf> {\n    \
                     // ok.then_some(Cf(p))\n    \
                     let s = \"ok.then_some(Cf(p))\";\n    \
                     let _ = ok.then_some(Plain { a: 1 });\n    \
                     ok.then_some(\n        Cf(p),\n    )\n\
                 }\n";
    let found = run(
        &EAGER_DROP,
        &[
            ("crates/x/src/eager.rs", OWNER),
            ("crates/x/src/lazy.rs", &lazy),
            ("crates/x/src/named.rs", &format!("{OWNER}{named}")),
            ("crates/x/src/no_drop.rs", named),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/x/src/eager.rs:4",
            "crates/x/src/named.rs:4",
            "crates/x/src/named.rs:18",
        ]
    );
}

#[test]
fn self_is_the_nearest_impl_and_a_trait_is_no_type() {
    assert_eq!(
        item_header("impl<'a, T: Copy> Drop for Guard<'a, T> {"),
        Some(Some("Guard"))
    );
    assert_eq!(
        item_header("    pub unsafe impl Send for crate::x::Accel {}"),
        Some(Some("Accel"))
    );
    assert_eq!(item_header("impl Cf {"), Some(Some("Cf")));
    assert_eq!(item_header("pub trait Adopt {"), Some(None));
    assert_eq!(item_header("let imply = 1;"), None);
    assert_eq!(
        drop_impl("    impl Drop for OwnedIcon {"),
        Some("OwnedIcon")
    );
    assert_eq!(
        drop_impl("impl std::ops::Drop for Release<'_> {"),
        Some("Release")
    );
    assert_eq!(drop_impl("impl NoDrop for Plain {"), None);
    assert_eq!(constructed("Self { kq: fd })"), Some("Self"));
    assert_eq!(constructed("crate::Group { pid })"), Some("Group"));
    assert_eq!(constructed("(a, b))"), None);
    assert_eq!(constructed("value)"), None);
}
