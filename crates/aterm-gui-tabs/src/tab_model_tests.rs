// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn topology_tokens_survive_presentation_updates_and_detect_every_mutable_owner() {
    let (mut tabs, mut views, ids) = parked_set(2, 1);
    let token = tabs.revision();
    tabs.presentation_at_mut(0).unwrap().title = "new label".into();
    tabs.switch_to(ids[1]);
    assert!(token.is_current(&tabs));
    let equal = tabs.clone();
    assert_eq!(tabs, equal, "cache identity is not content equality");
    assert!(
        !token.is_current(&equal),
        "a whole replacement has its own owner"
    );
    let token = tabs.revision();
    let _ = tabs.active_mut();
    assert!(!token.is_current(&tabs));
    let token = tabs.revision();
    let _ = tabs.tab_at_mut(0);
    assert!(!token.is_current(&tabs));
    let token = tabs.revision();
    tabs.reorder(ids[0], 1);
    assert!(!token.is_current(&tabs));
    let token = tabs.revision();
    tabs.keep(ids[2]);
    assert!(!token.is_current(&tabs));
    let token = tabs.revision();
    tabs.remove(ids[1]);
    assert!(!token.is_current(&tabs));
    let token = tabs.revision();
    tabs.push(equal.get(ids[1]).unwrap().clone()).unwrap();
    assert!(!token.is_current(&tabs));

    let token = views.revision();
    let id = views.insert_terminal(99).unwrap();
    assert!(!token.is_current(&views));
    let token = views.revision();
    views.remove(id);
    assert!(!token.is_current(&views));
    let token = views.revision();
    views = ViewStore::default();
    assert!(!token.is_current(&views));
}

#[test]
fn revision_overflow_changes_owner_instead_of_revalidating_an_old_token() {
    let (mut tabs, _, _) = parked_set(1, 0);
    tabs.revision.0.store(usize::MAX, Ordering::Relaxed);
    let token = tabs.revision();
    let _ = tabs.active_mut();
    assert!(!token.is_current(&tabs));
    let mut views = ViewStore::default();
    views.revision.0.store(usize::MAX, Ordering::Relaxed);
    let token = views.revision();
    views.insert_terminal(1).unwrap();
    assert!(!token.is_current(&views));
}

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

fn focused_membership_agrees(set: &TabSet, views: &[ViewId]) -> bool {
    views.iter().all(|view| {
        set.contains_focused_view(*view) == set.tabs().iter().any(|tab| tab.focus == *view)
    })
}

