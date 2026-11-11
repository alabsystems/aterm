// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn view_store_with(count: usize) -> (ViewStore, Vec<ViewId>) {
    let mut store = ViewStore::default();
    let ids = (0..count)
        .map(|session| {
            store
                .insert_terminal(session as u64)
                .expect("finite test id space")
        })
        .collect();
    (store, ids)
}

fn parked_set(shown: usize, parked: usize) -> (TabSet, ViewStore, Vec<TabId>) {
    let (store, views) = view_store_with(shown + parked);
    let mut ids = IdAllocator::<TabId>::default();
    let mut set = TabSet::default();
    let mut tabs = Vec::new();
    for (index, view) in views.into_iter().enumerate() {
        let id = ids.allocate().expect("finite test id space");
        let mut tab = Tab::new(id, view, TabPresentation::terminal(format!("t{index}")));
        tab.parked = index >= shown;
        tab.ephemeral = tab.parked;
        assert_eq!(set.push(tab).expect("fresh id"), index);
        tabs.push(id);
    }
    (set, store, tabs)
}

#[test]
fn a_shown_tab_lands_before_the_parked_suffix_and_a_parked_one_after_it() {
    let (mut set, store, tabs) = parked_set(2, 2);
    assert_eq!(set.shown_len(), 2);
    assert_eq!(set.shown().len() + set.parked().len(), 4);

    let view = set.tabs()[0].focus;
    let mut ids = IdAllocator::<TabId>::default();
    for _ in 0..tabs.len() {
        ids.allocate().expect("finite test id space");
    }
    let shown = Tab::new(
        ids.allocate().expect("finite test id space"),
        view,
        TabPresentation::terminal("new"),
    );
    let shown_id = shown.id;
    assert_eq!(set.push(shown).expect("fresh id"), 2, "at the boundary");
    assert_eq!(set.active_id(), Some(shown_id));
    assert_eq!(set.shown_len(), 3);
    assert!(set.invariant_holds(&store));

    let mut parked = Tab::new(
        ids.allocate().expect("finite test id space"),
        view,
        TabPresentation::terminal("worker"),
    );
    parked.parked = true;
    let parked_id = parked.id;
    assert_eq!(set.push(parked).expect("fresh id"), 5, "last of all");
    assert_eq!(set.tabs()[5].id, parked_id);
    assert_eq!(set.shown_len(), 3);
    assert!(set.invariant_holds(&store));
}

#[test]
fn a_broken_suffix_fails_the_invariant() {
    let (mut set, store, _) = parked_set(2, 1);
    assert!(set.invariant_holds(&store));
    // The negative control: a shown tab behind a parked one is the state nothing may build.
    set.tab_at_mut(0).expect("a tab").parked = true;
    assert!(!set.parked_is_suffix());
    assert!(!set.invariant_holds(&store));
}

#[test]
fn home_must_stand_among_the_shown_tabs() {
    let (mut set, store, tabs) = parked_set(2, 1);
    assert!(!set.set_home(Some(tabs[2])), "a parked tab is no home");
    assert_eq!(set.home(), None);
    assert!(set.set_home(Some(tabs[1])));
    assert!(set.invariant_holds(&store));
    // A home that became parked behind the model's back fails the invariant.
    set.tab_at_mut(1).expect("a tab").parked = true;
    assert!(!set.invariant_holds(&store));
}

#[test]
fn the_strip_selects_home_while_a_parked_tab_is_active() {
    let (mut set, _store, tabs) = parked_set(3, 2);
    assert!(set.set_home(Some(tabs[1])));
    assert!(set.switch_to(tabs[0]));
    assert_eq!(set.strip_active(), Some(0), "a shown tab is its own");
    assert!(!set.active_is_parked());
    assert!(set.switch_to(tabs[4]));
    assert!(set.active_is_parked());
    assert_eq!(set.active_index(), Some(4), "canonical stays canonical");
    assert_eq!(set.strip_active(), Some(1), "the chip stands for it");
    assert!(set.set_home(None));
    assert_eq!(set.strip_active(), Some(2), "no home: the last shown");
}

