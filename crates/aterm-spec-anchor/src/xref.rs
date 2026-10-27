// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The anchor half of the source↔spec cross-reference.

use std::collections::BTreeSet;

/// A source→spec refinement binding, emitted by `#[refines(machine, action, …)]`.
#[derive(Debug, Clone, Copy)]
pub struct RefinementAnchor {
    /// The TLA+ machine name (e.g. `"terminal_modes"`).
    pub machine: &'static str,
    /// The TLA+ action name (e.g. `"SetCursorVisible"`) — matches an `Action`.
    pub action: &'static str,
    /// The annotated Rust fn (path/name), e.g. `"TerminalHandler::show_cursor"`.
    pub rust_method: &'static str,
    /// Source location `file:line` (`file!()`/`line!()`): the line of the fn's first anchor
    /// attribute, for every anchor stacked on it, under either form of the attributes.
    pub location: &'static str,
    /// The projection fn path (the `project=` arg, which `#[refines]` requires): what Tier-1 reads the real state through.
    pub project: &'static str,
    /// The EXECUTION-EVIDENCE key for this anchor.
    pub entry_id: &'static str,
}

/// An explicit "this fn is intentionally NOT modeled" waiver, emitted by `#[spec_unmodeled(reason, …)]`.
#[derive(Debug, Clone, Copy)]
pub struct Waiver {
    pub machine: &'static str,
    pub action: &'static str,
    pub reason: &'static str,
    pub rust_method: &'static str,
    pub location: &'static str,
}

crate::registry::collect!(RefinementAnchor);
crate::registry::collect!(Waiver);

/// All [`RefinementAnchor`]s linked into the current binary.
pub fn refinements() -> impl Iterator<Item = &'static RefinementAnchor> {
    crate::registry::iter::<RefinementAnchor>()
}

/// All [`Waiver`]s linked into the current binary.
pub fn waivers() -> impl Iterator<Item = &'static Waiver> {
    crate::registry::iter::<Waiver>()
}

thread_local! {
    /// Is a `aterm_spec::xref::StepEvidence::step` window open on this thread? The probe's first act
    /// is to read this and return, so an un-armed probe costs one TLS `bool` load.
    static ARMED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };

    /// Anchor ids whose annotated fn was entered on THIS thread inside the open
    /// window.
    static ENTERED_ANCHORS: core::cell::RefCell<BTreeSet<&'static str>> =
        const { core::cell::RefCell::new(BTreeSet::new()) };
}

/// Record that the fn carrying `entry_id` was ENTERED on this thread.
#[cfg_attr(trust_verify, trust::skip)]
#[inline]
pub fn note_entered(entry_id: &'static str) {
    if !ARMED.try_with(|armed| armed.get()).unwrap_or(false) {
        return;
    }
    let _ = ENTERED_ANCHORS.try_with(|cell| {
        if let Ok(mut set) = cell.try_borrow_mut() {
            set.insert(entry_id);
        }
    });
}

/// Open an evidence window: forget every anchor entry recorded on this thread and ARM the probe.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn reset_entered_anchors() -> bool {
    let cleared = ENTERED_ANCHORS
        .try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut set) => {
                set.clear();
                set.is_empty()
            }
            Err(_) => false,
        })
        .unwrap_or(false);
    let armed = ARMED
        .try_with(|armed| {
            armed.set(true);
            armed.get()
        })
        .unwrap_or(false);
    cleared && armed
}

/// Close the evidence window: the probe goes back to being one `bool` load.
#[cfg_attr(trust_verify, trust::skip)]
pub fn disarm_entered_anchors() {
    let _ = ARMED.try_with(|armed| armed.set(false));
}

/// The anchor ids entered on this thread since the last [`reset_entered_anchors`].
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn entered_anchor_ids() -> BTreeSet<&'static str> {
    ENTERED_ANCHORS
        .try_with(|cell| match cell.try_borrow() {
            Ok(set) => (*set).clone(),
            Err(_) => BTreeSet::new(),
        })
        .unwrap_or_default()
}

/// How many ids are recorded on this thread.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn entered_anchor_count() -> Option<usize> {
    ENTERED_ANCHORS
        .try_with(|cell| cell.try_borrow().ok().map(|set| set.len()))
        .ok()
        .flatten()
}

/// Is an evidence window open on this thread? `None` when the flag could not be read.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn window_is_armed() -> Option<bool> {
    ARMED.try_with(core::cell::Cell::get).ok()
}

/// Close this thread's window and hand over what it recorded, leaving the record empty.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn take_entered_anchors() -> Option<BTreeSet<&'static str>> {
    if window_is_armed() != Some(true) {
        return None;
    }
    disarm_entered_anchors();
    ENTERED_ANCHORS
        .try_with(|cell| {
            cell.try_borrow_mut()
                .ok()
                .map(|mut set| core::mem::take(&mut *set))
        })
        .ok()
        .flatten()
}

/// Credit this thread's OPEN window with `ids`.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn merge_entered_anchors(ids: BTreeSet<&'static str>) -> bool {
    window_is_armed() == Some(true)
        && ENTERED_ANCHORS
            .try_with(|cell| cell.try_borrow_mut().map(|mut set| set.extend(ids)).is_ok())
            .unwrap_or(false)
}