#[test]
fn focused_membership_survives_tab_mutations_and_replacement() {
    fn shared<T: Send + Sync>() {}
    shared::<TabSet>();
    shared::<ViewStore>();
    let (mut set, mut store, tabs) = parked_set(2, 2);
    let mut views: Vec<_> = store.iter().map(|(id, _)| id).collect();
    views.sort_unstable();
    let original = set.tab_at(0).unwrap().focus;
    let alias = store.insert_terminal(0).unwrap();
    views.push(alias);
    let cold = set.clone();
    assert!(focused_membership_agrees(&set, &views));
    assert_eq!(set, cold, "warming membership does not change identity");
    assert_eq!(set.take_focused_view_visits_for_test(), 4);

    set.presentation_at_mut(0).unwrap().title = "renamed".into();
    assert!(set.reorder(tabs[0], 1));
    assert!(set.place(tabs[3], 0).is_some());
    assert!(set.keep(tabs[2]).is_some());
    assert!(set.switch_to(tabs[0]));
    set.cycle(true);
    set.cycle_shown(false);
    assert!(focused_membership_agrees(&set, &views));
    assert_eq!(set.take_focused_view_visits_for_test(), 0);

    assert!(set.switch_to(tabs[0]));
    assert!(
        set.active_mut()
            .unwrap()
            .split_focused(SplitAxis::Horizontal, alias)
    );
    assert!(focused_membership_agrees(&set, &views));
    assert!(!set.contains_focused_view(original));
    assert!(set.contains_focused_view(alias));

    let copy = set.active().unwrap().clone();
    let duplicate = Tab::new(
        TabId::from_stored(100),
        alias,
        TabPresentation::terminal("alias"),
    );
    set.push(duplicate.clone()).unwrap();
    assert!(focused_membership_agrees(&set, &views));
    set.remove(copy.id).unwrap();
    assert!(focused_membership_agrees(&set, &views));
    assert!(
        set.contains_focused_view(alias),
        "the duplicate still labels this view"
    );

    let first = set.tab_at(0).unwrap().clone();
    let last_index = set.shown_len() - 1;
    let last = set.tab_at(last_index).unwrap().clone();
    *set.tab_at_mut(0).unwrap() = last;
    *set.tab_at_mut(last_index).unwrap() = first;
    assert!(focused_membership_agrees(&set, &views));
    set.tab_at_mut(last_index).unwrap().id = TabId::from_stored(101);
    assert!(focused_membership_agrees(&set, &views));
    assert!(set.invariant_holds(&store));

    set = TabSet::new(copy);
    assert!(focused_membership_agrees(&set, &views));
    set = TabSet::default();
    assert!(focused_membership_agrees(&set, &views));

    let mut set = TabSet::new(duplicate);
    assert!(focused_membership_agrees(&set, &views));
    let stale = set.focused_views.get().unwrap().clone();
    *set.tab_at_mut(0).unwrap() = Tab::new(
        TabId::from_stored(102),
        original,
        TabPresentation::terminal("replacement"),
    );
    // Negative control: restore the cache the broad mutable loan correctly invalidated.
    set.focused_views.set(stale).unwrap();
    assert!(!focused_membership_agrees(&set, &views));
    set.focused_views.take();
    assert!(focused_membership_agrees(&set, &views));
}

#[test]
fn terminal_view_index_matches_the_forward_store() {
    fn check(store: &ViewStore) {
        for session in [0, 1, 2, 3, 99] {
            let mut expected: Vec<_> = store
                .iter()
                .filter_map(|(id, view)| (view.terminal_session() == Some(session)).then_some(id))
                .collect();
            let mut indexed: Vec<_> = store.terminal_views(session).collect();
            expected.sort_unstable();
            indexed.sort_unstable();
            assert_eq!(indexed, expected);
        }
    }
    let (mut store, ids) = view_store_with(4);
    check(&store);
    let alias = store.insert_terminal(1).unwrap();
    let native = store.insert_native(AppInstanceId::from_stored(1)).unwrap();
    check(&store);
    store.remove(ids[1]).unwrap();
    check(&store);
    assert_eq!(store.terminal_views(1).collect::<Vec<_>>(), [alias]);
    store.remove(alias).unwrap();
    check(&store);
    assert!(store.remove(alias).is_none());
    store.remove(native).unwrap();
    check(&store);
    store = ViewStore::default();
    store.insert_terminal(99).unwrap();
    check(&store);
}