#[test]
fn next_and_previous_walk_only_the_shown_tabs() {
    let (mut set, store, tabs) = parked_set(3, 2);
    assert!(set.set_home(Some(tabs[2])));
    assert!(set.switch_to(tabs[0]));
    assert_eq!(
        set.cycle_shown(false),
        Some(tabs[2]),
        "wraps past the suffix"
    );
    assert_eq!(
        set.cycle_shown(true),
        Some(tabs[0]),
        "wraps past the suffix"
    );
    assert_eq!(set.cycle_shown(true), Some(tabs[1]));
    assert!(set.switch_to(tabs[3]));
    assert_eq!(set.cycle_shown(true), Some(tabs[0]), "steps from home");
    assert!(set.switch_to(tabs[4]));
    assert_eq!(set.cycle_shown(false), Some(tabs[1]), "steps from home");
    assert!(set.invariant_holds(&store));

    let (mut one, _, only) = parked_set(1, 1);
    assert!(one.set_home(Some(only[0])));
    assert_eq!(
        one.cycle_shown(true),
        Some(only[0]),
        "a parked tab goes home"
    );
    assert_eq!(one.cycle_shown(true), Some(only[0]), "and home stays");
}

#[test]
fn closing_never_hands_the_front_to_a_parked_tab_while_a_shown_one_stands() {
    // The last shown tab closes: what follows it is parked, so the one before it takes over.
    let (mut set, store, tabs) = parked_set(2, 2);
    assert!(set.switch_to(tabs[1]));
    assert!(set.remove(tabs[1]).is_some());
    assert_eq!(set.active_id(), Some(tabs[0]));
    assert!(set.invariant_holds(&store));

    // A parked active tab closes: it lands on home, not its next sibling.
    let (mut set, store, tabs) = parked_set(3, 3);
    assert!(set.set_home(Some(tabs[1])));
    assert!(set.switch_to(tabs[3]));
    assert!(set.remove(tabs[3]).is_some());
    assert_eq!(set.active_id(), Some(tabs[1]));
    assert!(set.invariant_holds(&store));

    // A parked tab closes behind the front: the front stays.
    assert!(set.switch_to(tabs[0]));
    assert!(set.remove(tabs[5]).is_some());
    assert_eq!(set.active_id(), Some(tabs[0]));

    // With no home the last shown tab takes it.
    let (mut set, _store, tabs) = parked_set(2, 2);
    assert!(set.switch_to(tabs[2]));
    assert!(set.remove(tabs[2]).is_some());
    assert_eq!(set.active_id(), Some(tabs[1]));

    // Removing the home clears it, so no stale identity outlives the tab.
    let (mut set, store, tabs) = parked_set(2, 1);
    assert!(set.set_home(Some(tabs[1])));
    assert!(set.remove(tabs[1]).is_some());
    assert_eq!(set.home(), None);
    assert!(set.invariant_holds(&store));

    // Only parked tabs remain: one of them still takes the front, never nothing.
    let (mut set, store, tabs) = parked_set(1, 2);
    assert!(set.remove(tabs[0]).is_some());
    assert!(set.active_is_parked());
    assert!(set.invariant_holds(&store));
}

#[test]
fn a_move_never_crosses_the_boundary() {
    let (mut set, store, tabs) = parked_set(3, 2);
    assert!(!set.reorder(tabs[0], 3), "a shown tab stays shown");
    assert!(!set.reorder(tabs[4], 2), "a parked tab stays parked");
    assert_eq!(set.tabs()[0].id, tabs[0]);
    assert!(set.reorder(tabs[0], 2), "within the shown tabs");
    assert!(set.reorder(tabs[4], 3), "within the parked tabs");
    assert!(set.invariant_holds(&store));
    assert_eq!(set.shown_len(), 3);
}

#[test]
fn placing_a_tab_stops_at_the_boundary() {
    let (mut set, store, tabs) = parked_set(3, 2);
    assert_eq!(set.place(tabs[0], 4), Some(2), "a shown tab stays shown");
    assert_eq!(set.tabs()[2].id, tabs[0]);
    assert_eq!(set.place(tabs[4], 0), Some(3), "a parked tab stays parked");
    assert_eq!(set.tabs()[3].id, tabs[4]);
    assert!(set.invariant_holds(&store));
}

