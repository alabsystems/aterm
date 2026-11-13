// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};
use aterm_session::{Op, SessionId};

fn register_stub(app: &mut App, id: u64) -> SessionId {
    let s = crate::stub_session(id);
    let sid = s.ctx.self_id.clone();
    App::register_session(&app.store, &s, None);
    app.pool.insert(s);
    sid
}

fn session0_sid(app: &App) -> SessionId {
    app.store
        .read()
        .unwrap()
        .by_local(0)
        .expect("harness registers session 0")
        .sid
        .clone()
}

#[test]
fn precheck_refuses_headless_window_and_dead_origins_but_allows_tabs() {
    let app = App::headless_for_test();
    let sid0 = session0_sid(&app);
    assert_eq!(
        app.connected_spawn_precheck(ConnectedSpawnPlace::Window, &sid0),
        Err("headless: use place=tab".to_string()),
        "place=window under headless refuses BEFORE any create (design §1.4#7)"
    );
    assert_eq!(
        app.connected_spawn_precheck(ConnectedSpawnPlace::Tab, &sid0),
        Ok((0, Some(WindowId(0)))),
        "place=tab is headless-legal and lands beside the origin"
    );
    assert_eq!(
        app.connected_spawn_precheck(ConnectedSpawnPlace::Tab, &SessionId::new("s-nope")),
        Err("no such session".to_string())
    );
    app.store
        .write()
        .unwrap()
        .set_state(0, session_store::SessionState::Exited);
    assert_eq!(
        app.connected_spawn_precheck(ConnectedSpawnPlace::Tab, &sid0),
        Err("exited".to_string()),
        "a dead origin can neither drive nor be driven"
    );
}

#[test]
fn mint_connected_writes_a_both_connection_in_the_right_direction() {
    let mut app = App::headless_for_test();
    let origin = session0_sid(&app);
    let newborn = register_stub(&mut app, 41);

    assert_eq!(
        app.mint_connected(ConnectedSpawnKind::Controlled, &origin, &newborn, "test"),
        Ok(())
    );
    let rows = {
        let ctx = {
            let g = app.store.read().unwrap();
            g.by_sid(&newborn).unwrap().ctx.clone()
        };
        let edges = ctx.edges.lock().unwrap();
        edges.edges()
    };
    assert_eq!(rows.len(), 3, "both = pull + push = 3 ops: {rows:?}");
    for row in &rows {
        assert_eq!(row.src, origin, "the origin drives the newborn");
        assert_eq!(row.dst, newborn);
    }
    let ops: Vec<Op> = rows.iter().map(|e| e.op).collect();
    for op in [Op::ReadScreen, Op::WriteInput, Op::Signal] {
        assert!(ops.contains(&op), "missing {op:?} in {ops:?}");
    }
    let roles = crate::connections::roles_by_local(&app.store);
    assert!(roles[&0].outbound && !roles[&0].inbound);
    assert!(roles[&41].inbound && !roles[&41].outbound);

    let supervisor = register_stub(&mut app, 42);
    assert_eq!(
        app.mint_connected(ConnectedSpawnKind::Controller, &origin, &supervisor, "test"),
        Ok(())
    );
    let origin_rows = {
        let ctx = {
            let g = app.store.read().unwrap();
            g.by_sid(&origin).unwrap().ctx.clone()
        };
        let edges = ctx.edges.lock().unwrap();
        edges.edges()
    };
    assert_eq!(origin_rows.len(), 3);
    for row in &origin_rows {
        assert_eq!(row.src, supervisor, "the newborn supervises the origin");
        assert_eq!(row.dst, origin);
    }
}

