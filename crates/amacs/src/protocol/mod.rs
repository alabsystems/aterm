// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! amacs's machine-facing grammar: what crosses a process boundary
//! (docs/DESIGN-amacs-2026-10-19.md). Pure text in and out; the control
//! socket and the files belong to the faces.
//!
//! The `edit` request with its ticket replies (`edit`) and the file-URI codec
//! (`uri`) are implemented, and aterm-gui serves them. The edit script
//! (`script`), its `line:h6` anchors (`anchor`) and the anchored reads
//! (`read`) are implemented as pure code. aterm-gui serves the reads through
//! the `doc` verb, whose requests and `doc ls` rows are `doc`; `doc apply`
//! and `amacs --read`/`--apply` are planned work.

pub mod anchor;
pub mod doc;
pub mod edit;
pub mod read;
pub mod script;
pub mod uri;