#[test]
fn keeping_makes_the_worker_the_last_shown_tab_a_persons_and_keeps_the_front() {
    let (mut set, store, tabs) = parked_set(2, 3);
    assert!(set.switch_to(tabs[0]));
    assert_eq!(set.keep(tabs[3]), Some(2));
    assert_eq!(set.tabs()[2].id, tabs[3]);
    assert!(!set.tabs()[2].parked);
    assert!(!set.tabs()[2].ephemeral, "it is a person's now");
    assert_eq!(set.shown_len(), 3);
    assert_eq!(set.active_id(), Some(tabs[0]));
    assert!(set.invariant_holds(&store));
    assert_eq!(set.keep(tabs[0]), None, "a person's tab is kept already");
    // A worker's tab off the parked tabs keeps its mark until it is kept, where it stands.
    set.tab_at_mut(1).expect("a tab").ephemeral = true;
    assert_eq!(set.keep(tabs[1]), Some(1));
    assert!(!set.tabs()[1].ephemeral);
    assert_eq!(set.tabs()[1].id, tabs[1]);
    assert!(set.invariant_holds(&store));
}

#[test]
fn a_parked_tabs_chip_is_home_and_a_shown_tabs_is_its_own() {
    let (mut set, _store, tabs) = parked_set(2, 1);
    assert_eq!(set.chip_of(tabs[1]), Some(tabs[1]));
    assert_eq!(
        set.chip_of(tabs[2]),
        None,
        "no home yet: no chip stands for it"
    );
    assert!(set.set_home(Some(tabs[0])));
    assert_eq!(set.chip_of(tabs[2]), Some(tabs[0]));
}

#[test]
fn split_tree_layout_and_neighbor_share_logical_rects() {
    let mut tree = SplitTree::leaf(1u64);
    assert!(tree.split_leaf(1, SplitAxis::Horizontal, 2));
    assert!(tree.split_leaf(2, SplitAxis::Vertical, 3));
    let bounds = LogicalRect::new(0.0, 0.0, 801.0, 601.0);
    let leaves = tree.layout(bounds, 1.0, 1.0);
    assert_eq!(leaves.len(), 3);
    assert_eq!(
        tree.neighbor(3, FocusDirection::Up, bounds, 1.0, 1.0),
        Some(2)
    );
    assert_eq!(
        tree.neighbor(3, FocusDirection::Left, bounds, 1.0, 1.0),
        Some(1)
    );
}

#[test]
fn divider_hit_drives_the_same_split_and_stale_path_fails_closed() {
    let mut tree = SplitTree::leaf(1u64);
    assert!(tree.split_leaf(1, SplitAxis::Horizontal, 2));
    let bounds = LogicalRect::new(0.0, 0.0, 80.0, 24.0);
    let hit = tree
        .divider_at(LogicalPoint { x: 40.0, y: 5.0 }, bounds, 1.0, 1.0)
        .expect("root divider");
    let ratio = SplitTree::<u64>::ratio_for_pointer(&hit, LogicalPoint { x: 20.0, y: 5.0 }, 1.0)
        .expect("ratio");
    assert!(tree.set_divider_ratio(&hit, ratio));
    let layout = tree.layout(bounds, 1.0, 1.0);
    assert_eq!(layout[0].rect.size.width, 20.0);

    assert_eq!(tree.remove_leaf(2), RemoveLeaf::Removed);
    assert!(!tree.set_divider_ratio(&hit, 0.8));
}

#[test]
fn collapsed_leaf_placement_round_trips_exact_nested_topology() {
    let mut tree = SplitTree::leaf(1u64);
    assert!(tree.split_leaf(1, SplitAxis::Horizontal, 2));
    assert!(tree.split_leaf(2, SplitAxis::Vertical, 3));
    let expected = tree.clone();
    let (path, branch, axis, ratio) = tree.leaf_placement(3).expect("placement");
    assert_eq!(path.branches(), &[SplitBranch::Second]);
    assert_eq!(branch, SplitBranch::Second);
    assert_eq!(tree.remove_leaf(3), RemoveLeaf::Removed);
    assert!(tree.restore_collapsed_leaf(&path, branch, axis, ratio, 3));
    assert_eq!(tree, expected);
    assert!(!tree.restore_collapsed_leaf(&path, branch, axis, ratio, 3));
}

