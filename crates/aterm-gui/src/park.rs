// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The park every blocking control verb waits in: [`aterm_control::park`],
//! below this crate so the fabric endpoint's waits (`aterm-gui-fabric`) slice
//! the same way the dispatcher's do.

pub(crate) use aterm_control::park::{Lane, Woke, park};
