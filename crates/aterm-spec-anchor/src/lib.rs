// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
// The anchor attributes are `macro_rules!` attributes where Cargo resolved no proc macro
// under them (anchor_rules.rs).
#![cfg_attr(anchor_rules, feature(macro_attr))]

//! What an anchor expansion references, and the anchor attributes themselves.
//!
//! An anchored crate writes
//! `#[cfg_attr(any(test, debug_assertions), aterm_spec_anchor::refines(…))]` (or
//! `spec_unmodeled`). The attribute submits a record into the
//! link-time [`registry`] and, for `refines`, puts a [`xref::note_entered`] probe at
//! fn entry. A release build strips the attribute, so it carries neither.
//!
//! The attributes come in two implementations with one expansion, chosen by
//! `cfg(anchor_rules)` (anchor_rules.rs), the fact Cargo resolves this crate's
//! dependencies by:
//!
//! * where a Trust compiler's cfg query answers `trust_verify` (its build, test, check,
//!   doc and verified lanes, and the lint lane where `targo-tippy`'s query answers it
//!   too, as trust-41d79979's does): `macro_rules!` attributes defined here, so no
//!   proc macro is in any anchored library's graph and Trust's PM-1 boundary verifies
//!   it. Every anchored crate turns on `macro_attr` under the cfg its anchors compile
//!   in, `#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]`,
//!   narrowed further where every anchor sits in platform code (aterm-update): a feature
//!   declared in a build that expands no anchor is the compiler's `unused_features`, and
//!   one missing where an anchor expands is a hard error, so the compiler holds the
//!   line to the anchors both ways;
//! * on a stock compiler (the cross cells, the public snapshot) and under a lint driver
//!   whose cfg query has no `trust_verify`: `aterm-spec-macros`' proc macros,
//!   re-exported. `macro_attr` is not stable.
//!
//! Both emit the same records: tests/attrs.rs runs the proc macros' own suite against
//! the attributes exported here and pins the same digest. Two differences remain, both
//! in the declarative attributes. A record's `location` in a stack of anchors on one fn
//! is the line of the stack's first attribute (the one-step expansion re-emits the rest).
//! And the rebuilt fn is spanned by the expansion, so rustc's item-level lints
//! (`dead_code`, `missing_docs`) do not fire on it in those lanes; a lint gate on the
//! proc macros still sees them. So an `expect(dead_code)` on an anchored fn holds under
//! one lint driver and is unfulfilled under the other: compile the fn only in the builds
//! that call it instead.
//!
//! This crate is std-only, so depending on it costs an anchored library one small
//! compile and never the models: those live in `aterm-spec` (the engine) and the
//! `aterm-models-*` crates, which only tests depend on. `aterm-spec` re-exports
//! [`registry`] and [`xref`], so there is one registry per binary however a record
//! was submitted.

// Stock compilers and the lint driver: the proc macros.
#[cfg(not(anchor_rules))]
pub use aterm_spec_macros::{refines, spec_unmodeled};

// Everywhere else on Trust: the declarative attributes, exported at the crate root.
#[cfg(anchor_rules)]
mod attrs;

// The link-time collection: the `::aterm_spec_anchor::registry::submit!` the anchor
// attributes emit, and the `::aterm_spec::registry::submit!` (a re-export of this one)
// that `ty_model!` emits, resolve to the same lists.
pub mod registry;
pub mod xref;