#[test]
fn visible_plan_keeps_heterogeneous_leaves_paths_and_hits_in_lockstep() {
    let mut store = ViewStore::default();
    let terminal = store.insert_terminal(11).expect("terminal view");
    let native = store
        .insert_native(AppInstanceId::from_stored(7))
        .expect("native view");
    let second_terminal = store.insert_terminal(12).expect("terminal view");
    let mut tab = Tab::new(
        TabId::from_stored(3),
        terminal,
        TabPresentation::terminal("shell"),
    );
    assert!(tab.split_focused(SplitAxis::Horizontal, native));
    assert!(tab.split_focused(SplitAxis::Vertical, second_terminal));
    let plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 121.0, 61.0), 1.0, |view| {
        if view == native {
            LeafSizing::new(LogicalSize::new(30.0, 12.0), LogicalSize::new(72.0, 36.0))
        } else {
            LeafSizing::new(LogicalSize::new(2.0, 1.0), LogicalSize::new(80.0, 24.0))
        }
    });
    assert_eq!(plan.leaves.len(), 3);
    assert_eq!(plan.dividers.len(), 2);
    assert_eq!(plan.leaves[0].path.branches(), &[SplitBranch::First]);
    assert_eq!(
        plan.leaves[1].path.branches(),
        &[SplitBranch::Second, SplitBranch::First]
    );
    assert_eq!(
        plan.leaves[2].path.branches(),
        &[SplitBranch::Second, SplitBranch::Second]
    );
    for leaf in &plan.leaves {
        let point = LogicalPoint {
            x: leaf.rect.origin.x + leaf.rect.size.width * 0.5,
            y: leaf.rect.origin.y + leaf.rect.size.height * 0.5,
        };
        assert_eq!(plan.leaf_at(point).map(|hit| hit.view), Some(leaf.view));
    }
    assert!(plan.leaf(second_terminal).is_some_and(|leaf| leaf.focused));
    assert!(tab.invariant_holds(&store));
}

#[test]
fn pane_headers_reserve_content_and_hit_their_own_leaf_in_nested_splits() {
    let (_, views) = view_store_with(3);
    let mut tab = Tab::new(
        TabId::from_stored(1),
        views[0],
        TabPresentation::terminal("shell"),
    );
    assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
    assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
    let mut plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 80.0, 24.0), 1.0, |_| {
        LeafSizing::new(LogicalSize::new(2.0, 1.0), LogicalSize::new(80.0, 24.0))
    });
    let before = plan.clone();
    plan.reserve_pane_headers();
    assert_eq!(
        plan.dividers, before.dividers,
        "headers never move split handles"
    );
    for (leaf, original) in plan.leaves.iter().zip(&before.leaves) {
        let header = leaf.header.expect("roomy split has a title row");
        assert_eq!(leaf.outer_rect(), original.rect);
        assert_eq!(header.size.height, 1.0);
        assert_eq!(leaf.rect.origin.y, original.rect.origin.y + 1.0);
        assert_eq!(leaf.rect.size.height, original.rect.size.height - 1.0);
        let header_point = LogicalPoint {
            x: header.origin.x,
            y: header.origin.y,
        };
        assert_eq!(
            plan.header_at(header_point).map(|hit| hit.view),
            Some(leaf.view)
        );
        assert_eq!(
            plan.leaf_at(header_point).map(|hit| hit.view),
            Some(leaf.view)
        );
        assert!(
            !leaf.rect.contains(header_point),
            "chrome is outside terminal input"
        );
        assert!(
            plan.header_at(leaf.rect.origin).is_none(),
            "first content row is not chrome"
        );
    }
    for divider in &plan.dividers {
        assert!(plan.header_at(divider.rect.origin).is_none());
        assert!(plan.leaf_at(divider.rect.origin).is_none());
    }
    for leaf in &plan.leaves {
        for direction in [
            FocusDirection::Left,
            FocusDirection::Right,
            FocusDirection::Up,
            FocusDirection::Down,
        ] {
            let mut from_leaf = tab.clone();
            from_leaf.focus = leaf.view;
            assert_eq!(
                from_leaf.neighbor(direction, &plan),
                from_leaf.neighbor(direction, &before),
                "title rows must not change keyboard focus navigation"
            );
        }
    }
    let once = plan.clone();
    plan.reserve_pane_headers();
    assert_eq!(
        plan, once,
        "reserving twice never steals a second content row"
    );
}

