// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail doctests for what the spec macros refuse.
//!
//! ```
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n + 1 } else { n + 1 };
//!         }
//!         invariant InBox: n <= Max;
//!     }
//! };
//! assert_eq!(m.name, "Twin");
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n * 2 } else { n + 1 };
//!         }
//!         invariant InBox: n <= Max;
//!     }
//! };
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n + 1 } else { n + 1 };
//!         }
//!         invariant InBox: n < Max;
//!     }
//! };
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0;
//!         action Up when (n <= Max - 1 <= Max) {
//!             n = if n > 1 { n + 1 } else { n + 1 };
//!         }
//!         invariant InBox: n <= Max;
//!     }
//! };
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n + 1 };
//!         }
//!         invariant InBox: n <= Max;
//!     }
//! };
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n + 1; } else { n + 1 };
//!         }
//!         invariant InBox: n <= Max;
//!     }
//! };
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n + -1 } else { n + 1 };
//!         }
//!         invariant InBox: n <= Max;
//!     }
//! };
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n.max(1) } else { n + 1 };
//!         }
//!         invariant InBox: n <= Max;
//!     }
//! };
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n + 1 } else { n + 1 };
//!         }
//!         invariant InBox: n <= self::Max;
//!     }
//! };
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Buggy = 1;
//!         var n = 0;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n + 1 } else { n + 1 };
//!         }
//!         invariant InBox: n <= Max;
//!     }
//! };
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     Twin {
//!         const Buggy = 0;
//!         const Max = 3;
//!         scenario Max = 4;
//!         var n = 0.5;
//!         action Up when (n <= Max - 1) {
//!             n = if n > 1 { n + 1 } else { n + 1 };
//!         }
//!         invariant InBox: n <= Max;
//!     }
//! };
//! ```
//!
//! ```
//! #[aterm_spec_macros::refines(machine = "Twin", action = "Up", project = "twin::project")]
//! fn up(n: u32) -> u32 {
//!     n + 1
//! }
//! #[aterm_spec_macros::spec_unmodeled(reason = "a platform fast path", machine = "Twin")]
//! fn fast() {}
//! # fn main() { assert_eq!(up(1), 2); fast(); }
//! ```
//!
//! ```compile_fail
//! #[aterm_spec_macros::refines(machine = "Twin", action = "Up", project = "twin::project", tla = "x")]
//! fn up(n: u32) -> u32 {
//!     n + 1
//! }
//! #[aterm_spec_macros::spec_unmodeled(reason = "a platform fast path", machine = "Twin")]
//! fn fast() {}
//! # fn main() { assert_eq!(up(1), 2); fast(); }
//! ```
//!
//! ```compile_fail
//! #[aterm_spec_macros::refines(machine = "Twin", action = Up, project = "twin::project")]
//! fn up(n: u32) -> u32 {
//!     n + 1
//! }
//! #[aterm_spec_macros::spec_unmodeled(reason = "a platform fast path", machine = "Twin")]
//! fn fast() {}
//! # fn main() { assert_eq!(up(1), 2); fast(); }
//! ```
//!
//! ```compile_fail
//! #[aterm_spec_macros::refines(machine = "Twin", action = "Up", project = "twin::project")]
//! fn up(n: u32) -> u32 {
//!     n + 1
//! }
//! #[aterm_spec_macros::spec_unmodeled(machine = "Twin")]
//! fn fast() {}
//! # fn main() { assert_eq!(up(1), 2); fast(); }
//! ```
//!
//! ```
//! use aterm_spec::derive::{Liveness, Model, le, var};
//! #[aterm_spec::model]
//! fn twin() -> Model {
//!     aterm_spec::ty_model! { Twin { var n = 0; action Up when (n <= 0) { n = n + 1; } } }
//! }
//! #[aterm_spec::liveness(twin)]
//! fn settles() -> Liveness {
//!     Liveness { name: "Settles", goal: le(var("n"), var("n")), weak: vec![], strong: vec![], mutants: vec![] }
//! }
//! # fn main() { assert_eq!(twin().name, settles().name.replace("Settles", "Twin")); }
//! ```
//!
//! ```compile_fail
//! use aterm_spec::derive::{Liveness, Model, le, var};
//! #[aterm_spec::model(twin)]
//! fn twin() -> Model {
//!     aterm_spec::ty_model! { Twin { var n = 0; action Up when (n <= 0) { n = n + 1; } } }
//! }
//! #[aterm_spec::liveness(twin)]
//! fn settles() -> Liveness {
//!     Liveness { name: "Settles", goal: le(var("n"), var("n")), weak: vec![], strong: vec![], mutants: vec![] }
//! }
//! # fn main() { assert_eq!(twin().name, settles().name.replace("Settles", "Twin")); }
//! ```
//!
//! ```compile_fail
//! use aterm_spec::derive::{Liveness, Model, le, var};
//! #[aterm_spec::model]
//! fn twin() -> Model {
//!     aterm_spec::ty_model! { Twin { var n = 0; action Up when (n <= 0) { n = n + 1; } } }
//! }
//! #[aterm_spec::liveness(twin())]
//! fn settles() -> Liveness {
//!     Liveness { name: "Settles", goal: le(var("n"), var("n")), weak: vec![], strong: vec![], mutants: vec![] }
//! }
//! # fn main() { assert_eq!(twin().name, settles().name.replace("Settles", "Twin")); }
//! ```
//!
//! ```
//! let m = aterm_spec::ty_model! {
//!     UnicodeSuffixTwin {
//!         var n = 0u̕;
//!         invariant Small: n <= 1u̕;
//!     }
//! };
//! assert_eq!(m.vars[0].init, 0);
//! ```
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     UnicodeSuffixTwin {
//!         var n = 0u̕;
//!         invariant Small: n <= 1e3u̕;
//!     }
//! };
//! ```
