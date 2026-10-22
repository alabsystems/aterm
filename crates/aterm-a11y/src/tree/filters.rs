// Copyright 2023 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_consumer 0.37.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/filters.rs, and cut by the aterm project in 2026 to the
// nodes aterm's model has: nothing is hidden, grafted or clipped, so only a
// text run is looked through.

//! Which nodes a platform tree shows.

use crate::model::Role;

use super::node::Node;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FilterResult {
    Include,
    ExcludeNode,
    ExcludeSubtree,
}

/// The platform tree's filter: every node but a text run, which is part of
/// its document's text (unless it holds the focus).
#[inline]
pub(crate) fn common_filter(node: &Node) -> FilterResult {
    if node.is_focused() {
        return FilterResult::Include;
    }
    if node.role() == Role::TextRun {
        return FilterResult::ExcludeNode;
    }
    FilterResult::Include
}