#[test]
fn pane_headers_disappear_for_single_zoomed_and_one_row_leaves() {
    let (_, views) = view_store_with(2);
    let mut tab = Tab::new(
        TabId::from_stored(1),
        views[0],
        TabPresentation::terminal("shell"),
    );
    let plan_for = |tab: &Tab, rows| {
        let mut plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 80.0, rows), 1.0, |_| {
            LeafSizing::new(LogicalSize::new(2.0, 1.0), LogicalSize::new(80.0, 24.0))
        });
        plan.reserve_pane_headers();
        plan
    };
    let single = plan_for(&tab, 24.0);
    assert!(single.leaves[0].header.is_none());
    assert_eq!(single.leaves[0].rect, single.bounds);
    assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
    let tiny = plan_for(&tab, 1.0);
    assert!(tiny.leaves.iter().all(|leaf| leaf.header.is_none()));
    assert!(tiny.leaves.iter().all(|leaf| leaf.rect.size.height == 1.0));
    let just_fits = plan_for(&tab, 2.0);
    assert!(just_fits.leaves.iter().all(|leaf| leaf.header.is_some()));
    assert!(
        just_fits
            .leaves
            .iter()
            .all(|leaf| leaf.rect.size.height == 1.0)
    );
    tab.zoomed = true;
    let zoomed = plan_for(&tab, 24.0);
    assert_eq!(zoomed.leaves.len(), 1);
    assert!(zoomed.leaves[0].header.is_none());
    assert_eq!(zoomed.leaves[0].rect, zoomed.bounds);
    tab.zoomed = false;
    assert!(
        plan_for(&tab, 24.0)
            .leaves
            .iter()
            .all(|leaf| leaf.header.is_some())
    );
}

#[test]
fn impossible_minimum_fit_preserves_every_leaf_and_stale_divider_fails_closed() {
    let (_, views) = view_store_with(3);
    let mut tab = Tab::new(
        TabId::from_stored(1),
        views[0],
        TabPresentation::terminal("one"),
    );
    assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
    assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
    let tiny = tab.visible_plan(LogicalRect::new(0.0, 0.0, 3.0, 2.0), 1.0, |_| {
        LeafSizing::new(LogicalSize::new(20.0, 10.0), LogicalSize::new(80.0, 24.0))
    });
    assert_eq!(tiny.leaves.len(), 3, "shortage never drops a view");
    let nested = tiny
        .dividers
        .iter()
        .find(|divider| !divider.path.branches().is_empty())
        .expect("nested divider")
        .path
        .clone();
    assert_eq!(tab.remove_view(views[2]), RemoveLeaf::Removed);
    let before = tab.root.clone();
    assert!(!tab.set_divider_ratio(&nested, 0.8));
    assert_eq!(tab.root, before, "stale topology handles fail closed");
    assert_eq!(
        tab.focus, views[1],
        "close repairs focus deterministically onto the leaf that inherits the \
         removed leaf's space, not the tree's first leaf"
    );
}

