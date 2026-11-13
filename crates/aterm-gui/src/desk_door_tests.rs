// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const WID: crate::WindowId = crate::WindowId(0);

/// A stub session in a new tab of the window, its shell `pid`: the tab comes in front.
fn tab(app: &mut App, pid: i32) -> u64 {
    let id = app.next_session_id;
    let mut session = crate::stub_session(id);
    session.pid = pid;
    app.push_stub_tab(WID, session);
    id
}

/// A headless app whose one window has an OS surface and keyboard focus, with two more
/// tabs: `front`, the one it shows, and `behind`, each `(session, shell pid)`.
fn two_tabs() -> (App, (u64, i32), (u64, i32)) {
    const FRONT_SHELL: i32 = 4_100_001;
    const BEHIND_SHELL: i32 = 4_100_002;
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WID).expect("its window");
    ws.focused = true;
    ws.os_window_for_test = true;
    let behind = tab(&mut app, BEHIND_SHELL);
    let front = tab(&mut app, FRONT_SHELL);
    assert!(app.active_tab_shows_session(WID, front), "PRECONDITION");
    assert!(!app.active_tab_shows_session(WID, behind), "PRECONDITION");
    (app, (front, FRONT_SHELL), (behind, BEHIND_SHELL))
}

/// A process table where `child`'s parent is `shell`, and nothing else has one.
fn child_of(child: i32, shell: i32) -> impl Fn(i32) -> Option<i32> {
    move |pid| (pid == child).then_some(shell)
}

#[test]
fn a_client_stands_where_its_tab_does() {
    assert_eq!(standing(None, |_| true), Standing::Outside);
    assert_eq!(standing(Some(3), |_| true), Standing::InFront(3));
    assert_eq!(standing(Some(3), |_| false), Standing::Behind(3));
}

/// AN AGENT IN A BACKGROUND TAB TAKES NOTHING: a client whose process descends from the
/// shell of a tab behind the one shown is refused each desk effect, in words that name the
/// effect. NEGATIVE CONTROL: the same client under the shown tab's shell is admitted, and so
/// is one in no tab at all (the lab's driver).
#[test]
fn a_client_in_a_tab_behind_is_refused_and_one_in_front_admitted() {
    let (app, (front, front_shell), (behind, behind_shell)) = two_tabs();
    const CLIENT: i32 = 4_000_001;
    let none = |_| None;
    let at = |shell: i32| {
        app.desk_standing_with(Asker::Client(Some(CLIENT)), &child_of(CLIENT, shell), &none)
    };
    assert_eq!(at(behind_shell), Standing::Behind(behind));
    assert_eq!(at(front_shell), Standing::InFront(front), "control");
    assert_eq!(
        app.desk_standing_with(Asker::Client(Some(CLIENT)), &none, &none),
        Standing::Outside,
        "control: a client in no tab"
    );
    for effect in [
        Effect::Raise,
        Effect::SpawnInFront,
        Effect::Open,
        Effect::SystemSettings,
    ] {
        let refused = app
            .desk_judges(at(behind_shell), effect)
            .expect_err("behind");
        assert!(
            aterm_types::macos_setup::desk_refused(&refused),
            "{refused}"
        );
        assert!(refused.contains(effect.words()), "{refused}");
        assert_eq!(app.desk_judges(at(front_shell), effect), Ok(()), "control");
        assert_eq!(
            app.desk_judges(Standing::Outside, effect),
            Ok(()),
            "control"
        );
    }
}

/// THE TAB IN FRONT OF NOBODY IS BEHIND: the shown tab's own client is refused once its
/// window loses keyboard focus (aterm behind another app), since what it brought up would
/// come up over that app. NEGATIVE CONTROL: focus back, it is admitted again.
#[test]
fn the_shown_tab_is_behind_while_aterm_is_behind_another_app() {
    let (mut app, (front, front_shell), _) = two_tabs();
    const CLIENT: i32 = 4_000_002;
    let none = |_| None;
    let at = |app: &App| {
        app.desk_standing_with(
            Asker::Client(Some(CLIENT)),
            &child_of(CLIENT, front_shell),
            &none,
        )
    };
    app.window_focus_event(WID, false);
    assert_eq!(at(&app), Standing::Behind(front));
    app.window_focus_event(WID, true);
    assert_eq!(at(&app), Standing::InFront(front), "control");
}

/// aterm's own lanes and a client whose pid cannot be read are placed in no tab: the door
/// has no opinion of them.
#[test]
fn an_asker_the_door_cannot_place_is_outside() {
    let (app, ..) = two_tabs();
    assert_eq!(app.desk_standing(Asker::Own), Standing::Outside);
    assert_eq!(app.desk_standing(Asker::Client(None)), Standing::Outside);
}
