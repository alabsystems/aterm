// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of Aos hosted execution (`docs/DESIGN-aos-hosted-execution-2026-10-21.md`).
//!
//! Each `*_model()` is a bounded state machine written once with `aterm_spec::ty_model!`;
//! writing it registers it. The engine (the macro, the interpreter, `verify`, `xref`) is
//! `aterm-spec`; `aterm-models` links every domain crate for `spec-check` and the registry-wide
//! sweeps. The Tier-0 checks of these models are this crate's `tests/suite.rs`; the Tier-1
//! conformance test that binds them to the real code lives with that code (`aterm-aos`'s
//! `tests/conformance_publication.rs`). Two crates depend on this one: `aterm-models`, which
//! links every domain crate, and `aterm-aos`, as a dev-dependency for that test. So editing a
//! model here rebuilds this crate, `aterm-models` and `aterm-aos`'s tests, and no library the
//! product links.
//!
//! * [`aos_boundary_publication_model`]: same-schema replacement of a hosted task's code at a step
//!   boundary (`aterm_aos::publish::Boundary`, design §3.9): pause, prepare against the paused
//!   head, the bounded queue of inputs that arrive meanwhile, and the one-transaction commit or
//!   the reject that resumes the old pair.

mod models_publication;

pub use models_publication::aos_boundary_publication_model;