/// Shrinking clamps to bounds without dropping leaves, and regrowing restores the geometry.
#[test]
fn shrinking_a_canonical_plan_stays_inside_the_bounds_and_drops_no_leaf() {
    let (_, views) = view_store_with(4);
    let mut tab = Tab::new(
        TabId::from_stored(1),
        views[0],
        TabPresentation::terminal("one"),
    );
    assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
    assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
    assert!(tab.split_focused(SplitAxis::Horizontal, views[3]));
    let sizing = |_| LeafSizing::new(LogicalSize::new(16.0, 3.0), LogicalSize::new(80.0, 24.0));
    let roomy = tab.visible_plan(LogicalRect::new(0.0, 0.0, 200.0, 48.0), 1.0, sizing);
    assert_eq!(roomy.leaves.len(), 4);

    for w in 0u16..=8 {
        for h in 0u16..=8 {
            let bounds = LogicalRect::new(0.0, 0.0, f32::from(w), f32::from(h));
            let plan = tab.visible_plan(bounds, 1.0, sizing);
            assert_eq!(plan.leaves.len(), 4, "shrinking never drops a leaf");
            for leaf in &plan.leaves {
                let r = leaf.rect;
                assert!(
                    r.size.width >= 0.0 && r.size.height >= 0.0,
                    "no negative extent at {w}x{h}: {r:?}"
                );
                assert!(
                    r.origin.x >= plan.bounds.origin.x - f32::EPSILON
                        && r.origin.y >= plan.bounds.origin.y - f32::EPSILON,
                    "origin inside the {w}x{h} bounds: {r:?}"
                );
                assert!(
                    r.origin.x + r.size.width
                        <= plan.bounds.origin.x + plan.bounds.size.width + f32::EPSILON
                        && r.origin.y + r.size.height
                            <= plan.bounds.origin.y + plan.bounds.size.height + f32::EPSILON,
                    "extent inside the {w}x{h} bounds: {r:?}"
                );
            }
        }
    }

    let regrown = tab.visible_plan(LogicalRect::new(0.0, 0.0, 200.0, 48.0), 1.0, sizing);
    assert_eq!(
        regrown.leaves, roomy.leaves,
        "shrink -> grow restores the exact geometry"
    );
}

#[test]
fn zoom_focus_and_presentation_are_content_agnostic() {
    let (_, views) = view_store_with(2);
    let mut tab = Tab::new(
        TabId::from_stored(1),
        views[0],
        TabPresentation::terminal("left"),
    );
    assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
    let plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 80.0, 24.0), 1.0, |_| {
        LeafSizing::new(LogicalSize::new(1.0, 1.0), LogicalSize::new(10.0, 10.0))
    });
    assert_eq!(tab.neighbor(FocusDirection::Left, &plan), Some(views[0]));
    assert!(tab.set_focus(views[0]));
    assert_eq!(tab.focus, views[0]);
    assert!(tab.toggle_zoom());
    let zoom = tab.visible_plan(LogicalRect::new(0.0, 0.0, 80.0, 24.0), 1.0, |_| {
        LeafSizing::new(LogicalSize::new(1.0, 1.0), LogicalSize::new(10.0, 10.0))
    });
    assert_eq!(zoom.leaves.len(), 1);
    assert_eq!(zoom.leaves[0].view, views[0]);
    assert_eq!(zoom.leaves[0].rect, zoom.bounds);

    let aggregate = aggregate_presentations(
        views[0],
        [
            (
                views[0],
                TabPresentation {
                    title: "Focused".to_string(),
                    icon: None,
                    indicators: TabIndicators {
                        dirty: false,
                        busy: true,
                        attention: false,
                        status_attention: false,
                    },
                    conn: Some(TabConnRole::Inbound),
                    closable: true,
                    tooltip: Some("primary".to_string()),
                },
            ),
            (
                views[1],
                TabPresentation {
                    title: "Sibling".to_string(),
                    icon: Some(TabIconKind::Editor),
                    indicators: TabIndicators {
                        dirty: true,
                        busy: false,
                        attention: true,
                        status_attention: true,
                    },
                    conn: Some(TabConnRole::Outbound),
                    closable: false,
                    tooltip: None,
                },
            ),
        ],
    )
    .expect("aggregate");
    assert_eq!(aggregate.title, "Focused");
    assert_eq!(
        aggregate.indicators,
        TabIndicators {
            dirty: true,
            busy: true,
            attention: true,
            status_attention: true,
        }
    );
    assert_eq!(aggregate.conn, Some(TabConnRole::Both));
    assert!(!aggregate.closable);
}