#[cfg(unix)]
#[test]
fn a_connected_tab_rechecks_its_origin_before_installing() {
    use crate::tab_spawn::{SpawnAdmission, TabSpawned};
    use std::{cell::RefCell, rc::Rc};
    for kind in [
        ConnectedSpawnKind::Controlled,
        ConnectedSpawnKind::Controller,
    ] {
        for change in ["exited", "removed", "unhosted", "unchanged"] {
            let mut app = App::headless_for_test();
            let origin = session0_sid(&app);
            let (local, host) = app
                .connected_spawn_precheck(ConnectedSpawnPlace::Tab, &origin)
                .unwrap();
            let host = host.unwrap();
            let original = app
                .store
                .read()
                .unwrap()
                .by_sid(&origin)
                .unwrap()
                .ctx
                .clone();
            let heard = Rc::new(RefCell::new(None));
            let answer = Rc::clone(&heard);
            let then = App::connected_landing(
                kind,
                origin.clone(),
                Box::new(move |_, result| {
                    *answer.borrow_mut() = Some(result);
                }),
            );
            let id = crate::tab_spawn::tests::book_admitted(
                &mut app,
                host,
                SpawnAdmission::Connected {
                    origin: origin.clone(),
                    local,
                },
                then,
            );
            match change {
                "exited" => {
                    app.store
                        .write()
                        .unwrap()
                        .set_state(local, session_store::SessionState::Exited);
                }
                "removed" => app.retire_session_registration(local),
                "unhosted" => {
                    app.windows.get_mut(&host).unwrap().tab_set =
                        crate::tab_model::TabSet::default();
                }
                _ => {}
            }
            let session = crate::stub_session(id);
            let newborn = session.ctx.self_id.clone();
            let newborn_ctx = session.ctx.clone();
            let landed = app.install_spawned_tab(TabSpawned {
                id,
                window: host,
                session: Ok(session),
                reader_gate: None,
            });
            if change == "unchanged" {
                assert!(landed);
                assert_eq!(*heard.borrow(), Some(Ok(newborn.as_str().to_string())));
                let edge_ctx = if matches!(kind, ConnectedSpawnKind::Controlled) {
                    &newborn_ctx
                } else {
                    &original
                };
                assert_eq!(edge_ctx.edges.lock().unwrap().edges().len(), 3);
            } else {
                assert!(
                    !landed,
                    "{kind:?} origin {change}: no newborn may be installed"
                );
                assert!(heard.borrow().as_ref().unwrap().is_err());
                assert!(app.store.read().unwrap().by_sid(&newborn).is_none());
                assert!(app.pool.get(id).is_none());
                assert!(original.edges.lock().unwrap().edges().is_empty());
                assert!(newborn_ctx.edges.lock().unwrap().edges().is_empty());
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn a_connected_window_lands_connected_only_while_its_origin_lives() {
    use crate::tab_spawn::{SpawnAdmission, TabSpawned};
    use std::{cell::RefCell, rc::Rc};
    for kind in [
        ConnectedSpawnKind::Controlled,
        ConnectedSpawnKind::Controller,
    ] {
        for change in ["exited", "removed", "unchanged"] {
            let mut app = App::headless_for_test();
            let origin = session0_sid(&app);
            let (local, _) = app
                .connected_spawn_precheck(ConnectedSpawnPlace::Tab, &origin)
                .unwrap();
            let windows = app.windows.len();
            let heard = Rc::new(RefCell::new(None));
            let answer = Rc::clone(&heard);
            let then = App::connected_landing(
                kind,
                origin.clone(),
                Box::new(move |_, result| {
                    *answer.borrow_mut() = Some(result);
                }),
            );
            let lineage = matches!(kind, ConnectedSpawnKind::Controlled).then(|| origin.clone());
            let (id, wid) = crate::tab_spawn::test_support::book_window(
                &mut app,
                lineage,
                SpawnAdmission::Connected {
                    origin: origin.clone(),
                    local,
                },
                then,
            );
            match change {
                "exited" => {
                    app.store
                        .write()
                        .unwrap()
                        .set_state(local, session_store::SessionState::Exited);
                }
                "removed" => app.retire_session_registration(local),
                _ => {}
            }
            let (session, _pty) = crate::tab_spawn::tests::registered_piped_session(id);
            let newborn = session.ctx.self_id.clone();
            let landed = app.install_spawned_tab(TabSpawned {
                id,
                window: wid,
                session: Ok(session),
                reader_gate: None,
            });
            if change == "unchanged" {
                assert!(landed, "{kind:?}: the window lands");
                assert_eq!(app.frontmost_window, Some(wid));
                assert_eq!(app.focused_session_id(wid), Some(id));
                assert_eq!(*heard.borrow(), Some(Ok(newborn.as_str().to_string())));
                let store = app.store.read().unwrap();
                let parent = store.by_sid(&newborn).unwrap().parent.clone();
                let expected =
                    matches!(kind, ConnectedSpawnKind::Controlled).then(|| origin.clone());
                assert_eq!(parent, expected, "{kind:?}: the newborn's lineage");
                let dst = match kind {
                    ConnectedSpawnKind::Controlled => &newborn,
                    ConnectedSpawnKind::Controller => &origin,
                };
                let edges = store.by_sid(dst).unwrap().ctx.edges.lock().unwrap().edges();
                assert_eq!(edges.len(), 3, "{kind:?}: both = pull + push = 3 ops");
            } else {
                assert!(!landed, "{kind:?} origin {change}: no window may be made");
                assert!(heard.borrow().as_ref().unwrap().is_err());
                assert_eq!(app.windows.len(), windows);
                assert!(!app.windows.contains_key(&wid));
                assert!(app.store.read().unwrap().by_sid(&newborn).is_none());
                assert!(app.pool.get(id).is_none());
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn a_connected_tab_is_connected_as_it_lands() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let mut app = App::headless_for_test();
    let origin = session0_sid(&app);
    let heard: Rc<RefCell<Vec<Result<String, String>>>> = Rc::default();
    let tell = |heard: &Rc<RefCell<Vec<Result<String, String>>>>| -> crate::tab_spawn::SpawnDone {
        let heard = Rc::clone(heard);
        Box::new(move |_, spawned| heard.borrow_mut().push(spawned))
    };
    let then = App::connected_landing(ConnectedSpawnKind::Controller, origin.clone(), tell(&heard));
    let id = crate::tab_spawn::test_support::book(&mut app, WindowId(0), then);
    let session = crate::stub_session(id);
    let newborn = session.ctx.self_id.clone();
    assert!(app.install_spawned_tab(crate::tab_spawn::TabSpawned {
        id,
        window: WindowId(0),
        session: Ok(session),
        reader_gate: None,
    }));
    let rows = {
        let ctx = {
            let g = app.store.read().unwrap();
            g.by_sid(&origin).unwrap().ctx.clone()
        };
        let edges = ctx.edges.lock().unwrap();
        edges.edges()
    };
    assert_eq!(rows.len(), 3, "both = pull + push = 3 ops: {rows:?}");
    assert!(
        rows.iter()
            .all(|row| row.src == newborn && row.dst == origin),
        "the newborn supervises the origin: {rows:?}"
    );

    let then = App::connected_landing(ConnectedSpawnKind::Controller, origin, tell(&heard));
    let id = crate::tab_spawn::test_support::book(&mut app, WindowId(0), then);
    assert!(!app.install_spawned_tab(crate::tab_spawn::TabSpawned {
        id,
        window: WindowId(0),
        session: Err(std::io::Error::other("exec refused")),
        reader_gate: None,
    }));
    assert_eq!(
        *heard.borrow(),
        [
            Ok(newborn.as_str().to_string()),
            Err("exec refused".to_string())
        ]
    );
}
