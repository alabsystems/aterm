// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! amacs's machine-facing grammar: what crosses a process boundary
//! (docs/DESIGN-amacs-2026-10-19.md).

pub mod anchor;
pub mod doc;
// edit and uri live in amacs-protocol, so a client (aterm-ctl) links no editor engine.
pub use amacs_protocol::edit;
pub mod read;
pub mod script;
pub use amacs_protocol::uri;
