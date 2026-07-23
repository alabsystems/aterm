// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for stable tab and view identity.
//!
//! This test drives the genuine [`TabSet`], [`ViewStore`], and monotonic
//! [`IdAllocator`] implementation.  Its projection is deliberately independent
//! of vector indices: tab order is projected as chrome order, while live views
//! are ordered by their stable wire identity.  Every real operation must be an
//! admitted transition of the drift-free `NativeTabIdentity` model.

#![cfg(test)]

use aterm_spec::derive::{Model, native_tab_identity_model};
use aterm_spec::interp::{State, admits};

use crate::tab_model::{
    IdAllocator, SplitTree, Tab, TabId, TabPresentation, TabSet, ViewId, ViewStore,
};

#[derive(Clone, Copy, Default)]
struct IdentityHistory {
    next_tab_id: u64,
    next_view_id: u64,
    retired_tab_id: u64,
    retired_view_id: u64,
    close_count: u64,
    removed_count: u64,
}

impl IdentityHistory {
    fn initialized() -> Self {
        Self {
            next_tab_id: 2,
            next_view_id: 2,
            ..Self::default()
        }
    }
}

struct RealTabs {
    tab_ids: IdAllocator<TabId>,
    views: ViewStore,
    tabs: TabSet,
    history: IdentityHistory,
}

impl RealTabs {
    fn new() -> Self {
        let mut tab_ids = IdAllocator::<TabId>::default();
        let mut views = ViewStore::default();
        let view = views.insert_terminal(1).expect("bounded ViewId space");
        let tab = tab_ids.allocate().expect("bounded TabId space");
        Self {
            tab_ids,
            views,
            tabs: TabSet::new(Tab::new(tab, view, TabPresentation::terminal("one"))),
            history: IdentityHistory::initialized(),
        }
    }

    fn open(&mut self) {
        let ordinal = self.tabs.len() + 1;
        let view = self
            .views
            .insert_terminal(ordinal as u64)
            .expect("bounded ViewId space");
        let tab = self.tab_ids.allocate().expect("bounded TabId space");
        self.tabs
            .push(Tab::new(
                tab,
                view,
                TabPresentation::terminal(format!("tab {ordinal}")),
            ))
            .expect("monotonic TabId is unique");
        self.history.next_tab_id = tab.get() + 1;
        self.history.next_view_id = view.get() + 1;
    }

    fn close_at(&mut self, index: usize) {
        let tab = self.tabs.tab_at(index).expect("live tab at index").clone();
        assert_eq!(tab.root, SplitTree::leaf(tab.focus));
        let removed = self.tabs.remove(tab.id).expect("live tab removal");
        assert_eq!(removed, tab);
        assert!(self.views.remove(tab.focus).is_some());
        self.history.retired_tab_id = tab.id.get();
        self.history.retired_view_id = tab.focus.get();
        self.history.close_count += 1;
        self.history.removed_count += 1;
    }

    fn project(&self, model: &Model) -> State {
        assert!(self.tabs.invariant_holds(&self.views));
        let mut state = model.init_state();
        let tabs = self.tabs.tabs();
        state.insert(
            "tab_count",
            i64::try_from(tabs.len()).expect("bounded tabs"),
        );
        for (index, suffix) in ["one", "two", "three"].into_iter().enumerate() {
            state.insert(
                match suffix {
                    "one" => "tab_one",
                    "two" => "tab_two",
                    _ => "tab_three",
                },
                tabs.get(index)
                    .map_or(0, |tab| i64::try_from(tab.id.get()).expect("bounded TabId")),
            );
            state.insert(
                match suffix {
                    "one" => "tab_view_one",
                    "two" => "tab_view_two",
                    _ => "tab_view_three",
                },
                tabs.get(index).map_or(0, |tab| {
                    i64::try_from(tab.focus.get()).expect("bounded ViewId")
                }),
            );
        }

        // HashMap iteration order is intentionally irrelevant. Stable identity
        // order gives the model its compact live-view slots after a removal.
        let mut live: Vec<ViewId> = self.views.iter().map(|(id, _)| id).collect();
        live.sort_unstable();
        for (index, key) in ["live_view_one", "live_view_two", "live_view_three"]
            .into_iter()
            .enumerate()
        {
            state.insert(
                key,
                live.get(index)
                    .map_or(0, |id| i64::try_from(id.get()).expect("bounded ViewId")),
            );
        }

        let active = self.tabs.active().expect("non-empty bounded model");
        state.insert(
            "active_tab",
            i64::try_from(active.id.get()).expect("bounded TabId"),
        );
        state.insert(
            "focused_view",
            i64::try_from(active.focus.get()).expect("bounded ViewId"),
        );
        state.insert(
            "next_tab_id",
            i64::try_from(self.history.next_tab_id).expect("bounded next TabId"),
        );
        state.insert(
            "next_view_id",
            i64::try_from(self.history.next_view_id).expect("bounded next ViewId"),
        );
        state.insert(
            "retired_tab_id",
            i64::try_from(self.history.retired_tab_id).expect("bounded retired TabId"),
        );
        state.insert(
            "retired_view_id",
            i64::try_from(self.history.retired_view_id).expect("bounded retired ViewId"),
        );
        state.insert(
            "close_count",
            i64::try_from(self.history.close_count).expect("bounded close count"),
        );
        state.insert(
            "removed_count",
            i64::try_from(self.history.removed_count).expect("bounded remove count"),
        );
        state.insert("reused_tab_id", 0);
        state.insert("reused_view_id", 0);
        state
    }
}