/// `remove_view` re-seats focus onto the leaf that inherits the removed leaf's space.
#[test]
fn remove_view_repairs_focus_onto_the_leaf_that_inherits_the_space() {
    let (_, views) = view_store_with(4);
    let mut tab = Tab::new(
        TabId::from_stored(1),
        views[0],
        TabPresentation::terminal("one"),
    );
    assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
    assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
    assert_eq!(tab.focus, views[2]);
    assert_eq!(tab.remove_view(views[2]), RemoveLeaf::Removed);
    assert_eq!(
        tab.focus, views[1],
        "focus follows the space to view 1, not back to the first leaf 0"
    );

    let (_, views) = view_store_with(3);
    let mut tab = Tab::new(
        TabId::from_stored(2),
        views[0],
        TabPresentation::terminal("one"),
    );
    assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
    assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
    assert!(tab.set_focus(views[0]));
    assert_eq!(tab.remove_view(views[2]), RemoveLeaf::Removed);
    assert_eq!(
        tab.focus, views[0],
        "a background leaf closing never steals the keyboard"
    );
}

#[test]
fn heterogeneous_tab_mutators_refine_pane_tree_model_with_negative_control() {
    use aterm_models_gui::pane_tree_model;
    use aterm_spec::interp::{State, admits};

    fn project(tab: &Tab) -> State {
        let leaves = tab.root.leaves();
        let focused = leaves
            .iter()
            .position(|view| *view == tab.focus)
            .expect("focused leaf is live");
        State::from([
            ("leaf_count", leaves.len() as i64),
            ("focused", focused as i64),
        ])
    }

    let (_, views) = view_store_with(3);
    let model = pane_tree_model();
    let mut tab = Tab::new(
        TabId::from_stored(1),
        views[0],
        TabPresentation::terminal("one"),
    );
    let initial = project(&tab);
    assert_eq!(initial, model.init_state());

    assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
    let split_once = project(&tab);
    assert_eq!(admits(&model, &initial, &split_once), Some("Split"));
    assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
    let split_twice = project(&tab);
    assert_eq!(admits(&model, &split_once, &split_twice), Some("Split"));
    assert_eq!(tab.remove_view(views[2]), RemoveLeaf::Removed);
    let closed = project(&tab);
    assert!(
        model.successors("Close", &split_twice).contains(&closed),
        "real generic close is one admitted nondeterministic focus repair"
    );

    let mut dangling = closed.clone();
    dangling.insert("focused", dangling["leaf_count"]);
    assert_eq!(admits(&model, &split_twice, &dangling), None);
    assert!(!model.check_invariant("FocusInRange", &dangling));
}

/// `any_leaf` agrees with `leaves().contains()` and short-circuits on the first match.
#[test]
fn any_leaf_agrees_with_leaves_any_and_stops_at_the_first_match() {
    let (_, views) = view_store_with(3);
    let mut tab = Tab::new(
        TabId::from_stored(1),
        views[0],
        TabPresentation::terminal("one"),
    );
    assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
    assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
    let leaves = tab.root.leaves();
    assert_eq!(leaves.len(), 3, "a nested three-pane tree, not a lone leaf");

    for probe in views.iter().copied().chain([ViewId::from_stored(9999)]) {
        assert_eq!(
            tab.root.any_leaf(&mut |view| *view == probe),
            leaves.contains(&probe),
            "any_leaf disagrees with leaves().contains() on {probe}"
        );
    }

    let mut seen = Vec::new();
    assert!(tab.root.any_leaf(&mut |view| {
        seen.push(*view);
        true
    }));
    assert_eq!(
        seen,
        leaves[..1],
        "stopped at the first match, in visit order"
    );
    assert!(!tab.set_divider_ratio(&SplitPath::root(), f32::NAN));
    let plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 200.0, 100.0), f32::NAN, |_| {
        LeafSizing::new(LogicalSize::default(), LogicalSize::default())
    });
    assert!(plan.divider.is_finite());
    assert!(plan.leaves.iter().all(|l| l.rect.size.width.is_finite()
        && l.rect.size.height.is_finite()
        && l.rect.origin.x.is_finite()
        && l.rect.origin.y.is_finite()));
    assert_eq!(
        plan.ratio_for_pointer(
            &plan.dividers[0],
            LogicalPoint {
                x: f32::NAN,
                y: 10.0,
            },
        ),
        None
    );
}
