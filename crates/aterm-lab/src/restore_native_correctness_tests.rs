// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NATIVE: &str = "window id=0 focused=true tabs=1 active=8 rows=48 cols=110\n\
tab id=8 index=1 kind=app title=\"file\" state=clean active=true parked=false\n\
leaf path=/ view=10 rect_px=0,0,880,720 focused=true kind=app app=editor title=\"file.txt\"\n";

const USED: &str = "window id=0 focused=true tabs=2 active=7 rows=48 cols=110\n\
tab id=7 index=1 kind=terminal title=\"shell\" state=clean active=true parked=false\n\
leaf path=/ view=9 rect_px=0,0,880,720 focused=true kind=terminal session=0\n\
tab id=8 index=2 kind=app title=\"file\" state=clean active=false parked=false\n\
leaf path=/ view=10 rect_px=0,0,880,720 focused=true kind=app app=editor title=\"file.txt\"\n";

#[test]
fn native_only_completion_admits_only_the_explicitly_used_bootstrap() {
    let expected = shape(NATIVE).unwrap();
    let used = shape(USED).unwrap();
    finished_shape(&expected, &expected, false).unwrap();
    finished_shape(&expected, &used, true).unwrap();
    assert!(finished_shape(&expected, &used, false).is_err());
    assert!(finished_shape(&expected, &expected, true).is_err());
    let stole_focus = USED
        .replace("tabs=2 active=7", "tabs=2 active=8")
        .replace(
            "state=clean active=true parked=false",
            "state=clean active=false parked=false",
        )
        .replace(
            "kind=app title=\"file\" state=clean active=false",
            "kind=app title=\"file\" state=clean active=true",
        );
    assert!(finished_shape(&expected, &shape(&stole_focus).unwrap(), true).is_err());
    assert_eq!(terminal_view(USED, 0).unwrap(), 9);
    assert!(terminal_view(NATIVE, 0).is_err());
    let wrong = USED.replace("title=\"file.txt\"", "title=\"wrong.txt\"");
    assert!(finished_shape(&expected, &shape(&wrong).unwrap(), true).is_err());
}

#[test]
fn close_seed_confirms_only_its_own_tab_close() {
    let mut commands = Vec::new();
    request_fixture_action("CloseTab", "close-tab", |command| {
        commands.push(command.to_owned());
        Ok(if command == "invoke CloseTab" {
            "OK confirm pending kind=close-tab window=0; help".into()
        } else {
            "OK answered=yes kind=close-tab".into()
        })
    })
    .unwrap();
    assert_eq!(commands, ["invoke CloseTab", "confirm yes"]);
    for reply in [
        "OK confirm pending kind=quit window=0",
        "OK confirm pending kind=close-tab window=1",
        "OK confirm pending kind=close-pane window=0",
        "OK invoked Quit",
        "ERR refused",
    ] {
        commands.clear();
        assert!(
            request_fixture_action("CloseTab", "close-tab", |command| {
                commands.push(command.to_owned());
                Ok(reply.into())
            })
            .is_err()
        );
        assert_eq!(commands, ["invoke CloseTab"]);
    }
}

#[test]
fn closed_view_oracle_requires_the_exact_graft_or_unchanged_fallback_survivor() {
    let terminal = "window id=0 focused=true tabs=1 active=7 rows=48 cols=110\n\
        tab id=7 index=1 kind=terminal title=\"shell\" state=clean active=true parked=false\n\
        leaf path=/ view=9 rect_px=0,0,880,720 focused=true kind=terminal session=0\n";
    let graft = "window id=0 focused=true tabs=1 active=7 rows=48 cols=110\n\
        tab id=7 index=1 kind=mixed title=\"shell\" state=clean active=true parked=false\n\
        split path=/ axis=horizontal ratio=0.3700 rect_px=0,0,880,720\n\
        leaf path=/0 view=9 rect_px=0,0,325,720 focused=false kind=terminal session=0\n\
        leaf path=/1 view=10 rect_px=326,0,554,720 focused=true kind=app app=editor title=\"closed.txt\"\n";
    let before = shape(terminal).unwrap();
    judge_closed_shape(ClosedRoute::ViewGraft, &before, &shape(graft).unwrap(), 0).unwrap();
    for bad in [
        graft.replace("0.3700", "0.5000"),
        graft.replace("session=0", "session=1"),
        graft
            .replace("focused=false kind=terminal", "focused=true kind=terminal")
            .replace("focused=true kind=app", "focused=false kind=app"),
        graft
            .replace("path=/0", "path=/x")
            .replace("path=/1", "path=/0")
            .replace("path=/x", "path=/1"),
    ] {
        assert!(
            judge_closed_shape(ClosedRoute::ViewGraft, &before, &shape(&bad).unwrap(), 0).is_err()
        );
    }
    let split = "window id=0 focused=true tabs=1 active=7 rows=48 cols=110\n\
        tab id=7 index=1 kind=terminal title=\"shell\" state=clean active=true parked=false\n\
        split path=/ axis=vertical ratio=0.5000 rect_px=0,0,880,720\n\
        leaf path=/0 view=9 rect_px=0,0,880,359 focused=true kind=terminal session=0\n\
        leaf path=/1 view=11 rect_px=0,360,880,360 focused=false kind=terminal session=1\n";
    let fallback = format!(
        "{}{}",
        split
            .replace("tabs=1 active=7", "tabs=2 active=8")
            .replace("state=clean active=true", "state=clean active=false"),
        "tab id=8 index=2 kind=app title=\"closed\" state=clean active=true parked=false\n\
        leaf path=/ view=10 rect_px=0,0,880,720 focused=true kind=app app=editor title=\"closed.txt\"\n"
    );
    judge_closed_shape(
        ClosedRoute::ViewFallback,
        &shape(split).unwrap(),
        &shape(&fallback).unwrap(),
        0,
    )
    .unwrap();
    let bad = fallback.replace("0.5000", "0.4000");
    assert!(
        judge_closed_shape(
            ClosedRoute::ViewFallback,
            &shape(split).unwrap(),
            &shape(&bad).unwrap(),
            0
        )
        .is_err()
    );
    assert!(
        judge_closed_shape(
            ClosedRoute::ViewFallback,
            &shape(split).unwrap(),
            &shape(graft).unwrap(),
            0
        )
        .is_err()
    );
}