/// Check the selected position against an independent identity search after edits that move
/// the selected tab or its neighbors, including moves across the parked boundary by `keep`.
#[test]
fn active_position_tracks_identity_through_every_tab_edit() {
    fn check(set: &mut TabSet, views: &ViewStore, expected: Option<TabId>) {
        let searched = set.tabs().iter().position(|tab| Some(tab.id) == expected);
        assert_eq!(set.active_id(), expected);
        assert_eq!(set.active_index(), searched);
        assert_eq!(set.active().map(|tab| tab.id), expected);
        assert_eq!(set.active_mut().map(|tab| tab.id), expected);
        assert!(set.invariant_holds(views));
    }

    let (views, view_ids) = view_store_with(6);
    let mut ids = IdAllocator::<TabId>::default();
    let mut pushed = TabSet::default();
    check(&mut pushed, &views, None);
    for (index, view) in view_ids.into_iter().enumerate() {
        let id = ids.allocate().expect("finite test id space");
        let mut tab = Tab::new(id, view, TabPresentation::terminal("tab"));
        tab.parked = index % 2 == 0;
        pushed.push(tab.clone()).expect("new identity");
        check(&mut pushed, &views, Some(id));
        assert_eq!(pushed.push(tab), Err(DuplicateTabId));
        check(&mut pushed, &views, Some(id));
    }
    let stale = ids.allocate().expect("absent identity");
    let expected = pushed.active_id();
    assert!(!pushed.switch_to(stale));
    assert!(!pushed.switch_to_index(pushed.len()));
    assert!(pushed.remove(stale).is_none());
    assert!(!pushed.reorder(stale, 0));
    assert!(pushed.place(stale, 0).is_none());
    assert!(pushed.keep(stale).is_none());
    check(&mut pushed, &views, expected);

    let (base, views, tabs) = parked_set(3, 3);
    for home in [None, Some(tabs[1])] {
        for &selected in &tabs {
            let mut base = base.clone();
            assert!(base.set_home(home));
            assert!(base.switch_to(selected));
            check(&mut base, &views, Some(selected));
            for (from, &moved) in tabs.iter().enumerate() {
                for to in 0..=tabs.len() {
                    let mut reordered = base.clone();
                    let _ = reordered.reorder(moved, to);
                    check(&mut reordered, &views, Some(selected));
                    let mut placed = base.clone();
                    let _ = placed.place(moved, to);
                    check(&mut placed, &views, Some(selected));
                }

                let mut kept = base.clone();
                kept.tab_at_mut(from).expect("present tab").ephemeral = true;
                assert!(kept.keep(moved).is_some());
                check(&mut kept, &views, Some(selected));

                let mut removed = base.clone();
                let removed_tab = removed.remove(moved).expect("present tab");
                let expected = if moved != selected {
                    selected
                } else {
                    let shown: Vec<_> = removed.tabs().iter().filter(|tab| !tab.parked).collect();
                    if removed_tab.parked {
                        home.unwrap_or_else(|| shown.last().expect("shown tab").id)
                    } else {
                        shown[from.min(shown.len() - 1)].id
                    }
                };
                check(&mut removed, &views, Some(expected));
            }

            for forward in [false, true] {
                let mut cycled = base.clone();
                let mut expected = selected;
                for _ in 0..=tabs.len() {
                    let order: Vec<_> = cycled.tabs().iter().map(|tab| tab.id).collect();
                    let at = order.iter().position(|id| *id == expected).unwrap();
                    expected = order[if forward {
                        (at + 1) % order.len()
                    } else {
                        (at + order.len() - 1) % order.len()
                    }];
                    assert_eq!(cycled.cycle(forward), Some(expected));
                    check(&mut cycled, &views, Some(expected));
                }

                let mut cycled = base.clone();
                let shown: Vec<_> = cycled.shown().iter().map(|tab| tab.id).collect();
                let mut expected = selected;
                for _ in 0..=shown.len() {
                    let at = shown
                        .iter()
                        .position(|id| *id == expected)
                        .unwrap_or_else(|| {
                            home.and_then(|id| shown.iter().position(|shown| *shown == id))
                                .unwrap_or(shown.len() - 1)
                        });
                    expected = shown[if forward {
                        (at + 1) % shown.len()
                    } else {
                        (at + shown.len() - 1) % shown.len()
                    }];
                    assert_eq!(cycled.cycle_shown(forward), Some(expected));
                    check(&mut cycled, &views, Some(expected));
                }
            }
        }
    }

    let mut drained = base;
    for &id in &tabs {
        assert!(drained.switch_to(id));
        check(&mut drained, &views, Some(id));
        drained.remove(id).expect("present tab");
        let expected = drained.tabs().first().map(|tab| tab.id);
        check(&mut drained, &views, expected);
    }
    assert_eq!(drained.cycle(true), None);
    assert_eq!(drained.cycle_shown(false), None);
    check(&mut drained, &views, None);

    let mut one = TabSet::new(Tab::new(
        tabs[0],
        views.iter().next().expect("a view").0,
        TabPresentation::terminal("one"),
    ));
    check(&mut one, &views, Some(tabs[0]));
    assert_eq!(one.cycle(false), Some(tabs[0]));
    assert_eq!(one.cycle_shown(true), Some(tabs[0]));
    check(&mut one, &views, Some(tabs[0]));
    one.active_mut().expect("selected tab").id = stale;
    assert_eq!(one.active_id(), Some(tabs[0]));
    assert_eq!(
        one.active_index(),
        None,
        "a replaced ID is not the selection"
    );
    assert!(one.active().is_none());
    assert!(one.active_mut().is_none());
    assert!(!one.invariant_holds(&views));
    one.tab_at_mut(0).expect("remaining tab").id = tabs[0];
    check(&mut one, &views, Some(tabs[0]));
}

