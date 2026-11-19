// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;
use crate::native_app::{AppKind, NativeApp};
use crate::native_registry::RegistryApp;
use crate::native_tables::TablesApp;
use crate::native_tables::native_tables_tests::{WID, act, put_in, render, said};
use crate::restore::{NativeLeafRestore, RestoreManifest, RestoredSplitTree, RestoredView};
use crate::tables_store::{FAMILIES, Paths};
use aterm_db::alab::{Registry, registry::MACHINE};
use aterm_db::table::fixture::Lab;
use aterm_json::json;
use aterm_secrets::commons::roster::Role;

fn descriptors(manifest: &RestoreManifest) -> Vec<NativeLeafRestore> {
    manifest.windows[0]
        .restored_tabs
        .iter()
        .filter_map(|tab| match &tab.root {
            RestoredSplitTree::Leaf {
                view: RestoredView::Native(native),
            } => Some(native.clone()),
            _ => None,
        })
        .collect()
}

/// Give a fresh process's two readers the private lab store. Placement, routes and all view
/// state come exclusively from the serialized manifest through the ordinary detached restorer.
fn relaunch(manifest: &RestoreManifest, paths: &Paths) -> App {
    let mut cold = App::headless_for_test();
    let tables = TablesApp::new(Some(paths.clone()), FAMILIES, || {}).unwrap();
    cold.native_runtime
        .insert_instance(NativeApp::Tables(Box::new(tables)))
        .unwrap();
    let registry = RegistryApp::new(Some(paths.clone()), crate::qos::Role::Background, || {});
    cold.native_runtime
        .insert_instance(NativeApp::Registry(Box::new(registry)))
        .unwrap();
    let serialized = manifest.to_toml().unwrap();
    let read = RestoreManifest::from_toml(&serialized).unwrap();
    cold.restore_into_window(WID, read.windows.into_iter().next().unwrap());
    cold
}

fn shows(app: &mut App, key: &str, words: &str) {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, key, || {
        app.on_registry_changed();
        app.on_tables_changed();
        (said(&render(app), key).as_deref() == Some(words)).then_some(())
    });
}

fn kind_in_front(app: &App) -> AppKind {
    let (instance, _) = app.active_native_view(WID).unwrap();
    app.native_runtime.app(instance).unwrap().kind()
}

#[test]
fn registry_hands_a_machine_to_the_existing_tables_tab_and_both_restore_their_own_row() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let first = "mc-0000000000000001";
    let next = "mc-0000000000000002";
    let store = Registry::at(paths.clone());
    store.open().unwrap();
    store
        .write(vec![
            vec![put_in(&MACHINE, first, "label", json!("m1"))],
            vec![put_in(&MACHINE, next, "label", json!("m2"))],
        ])
        .unwrap();

    let mut original = App::headless_for_test();
    original.open_tables_tab_at(Some(paths.clone())).unwrap();
    let legacy = original.capture_restore_manifest();
    assert_eq!(descriptors(&legacy), [NativeLeafRestore::tables(None)]);
    let mut app = relaunch(&legacy, paths);
    assert_eq!(kind_in_front(&app), AppKind::Tables);
    let tables_view = app.active_native_view(WID).unwrap();
    let tab_count = app.windows[&WID].tab_set.len();

    app.open_registry_tab().unwrap();
    let choice = format!("registry/machine/{next}");
    shows(&mut app, &choice, "m2");
    act(&mut app, &choice, None);
    shows(&mut app, "registry/detail/name", "m2");
    act(&mut app, "registry/open-in-tables", None);
    assert_eq!(app.active_native_view(WID), Some(tables_view));
    assert_eq!(app.windows[&WID].tab_set.len(), tab_count + 1);
    shows(&mut app, "tables/pane/title", "m2");

    // The original grid automation key still selects another row; the product keeps its machine.
    act(&mut app, &format!("tables/cell/{first}/label"), None);
    shows(&mut app, "tables/pane/title", "m1");
    app.open_registry_tab().unwrap();
    shows(&mut app, "registry/detail/name", "m2");
    let saved = app.capture_restore_manifest();
    assert_eq!(
        descriptors(&saved),
        [
            NativeLeafRestore::tables(Some(format!("machine/{first}"))),
            NativeLeafRestore::registry(Some(next)),
        ]
    );

    let mut cold = relaunch(&saved, paths);
    assert_eq!(kind_in_front(&cold), AppKind::Registry);
    shows(&mut cold, "registry/detail/name", "m2");
    cold.open_tables_tab().unwrap();
    shows(&mut cold, "tables/pane/title", "m1");
    assert!(cold.structural_invariants_ok());
}
