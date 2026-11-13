// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_explicit_command_leaves_the_spare_for_the_next_ordinary_shell() {
    let mut app = App::headless_for_test();
    app.session_factory.shell_override = Some("/bin/zsh".into());
    let window = WindowId(0);
    let grid = app.spawn_grid(window);
    let id = app.next_session_id;
    app.next_session_id += 1;
    let cwd = app
        .focused_pane_folder(window)
        .map(|folder| folder.reported);
    app.spares.slot = Some(super::tests::parked_slot_for(&app, id, cwd.as_deref()));
    let ask = crate::tab_spawn::SpawnAsk {
        exec_command: Some(vec!["/bin/sh".into()]),
        ..crate::tab_spawn::SpawnAsk::default()
    };
    assert_eq!(
        app.try_adopt_spare(window, Some(window), grid, &ask)
            .map(|_| ()),
        Err("ask")
    );
    assert_eq!(app.spares.slot.as_ref().unwrap().id, id);
    let ordinary = crate::tab_spawn::SpawnAsk::default();
    let (taken, _) = app
        .try_adopt_spare(window, Some(window), grid, &ordinary)
        .expect("the same eligible spare remains available to an ordinary shell");
    assert_eq!(taken.id, id);
    assert!(app.spares.slot.is_none());
}