#[test]
fn live_edit_reuse_requires_dirty_state_without_weakening_clean_restore_checks() {
    let topology = shape(NATIVE).unwrap();
    let dirty = "OK 1\ndoc=1 uri=file:///closed.txt seq=2 dirty=1 disk=same views=10 ticket=- cursor=- sel=-\n";
    assert_eq!(
        document_positions_with_dirty(&topology, dirty, true).unwrap(),
        BTreeMap::from([("1:1:/".to_owned(), "file:///closed.txt".to_owned())])
    );
    assert!(document_positions(&topology, dirty).is_err());
    assert!(
        document_positions_with_dirty(&topology, &dirty.replace("dirty=1", "dirty=0"), true)
            .is_err()
    );
    assert!(
        document_positions_with_dirty(&topology, &dirty.replace("views=10", "views=11"), true)
            .is_err()
    );
    assert!(
        document_positions_with_dirty(&topology, &dirty.replace("disk=same", "disk=changed"), true)
            .is_err()
    );
}

#[test]
fn held_native_input_requires_the_exact_focused_split_leaf() {
    let split = "window id=0 focused=true tabs=1 active=7 rows=48 cols=110\n\
        tab id=7 index=1 kind=mixed title=\"shell\" state=clean active=true parked=false\n\
        split path=/ axis=horizontal ratio=0.5000 rect_px=0,0,880,720\n\
        leaf path=/0 view=9 rect_px=0,0,440,720 focused=true kind=terminal session=0\n\
        leaf path=/1 view=11 rect_px=441,0,439,720 focused=false kind=terminal session=1\n";
    assert_eq!(focused_terminal_tab(split, 0, 0, 9).unwrap(), 7);
    let wrong_pane = split
        .replace(
            "focused=true kind=terminal session=0",
            "focused=false kind=terminal session=0",
        )
        .replace(
            "focused=false kind=terminal session=1",
            "focused=true kind=terminal session=1",
        );
    // Both terminals still belong to the active tab in the focused window.
    assert!(focused_terminal_tab(&wrong_pane, 0, 0, 9).is_err());
    for bad in [
        split.replace("view=9", "view=12"),
        split.replace("session=0", "session=2"),
        split.replace("focused=true kind=terminal", "focused=false kind=terminal"),
        split.replace("focused=false kind=terminal", "focused=true kind=terminal"),
        split.replace("state=clean active=true", "state=clean active=false"),
        split.replace("tabs=1 active=7", "tabs=1 active=8"),
        split.replace("window id=0 focused=true", "window id=0 focused=false"),
        format!("{split}{split}"),
    ] {
        assert!(focused_terminal_tab(&bad, 0, 0, 9).is_err(), "{bad}");
    }
    assert!(focused_terminal_tab(split, 1, 0, 9).is_err());
    // A hidden tab may retain its own focused leaf without owning keyboard input.
    let hidden = format!(
        "{split}tab id=8 index=2 kind=terminal active=false\n\
        leaf path=/ view=12 focused=true kind=terminal session=2\n"
    );
    assert_eq!(focused_terminal_tab(&hidden, 0, 0, 9).unwrap(), 7);
    assert!(focused_terminal_tab(&hidden, 0, 2, 12).is_err());
}