fn assert_transition(model: &Model, before: &State, after: &State, action: &'static str) {
    assert_eq!(
        model.successors(action, before).as_slice(),
        std::slice::from_ref(after),
        "real transition must conform specifically to {action}"
    );
    assert_eq!(admits(model, before, after), Some(action));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "post-state violates {}::{}: {after:?}",
            model.name,
            invariant.name,
        );
    }
}

fn drive(
    model: &Model,
    real: &mut RealTabs,
    action: &'static str,
    operation: impl FnOnce(&mut RealTabs),
) {
    let before = real.project(model);
    operation(real);
    let after = real.project(model);
    assert_transition(model, &before, &after, action);
}

#[test]
fn real_tab_set_conforms_across_open_select_reorder_and_close() {
    let model = native_tab_identity_model();
    let mut real = RealTabs::new();
    assert_eq!(real.project(&model), model.init_state());

    drive(&model, &mut real, "OpenTab", RealTabs::open);
    drive(&model, &mut real, "OpenTab", RealTabs::open);
    drive(&model, &mut real, "SelectFirst", |real| {
        assert!(real.tabs.switch_to_index(0));
    });
    drive(&model, &mut real, "SelectSecond", |real| {
        assert!(real.tabs.switch_to_index(1));
    });
    drive(&model, &mut real, "SelectThird", |real| {
        assert!(real.tabs.switch_to_index(2));
    });
    drive(&model, &mut real, "ReorderFirstSecond", |real| {
        let first = real.tabs.tab_at(0).expect("first tab").id;
        assert!(real.tabs.reorder(first, 1));
    });
    drive(&model, &mut real, "ReorderSecondThird", |real| {
        let second = real.tabs.tab_at(1).expect("second tab").id;
        assert!(real.tabs.reorder(second, 2));
    });

    // Close the selected first tab. The shipping TabSet moves selection and
    // focus to the live tab now occupying that chrome position.
    drive(&model, &mut real, "SelectFirst", |real| {
        assert!(real.tabs.switch_to_index(0));
    });
    let before_close = real.project(&model);
    real.close_at(0);
    let after_close = real.project(&model);
    assert_transition(&model, &before_close, &after_close, "CloseFirst");

    // Negative control: a close router that leaves active/focus addressed to
    // retired identities is rejected and violates the named live-reference
    // invariant. This projection does not read the corrected TabSet selection.
    let mut dangling = after_close.clone();
    dangling.insert("active_tab", after_close["retired_tab_id"]);
    dangling.insert("focused_view", after_close["retired_view_id"]);
    assert_eq!(admits(&model, &before_close, &dangling), None);
    assert!(!model.check_invariant("ActiveReferencesLiveTab", &dangling));

    // A subsequent allocation comes from both genuine monotonic allocators,
    // never either retired identity.
    let before_reopen = real.project(&model);
    real.open();
    let after_reopen = real.project(&model);
    assert_transition(&model, &before_reopen, &after_reopen, "OpenTab");
    assert_ne!(after_reopen["active_tab"], after_reopen["retired_tab_id"]);
    assert_ne!(
        after_reopen["focused_view"],
        after_reopen["retired_view_id"]
    );

    // Negative control: a replacement that aliases both retired wire ids is
    // not admitted at Buggy=0, and the explicit non-reuse witnesses fail.
    let mut reused = after_reopen.clone();
    reused.insert("tab_three", before_reopen["retired_tab_id"]);
    reused.insert("tab_view_three", before_reopen["retired_view_id"]);
    reused.insert("live_view_three", before_reopen["retired_view_id"]);
    reused.insert("active_tab", before_reopen["retired_tab_id"]);
    reused.insert("focused_view", before_reopen["retired_view_id"]);
    reused.insert("reused_tab_id", 1);
    reused.insert("reused_view_id", 1);
    assert_eq!(admits(&model, &before_reopen, &reused), None);
    assert!(!model.check_invariant("TabIdsNeverReused", &reused));
    assert!(!model.check_invariant("ViewIdsNeverReused", &reused));

    // Exercise the remaining real close shapes from a full three-tab state.
    drive(&model, &mut real, "SelectSecond", |real| {
        assert!(real.tabs.switch_to_index(1));
    });
    drive(&model, &mut real, "CloseSecond", |real| real.close_at(1));
    drive(&model, &mut real, "OpenTab", RealTabs::open);
    drive(&model, &mut real, "SelectThird", |real| {
        assert!(real.tabs.switch_to_index(2));
    });
    drive(&model, &mut real, "CloseThird", |real| real.close_at(2));

    let final_state = real.project(&model);
    assert_eq!(final_state["close_count"], final_state["removed_count"]);
    assert_eq!(real.tabs.len(), real.views.iter().count());
}