/// Mutable tab access can relocate the selected identity without using a TabSet move. Every
/// later operation must treat its remembered position as a hint, never as the selection.
#[test]
fn relocated_active_identity_survives_a_stale_position_hint() {
    fn relocated() -> (TabSet, ViewStore, Vec<TabId>) {
        let (mut set, views, ids) = parked_set(3, 0);
        assert!(set.switch_to(ids[0]));
        let last = set.tab_at(2).expect("last tab").clone();
        let first = std::mem::replace(set.active_mut().expect("first tab"), last);
        *set.tab_at_mut(2).expect("last slot") = first;
        // This check must not repair a position hint to accept the valid identity move.
        assert!(set.invariant_holds(&views));
        assert_eq!(set.active_id(), Some(ids[0]));
        (set, views, ids)
    }

    fn check(set: &TabSet, views: &ViewStore, selected: TabId) {
        let cold = set.clone();
        let searched = set.tabs().iter().position(|tab| tab.id == selected);
        assert_eq!(set.active_id(), Some(selected));
        assert_eq!(set.active_index(), searched);
        assert_eq!(set.active().map(|tab| tab.id), Some(selected));
        assert_eq!(*set, cold, "repairing the hint does not change the tab set");
        assert!(set.invariant_holds(views));
    }

    let (set, views, ids) = relocated();
    check(&set, &views, ids[0]);

    let (mut set, views, ids) = relocated();
    assert!(set.reorder(ids[0], 1));
    check(&set, &views, ids[0]);

    let (mut set, views, ids) = relocated();
    set.remove(ids[1]).expect("unselected tab");
    check(&set, &views, ids[0]);

    let (mut set, views, ids) = relocated();
    set.remove(ids[0]).expect("selected tab");
    check(&set, &views, ids[1]);

    let (mut set, views, ids) = relocated();
    assert_eq!(set.cycle(true), Some(ids[2]));
    check(&set, &views, ids[2]);

    let (mut set, views, ids) = relocated();
    assert_eq!(set.cycle_shown(false), Some(ids[1]));
    check(&set, &views, ids[1]);
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

/// A SIDEBAR'S SEAT IS FIRST, BESIDE THE WHOLE TREE: the new leaf is the root's first child at
/// the share asked for, and everything the tree held is its second child, its own splits and
/// ratios as they were. NEGATIVE CONTROL: `split_leaf` seats its new leaf second at half.
#[test]
fn a_first_seat_puts_the_new_leaf_left_of_the_whole_tree_at_its_share() {
    let mut tree = SplitTree::leaf(1u64);
    assert!(tree.split_leaf(1, SplitAxis::Vertical, 2));
    let held = tree.clone();
    assert!(tree.seat_first(SplitAxis::Horizontal, 9, 0.25));
    assert_eq!(tree.leaves(), vec![9, 1, 2], "the new leaf is first");
    let SplitTree::Split {
        axis,
        ratio,
        first,
        second,
    } = &tree
    else {
        panic!("a split at the root");
    };
    assert_eq!((*axis, *ratio), (SplitAxis::Horizontal, 0.25));
    assert_eq!(**first, SplitTree::Leaf(9));
    assert_eq!(**second, held, "what the tree held is whole on the right");
    let bounds = LogicalRect::new(0.0, 0.0, 121.0, 24.0);
    let leaves = tree.layout(bounds, 1.0, 1.0);
    assert_eq!(leaves[0].rect.origin.x, 0.0);
    assert_eq!(leaves[0].rect.size.width, 30.0, "a quarter of 120 cells");
    assert!(
        !tree.seat_first(SplitAxis::Horizontal, 9, 0.25),
        "held once"
    );
    assert!(!tree.seat_first(SplitAxis::Horizontal, 7, f32::NAN));

    let mut second_seat = SplitTree::leaf(1u64);
    assert!(second_seat.split_leaf(1, SplitAxis::Horizontal, 9));
    assert_eq!(second_seat.leaves(), vec![1, 9]);
}

#[test]
fn a_tabs_first_seat_focuses_the_new_view_and_ends_a_zoom() {
    let mut store = ViewStore::default();
    let terminal = store.insert_terminal(11).expect("terminal view");
    let native = store
        .insert_native(AppInstanceId::from_stored(7))
        .expect("native view");
    let mut tab = Tab::new(
        TabId::from_stored(3),
        terminal,
        TabPresentation::terminal("shell"),
    );
    assert!(tab.seat_first(SplitAxis::Horizontal, native, 0.25));
    assert_eq!(tab.root.leaves(), vec![native, terminal]);
    assert_eq!(tab.focus, native);
    assert!(!tab.zoomed);
    assert!(!tab.seat_first(SplitAxis::Horizontal, native, 0.25));
    assert!(tab.invariant_holds(&store));
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

#[test]
fn title_membership_custody_follows_real_mutation_and_clones_start_unbound() {
    let (mut tabs, _, ids) = parked_set(2, 1);
    let mut authority = TitleAuthority::default();
    let custody = tabs.bind_title_owner(11, &authority);
    assert_eq!(
        authority.take_pending().into_iter().collect::<Vec<_>>(),
        [11]
    );
    let token = authority.revision();
    tabs.presentation_at_mut(0).unwrap().title = "presentation".into();
    tabs.switch_to(ids[1]);
    tabs.reorder(ids[0], 1);
    assert!(token.is_current(&authority));
    let mut clone = tabs.clone();
    let _ = clone.active_mut();
    assert!(
        token.is_current(&authority),
        "cloning does not subscribe a second payload"
    );
    let _ = tabs.active_mut();
    assert!(!token.is_current(&authority));
    assert_eq!(
        authority.take_pending().into_iter().collect::<Vec<_>>(),
        [11]
    );
    drop(custody);
    let token = authority.revision();
    let _ = tabs.tab_at_mut(0);
    assert!(
        token.is_current(&authority),
        "retired payload cannot wake its former owner"
    );
}

#[test]
fn title_owner_late_release_cannot_remove_a_cross_owner_binding() {
    let (mut tabs, _, _) = parked_set(1, 0);
    let mut authority = TitleAuthority::default();
    let old = tabs.bind_title_owner(11, &authority);
    let new = tabs.bind_title_owner(22, &authority);
    drop(old);
    let _ = authority.take_pending();
    let _ = tabs.active_mut();
    assert_eq!(
        authority.take_pending().into_iter().collect::<Vec<_>>(),
        [22]
    );
    let mut other = TitleAuthority::default();
    let _other = tabs.bind_title_owner(22, &other);
    drop(new);
    let _ = other.take_pending();
    let _ = tabs.active_mut();
    assert_eq!(other.take_pending().into_iter().collect::<Vec<_>>(), [22]);
}

#[test]
fn title_owner_old_incarnation_cannot_revoke_the_same_owner_after_rebinding() {
    let (mut tabs, _, _) = parked_set(1, 0);
    let mut authority = TitleAuthority::default();
    let old = tabs.bind_title_owner(7, &authority);
    let _new = tabs.bind_title_owner(7, &authority);
    drop(old);
    let _ = authority.take_pending();
    let _ = tabs.active_mut();
    assert_eq!(
        authority.take_pending().into_iter().collect::<Vec<_>>(),
        [7]
    );
}
