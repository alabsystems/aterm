// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P2, gap heal (step PF2). Its island takes `crate::revocation`'s decisions as parameters with
//! exactness premises; it never redefines revocation.
//!
//! Reserved by PF1, which created this crate, so the step that proves it edits only this file
//! and its island.
