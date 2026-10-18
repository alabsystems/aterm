// Copyright 2021 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_consumer 0.37.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/iterators.rs, and cut by the aterm project in 2026 to the
// filtered walks (the unfiltered sibling iterators became
// `Node::next_sibling` / `Node::previous_sibling`, and grafts are gone).
//
// Derived from Chromium's accessibility abstraction.
// Copyright 2018 The Chromium Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-CHROMIUM file.

//! Walks over a node's children and siblings as a filter sees them: a node
//! the filter excludes is looked through to its children, a subtree it
//! excludes is skipped.

use std::iter::FusedIterator;

use super::filters::FilterResult;
use super::node::Node;

fn next_filtered_sibling<'a>(
    node: Option<Node<'a>>,
    filter: &impl Fn(&Node) -> FilterResult,
) -> Option<Node<'a>> {
    let mut next = node;
    let mut consider_children = false;
    while let Some(current) = next {
        if let Some(Some(child)) = consider_children.then(|| current.children().next()) {
            let result = filter(&child);
            next = Some(child);
            if result == FilterResult::Include {
                return next;
            }
            consider_children = result == FilterResult::ExcludeNode;
        } else {
            match current.next_sibling() {
                Some(sibling) => {
                    let result = filter(&sibling);
                    next = Some(sibling);
                    if result == FilterResult::Include {
                        return next;
                    }
                    if result == FilterResult::ExcludeNode {
                        consider_children = true;
                    }
                }
                _ => {
                    let parent = current.parent()?;
                    next = Some(parent);
                    if filter(&parent) != FilterResult::ExcludeNode {
                        return None;
                    }
                    consider_children = false;
                }
            }
        }
    }
    None
}

fn previous_filtered_sibling<'a>(
    node: Option<Node<'a>>,
    filter: &impl Fn(&Node) -> FilterResult,
) -> Option<Node<'a>> {
    let mut previous = node;
    let mut consider_children = false;
    while let Some(current) = previous {
        if let Some(Some(child)) = consider_children.then(|| current.children().next_back()) {
            let result = filter(&child);
            previous = Some(child);
            if result == FilterResult::Include {
                return previous;
            }
            consider_children = result == FilterResult::ExcludeNode;
        } else {
            match current.previous_sibling() {
                Some(sibling) => {
                    let result = filter(&sibling);
                    previous = Some(sibling);
                    if result == FilterResult::Include {
                        return previous;
                    }
                    if result == FilterResult::ExcludeNode {
                        consider_children = true;
                    }
                }
                _ => {
                    let parent = current.parent()?;
                    previous = Some(parent);
                    if filter(&parent) != FilterResult::ExcludeNode {
                        return None;
                    }
                    consider_children = false;
                }
            }
        }
    }
    None
}

/// Whether a double-ended walk has met itself.
fn met(front: Option<&Node>, back: Option<&Node>) -> bool {
    front
        .zip(back)
        .map(|(f, b)| f.id() == b.id())
        .unwrap_or(true)
}

/// The siblings after a node, as `filter` sees them.
pub(crate) struct FollowingFilteredSiblings<'a, Filter: Fn(&Node) -> FilterResult> {
    filter: Filter,
    back: Option<Node<'a>>,
    done: bool,
    front: Option<Node<'a>>,
}

impl<'a, Filter: Fn(&Node) -> FilterResult> FollowingFilteredSiblings<'a, Filter> {
    pub(crate) fn new(node: Node<'a>, filter: Filter) -> Self {
        let front = next_filtered_sibling(Some(node), &filter);
        let back = node
            .filtered_parent(&filter)
            .and_then(|parent| parent.last_filtered_child(&filter));
        Self {
            filter,
            back,
            done: back.is_none() || front.is_none(),
            front,
        }
    }
}

impl<'a, Filter: Fn(&Node) -> FilterResult> Iterator for FollowingFilteredSiblings<'a, Filter> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else {
            self.done = met(self.front.as_ref(), self.back.as_ref());
            let current = self.front;
            self.front = next_filtered_sibling(self.front, &self.filter);
            current
        }
    }
}

impl<Filter: Fn(&Node) -> FilterResult> DoubleEndedIterator
    for FollowingFilteredSiblings<'_, Filter>
{
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else {
            self.done = met(self.front.as_ref(), self.back.as_ref());
            let current = self.back;
            self.back = previous_filtered_sibling(self.back, &self.filter);
            current
        }
    }
}

impl<Filter: Fn(&Node) -> FilterResult> FusedIterator for FollowingFilteredSiblings<'_, Filter> {}

/// The siblings before a node, nearest first, as `filter` sees them.
pub(crate) struct PrecedingFilteredSiblings<'a, Filter: Fn(&Node) -> FilterResult> {
    filter: Filter,
    back: Option<Node<'a>>,
    done: bool,
    front: Option<Node<'a>>,
}

impl<'a, Filter: Fn(&Node) -> FilterResult> PrecedingFilteredSiblings<'a, Filter> {
    pub(crate) fn new(node: Node<'a>, filter: Filter) -> Self {
        let front = previous_filtered_sibling(Some(node), &filter);
        let back = node
            .filtered_parent(&filter)
            .and_then(|parent| parent.first_filtered_child(&filter));
        Self {
            filter,
            back,
            done: back.is_none() || front.is_none(),
            front,
        }
    }
}

impl<'a, Filter: Fn(&Node) -> FilterResult> Iterator for PrecedingFilteredSiblings<'a, Filter> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else {
            self.done = met(self.front.as_ref(), self.back.as_ref());
            let current = self.front;
            self.front = previous_filtered_sibling(self.front, &self.filter);
            current
        }
    }
}

impl<Filter: Fn(&Node) -> FilterResult> DoubleEndedIterator
    for PrecedingFilteredSiblings<'_, Filter>
{
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else {
            self.done = met(self.front.as_ref(), self.back.as_ref());
            let current = self.back;
            self.back = next_filtered_sibling(self.back, &self.filter);
            current
        }
    }
}

impl<Filter: Fn(&Node) -> FilterResult> FusedIterator for PrecedingFilteredSiblings<'_, Filter> {}

/// A node's children, as `filter` sees them.
pub(crate) struct FilteredChildren<'a, Filter: Fn(&Node) -> FilterResult> {
    filter: Filter,
    back: Option<Node<'a>>,
    done: bool,
    front: Option<Node<'a>>,
}

impl<'a, Filter: Fn(&Node) -> FilterResult> FilteredChildren<'a, Filter> {
    pub(crate) fn new(node: Node<'a>, filter: Filter) -> Self {
        let front = node.first_filtered_child(&filter);
        let back = node.last_filtered_child(&filter);
        Self {
            filter,
            back,
            done: back.is_none() || front.is_none(),
            front,
        }
    }
}

impl<'a, Filter: Fn(&Node) -> FilterResult> Iterator for FilteredChildren<'a, Filter> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else {
            self.done = met(self.front.as_ref(), self.back.as_ref());
            let current = self.front;
            self.front = next_filtered_sibling(self.front, &self.filter);
            current
        }
    }
}

impl<Filter: Fn(&Node) -> FilterResult> DoubleEndedIterator for FilteredChildren<'_, Filter> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else {
            self.done = met(self.front.as_ref(), self.back.as_ref());
            let current = self.back;
            self.back = previous_filtered_sibling(self.back, &self.filter);
            current
        }
    }
}

impl<Filter: Fn(&Node) -> FilterResult> FusedIterator for FilteredChildren<'_, Filter> {}
