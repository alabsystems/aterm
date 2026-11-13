// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::native_app::AppKind;
use crate::{App, WindowId};

#[test]
fn open_aterm_toml_menu_command_is_the_native_manual_editor() {
    const ROOT: &str = "ATERM_NATIVE_PREFERENCES_MENU_ROOT";
    if !crate::test_alone::in_child() {
        let root = aterm_tempfile::TempDir::new().unwrap();
        crate::test_alone::run_alone_with(&[
            (ROOT, root.path().as_os_str()),
            ("XDG_CONFIG_HOME", root.path().as_os_str()),
        ]);
        return;
    }

    let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
    let mut app = App::headless_for_test();
    app.open_config_editor_in_front()
        .expect("the menu command opens aterm.toml");
    assert!(
        root.join("aterm/aterm.toml").is_file(),
        "Manual must resolve only the isolated config file"
    );
    let (instance, _) = app
        .active_native_view(WindowId(0))
        .expect("native Manual editor");
    assert_eq!(
        app.native_runtime.app(instance).map(|app| app.kind()),
        Some(AppKind::Editor)
    );
    assert!(app.windows[&WindowId(0)].front_terminal().is_none());
    assert!(app.windows[&WindowId(0)].overlay.is_none());
}
