// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A table's derived files on this Mac (docs/design-commons-at-scale/journal.md §2.6, where they
//! are called `state/`; the merge design's decision 14 names the module `derived`, since
//! `table/state.rs` is the window design's). Everything here is rebuilt from the table's primary
//! files, so nothing here is flushed on the commit path.

pub(crate) mod run;
