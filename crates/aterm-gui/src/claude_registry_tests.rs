// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The watch's own laws, on real directories and a real kernel queue.
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use aterm_spec::wait::MUST_HAPPEN;

use super::{Awaited, RegistryWatch, Sink, Told, row_of};

/// A watch whose sink is a channel the test reads.
fn watch() -> (RegistryWatch, Receiver<Told>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let sink: Sink = Arc::new(move |told| {
        tx.lock()
            .unwrap_or_else(|p| p.into_inner())
            .send(told)
            .is_ok()
    });
    (RegistryWatch::new(atpkg::dirwatch::Watcher::new, sink), rx)
}

/// Wait for `want`, past what an earlier step's events still tell (a
/// hang detector: what must be told and never is fails the test).
fn until(rx: &Receiver<Told>, want: &Told) {
    loop {
        let told = rx
            .recv_timeout(MUST_HAPPEN)
            .unwrap_or_else(|_| panic!("never told {want:?}"));
        if told == *want {
            return;
        }
    }
}

/// A fresh scratch root for test `name`.
fn root(name: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let owned = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-gui-registry-{name}-"))
        .tempdir()
        .expect("exclusive registry scratch");
    let path = std::fs::canonicalize(owned.path()).unwrap();
    (owned, path)
}

fn awaited(dir: &Path, pid: u32) -> Vec<Awaited> {
    vec![Awaited {
        dir: dir.to_path_buf(),
        pid,
    }]
}

/// Nothing awaited, nothing up; something awaited, the watch is placed
/// (and said fresh once); nothing awaited again, it all goes.
#[test]
fn the_watch_lives_exactly_while_something_is_awaited() {
    let (_owned_root, root) = root("lives");
    let claude = root.join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let (mut watch, _rx) = watch();
    assert!(watch.reconcile(&[]).is_empty());
    assert!(!watch.is_live());
    assert_eq!(
        watch.reconcile(&awaited(&claude, 7)),
        std::slice::from_ref(&claude)
    );
    assert!(watch.is_live());
    assert_eq!(watch.level_of(&claude), Some(claude.join("sessions")));
    assert!(
        watch.reconcile(&awaited(&claude, 7)).is_empty(),
        "a kept watch is not fresh"
    );
    assert!(watch.reconcile(&[]).is_empty());
    assert!(!watch.is_live(), "nothing awaited: no watch, no thread");
    assert_eq!(watch.level_of(&claude), None);
    let (tx, _) = channel::<Told>();
    let tx = Mutex::new(tx);
    let mut refused = RegistryWatch::new(
        || Err(std::io::Error::from(std::io::ErrorKind::Unsupported)),
        Arc::new(move |told| tx.lock().unwrap().send(told).is_ok()),
    );
    assert!(refused.reconcile(&awaited(&claude, 7)).is_empty());
    assert!(!refused.is_live());
}

/// A row created in `sessions/` is told as its Claude dir; a write in
/// place to a row that is there is told through its file watch.
#[test]
fn a_created_row_and_a_row_written_in_place_are_told() {
    let (_owned_root, root) = root("told");
    let claude = root.join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let (mut watch, rx) = watch();
    let _ = watch.reconcile(&awaited(&claude, 7));
    let row = row_of(&claude, 7);
    std::fs::write(&row, b"").unwrap();
    assert_eq!(
        rx.recv_timeout(MUST_HAPPEN).unwrap(),
        Told::Rows(claude.clone())
    );
    assert_eq!(
        watch.reconcile(&awaited(&claude, 7)),
        std::slice::from_ref(&claude)
    );
    assert!(watch.file_watched(&row));
    while rx.try_recv().is_ok() {}
    std::fs::write(&row, b"{}").unwrap();
    assert_eq!(
        rx.recv_timeout(MUST_HAPPEN).unwrap(),
        Told::Rows(claude.clone())
    );
}

