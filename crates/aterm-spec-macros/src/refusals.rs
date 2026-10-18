// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail controls for what the macros refuse
//! (docs/AUDIT-third-party-2026-10-15.md, SP.8).
//!
//! Each refused form is a `compile_fail` doctest after an accepted twin that
//! differs from it in one place, so a refusal cannot pass because its snippet
//! was broken some other way. Every form refused here was refused before the
//! macros left syn too.
//!
//! # `ty_model!`
//!
//! Accepted twin:
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
//! An operator outside `+ - > <= == && ||`:
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
//! `<`, which is outside the set too:
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
//! A chained comparison:
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
//! An `if` with no `else`:
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
//! An arm that is not one expression:
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
//! A prefix minus (the grammar has no negative literal in an expression):
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
//! A method call:
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
//! A path where a name goes:
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
//! A scenario that sets `Buggy`:
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
//! A float where an integer is declared:
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
//! # The anchor attributes
//!
//! Accepted twin:
//!
//! ```
//! #[aterm_spec_anchor::refines(machine = "Twin", action = "Up", project = "twin::project")]
//! fn up(n: u32) -> u32 {
//!     n + 1
//! }
//! #[aterm_spec_anchor::spec_unmodeled(reason = "a platform fast path", machine = "Twin")]
//! fn fast() {}
//! # fn main() { assert_eq!(up(1), 2); fast(); }
//! ```
//!
//! A key the attribute does not take:
//!
//! ```compile_fail
//! #[aterm_spec_anchor::refines(machine = "Twin", action = "Up", project = "twin::project", tla = "x")]
//! fn up(n: u32) -> u32 {
//!     n + 1
//! }
//! #[aterm_spec_anchor::spec_unmodeled(reason = "a platform fast path", machine = "Twin")]
//! fn fast() {}
//! # fn main() { assert_eq!(up(1), 2); fast(); }
//! ```
//!
//! A value that is not a string literal:
//!
//! ```compile_fail
//! #[aterm_spec_anchor::refines(machine = "Twin", action = Up, project = "twin::project")]
//! fn up(n: u32) -> u32 {
//!     n + 1
//! }
//! #[aterm_spec_anchor::spec_unmodeled(reason = "a platform fast path", machine = "Twin")]
//! fn fast() {}
//! # fn main() { assert_eq!(up(1), 2); fast(); }
//! ```
//!
//! A waiver with no `reason`:
//!
//! ```compile_fail
//! #[aterm_spec_anchor::refines(machine = "Twin", action = "Up", project = "twin::project")]
//! fn up(n: u32) -> u32 {
//!     n + 1
//! }
//! #[aterm_spec_anchor::spec_unmodeled(machine = "Twin")]
//! fn fast() {}
//! # fn main() { assert_eq!(up(1), 2); fast(); }
//! ```
//!
//! # The registering attributes
//!
//! Accepted twin:
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
//! `#[model]` with an argument:
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
//! `#[liveness]` naming something that is not a path:
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
//! A compiler-lexed Unicode suffix is accepted on integer digits:
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
//! The same suffix does not turn a floating exponent into an integer:
//!
//! ```compile_fail
//! let m = aterm_spec::ty_model! {
//!     UnicodeSuffixTwin {
//!         var n = 0u̕;
//!         invariant Small: n <= 1e3u̕;
//!     }
//! };
//! ```