/// With `sessions/` and its parent missing, the watch starts on the nearest existing ancestor
/// and moves inward as each level appears.
#[test]
fn the_watch_moves_inward_as_the_levels_appear() {
    let (_owned_root, root) = root("inward");
    let claude = root.join("claude");
    let (mut watch, rx) = watch();
    assert_eq!(
        watch.reconcile(&awaited(&claude, 7)),
        std::slice::from_ref(&claude)
    );
    assert_eq!(watch.level_of(&claude), Some(root.clone()));
    std::fs::write(root.join("settings.json"), b"{}").unwrap();
    assert_eq!(rx.recv_timeout(MUST_HAPPEN).unwrap(), Told::Level);
    assert!(
        watch.reconcile(&awaited(&claude, 7)).is_empty(),
        "nothing new to watch"
    );
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    until(&rx, &Told::Level);
    assert_eq!(
        watch.reconcile(&awaited(&claude, 7)),
        std::slice::from_ref(&claude)
    );
    assert_eq!(
        watch.level_of(&claude),
        Some(claude.join("sessions")),
        "straight to the target"
    );
    std::fs::remove_dir(claude.join("sessions")).unwrap();
    until(&rx, &Told::Rows(claude.clone()));
    assert_eq!(
        watch.reconcile(&awaited(&claude, 7)),
        std::slice::from_ref(&claude)
    );
    assert_eq!(watch.level_of(&claude), Some(claude.clone()));
    std::fs::create_dir(claude.join("sessions")).unwrap();
    until(&rx, &Told::Level);
    assert_eq!(
        watch.reconcile(&awaited(&claude, 7)),
        std::slice::from_ref(&claude)
    );
    assert_eq!(watch.level_of(&claude), Some(claude.join("sessions")));
    while rx.try_recv().is_ok() {}
    std::fs::write(row_of(&claude, 7), b"").unwrap();
    until(&rx, &Told::Rows(claude.clone()));
}

/// A watch the kernel dropped is placed again even if `(dev, ino)` is unchanged.
#[test]
fn a_watch_the_kernel_let_go_is_placed_again() {
    let (_owned_root, root) = root("gone");
    let claude = root.join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let row = row_of(&claude, 7);
    std::fs::write(&row, b"").unwrap();
    let (mut watch, rx) = watch();
    assert_eq!(
        watch.reconcile(&awaited(&claude, 7)),
        std::slice::from_ref(&claude)
    );
    assert_eq!(watch.holds(&claude, &row), (true, true));
    watch.lose_watches(&claude, &row);
    assert_eq!(watch.holds(&claude, &row), (false, false), "let go");
    assert_eq!(
        watch.reconcile(&awaited(&claude, 7)),
        std::slice::from_ref(&claude),
        "placed again, so the row is read again"
    );
    assert_eq!(watch.level_of(&claude), Some(claude.join("sessions")));
    assert_eq!(watch.holds(&claude, &row), (true, true), "both held again");
    std::fs::write(row_of(&claude, 8), b"").unwrap();
    until(&rx, &Told::Rows(claude.clone()));
    std::fs::write(&row, b"{}").unwrap();
    until(&rx, &Told::Rows(claude.clone()));
}

/// The forwarder reports only kernel-fired events, never a bare wake.
#[test]
fn the_forwarder_tells_only_what_the_kernel_fired() {
    let (_owned_root, root) = root("kernel-only");
    let claude = root.join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let row = row_of(&claude, 7);
    std::fs::write(&row, b"").unwrap();
    let (mut watch, rx) = watch();
    assert_eq!(
        watch.reconcile(&awaited(&claude, 7)),
        std::slice::from_ref(&claude)
    );
    assert_eq!(watch.holds(&claude, &row), (true, true));
    for _ in 0..3 {
        watch.wake_and_settle();
        assert_eq!(rx.try_recv().ok(), None, "a wake with nothing fired");
    }
    std::fs::write(&row, b"{}").unwrap();
    assert_eq!(
        rx.recv_timeout(MUST_HAPPEN).unwrap(),
        Told::Rows(claude.clone())
    );
}

/// A row no longer awaited loses its file watch, and a Claude dir no
/// process awaits loses its directory watch, while another stays.
#[test]
fn what_is_no_longer_awaited_is_let_go() {
    let (_owned_root, root) = root("let-go");
    let (a, b) = (root.join("a"), root.join("b"));
    for dir in [&a, &b] {
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
    }
    std::fs::write(row_of(&a, 7), b"").unwrap();
    let (mut watch, _rx) = watch();
    let both = [
        Awaited {
            dir: a.clone(),
            pid: 7,
        },
        Awaited {
            dir: b.clone(),
            pid: 8,
        },
    ];
    assert_eq!(watch.reconcile(&both), [a.clone(), b.clone()]);
    assert!(watch.file_watched(&row_of(&a, 7)));
    watch.let_go(&both[1..]);
    assert!(!watch.file_watched(&row_of(&a, 7)));
    assert_eq!(watch.level_of(&a), None);
    assert_eq!(watch.level_of(&b), Some(b.join("sessions")));
    assert!(watch.is_live());
    watch.let_go(&[]);
    assert!(!watch.is_live());
}
