// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn replace(path: &Path) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, b"").unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn poll(path: Option<PathBuf>) -> (SidePoll, impl Fn() -> usize) {
    let told = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&told);
    let side = SidePoll::new(
        path,
        Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }),
    );
    (side, move || told.load(Ordering::SeqCst))
}

/// Off macOS the fallback is a look at the marks ([`Polled`]), tested on every unix here: a
/// wait answers `true` only for a mark that moved, once per move; a side mark is told and
/// ends no wait; a pulled trigger answers `false` at once and leaves a move for the next look.
#[cfg(unix)]
#[test]
fn the_polled_wake_answers_only_a_mark_that_moved() {
    const TICK: Duration = Duration::from_millis(20);
    const HANG: Duration = Duration::from_secs(60);
    let scratch_dir = aterm_tempfile::Builder::new()
        .prefix("aterm-polled-wake-")
        .tempdir()
        .unwrap();
    let root = scratch_dir.path();
    let layout = atpkg::store::Layout {
        prefix: root.join("pkg"),
    };
    let agents = layout.agents_dir();
    std::fs::create_dir_all(&agents).unwrap();
    let (old, new) = (root.join("old"), root.join("new"));
    std::fs::write(&old, b"old build").unwrap();
    std::fs::write(&new, b"new build").unwrap();
    let twin = agents.join("claude");
    std::os::unix::fs::symlink(&old, &twin).unwrap();
    let home = root.join("home");
    let word = root.join("owner/word");
    let fixed = |path: PathBuf| -> Locate { Box::new(move || Some(path.clone())) };
    let mut wake = Polled::with_claude_sources(Some(&home), vec![fixed(twin.clone())]);
    wake.waking(fixed(word.clone()));
    let store = root.join("accounts");
    let (side, heard) = poll(Some(store.join("wake")));
    wake.sides.push(side);
    let muse_twin = agents.join("muse");
    std::os::unix::fs::symlink(&old, &muse_twin).unwrap();
    let muse_told = Arc::new(AtomicUsize::new(0));
    let muse_count = Arc::clone(&muse_told);
    wake.with_agent(
        agent_twin("muse", move || Some(layout.clone())),
        Box::new(move || {
            muse_count.fetch_add(1, Ordering::SeqCst);
        }),
    );
    assert_eq!(
        wake.marks.last().and_then(|m| m.path.clone()),
        Some(muse_twin.clone()),
        "Muse Code's own mark is the twin atpkg lays for it"
    );

    assert!(!wake.wait(TICK), "nothing moved: no activation");
    assert_eq!(muse_told.load(Ordering::SeqCst), 0);
    let next_muse = agents.join("next-muse");
    std::os::unix::fs::symlink(&new, &next_muse).unwrap();
    std::fs::rename(&next_muse, &muse_twin).unwrap();
    assert!(
        !wake.wait(TICK),
        "an agent's own twin wakes no other agent's worker"
    );
    assert_eq!(
        (muse_told.load(Ordering::SeqCst), heard()),
        (1, 0),
        "the agent's own listener is told, and no other"
    );
    assert!(!wake.wait(TICK));
    assert_eq!(muse_told.load(Ordering::SeqCst), 1, "told once");
    std::fs::create_dir_all(word.parent().unwrap()).unwrap();
    replace(&word);
    assert!(wake.wait(TICK), "the owner's word");
    assert!(!wake.wait(TICK), "told once");
    let next = agents.join("next");
    std::os::unix::fs::symlink(&new, &next).unwrap();
    std::fs::rename(&next, &twin).unwrap();
    assert!(wake.wait(TICK), "the managed twin moved");
    assert_eq!(
        muse_told.load(Ordering::SeqCst),
        1,
        "Claude Code's landing tells no agent's own listener"
    );
    assert!(!wake.wait(TICK));
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::os::unix::fs::symlink(&new, native_launcher(&home)).unwrap();
    assert!(wake.wait(TICK), "the native link appeared");
    std::fs::write(agents.join("unrelated"), b"x").unwrap();
    assert!(!wake.wait(TICK), "a neighbour is no activation");

    std::fs::create_dir_all(&store).unwrap();
    replace(&store.join("wake"));
    assert!(!wake.wait(TICK), "a side mark ends no wait");
    assert_eq!(heard(), 1, "its listener is told");

    replace(&word);
    ring(&wake.pulled);
    assert!(!wake.wait(HANG), "a pulled trigger is no news");
    assert!(wake.wait(TICK), "the word it cut short is the next look's");
}

/// The twin the poll looks at is the file atpkg lays ([`twin_under`]; on Windows the `.cmd`),
/// and a newer build's landing moves it for the next look, on every platform this runs on.
#[test]
fn the_polled_twin_is_the_one_atpkg_lays_and_a_landing_moves_it() {
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-polled-twin-")
        .tempdir()
        .unwrap();
    let layout = atpkg::store::Layout {
        prefix: root.path().canonicalize().unwrap(),
    };
    let claude = atpkg::store::ToolName::new("claude").unwrap();
    let land = |build: &str| {
        let package = layout.prefix.join("store/claude").join(build);
        let exe = package.join("bin").join(claude.exe_file());
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, format!("#!/bin/sh\necho {build}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        atpkg::store::mark_build_ready(&package).unwrap();
        atpkg::activate::activate_build(&layout, &package).unwrap();
        atpkg::activate::install_shims(
            &layout,
            &package,
            &["claude".to_owned()],
            atpkg::Aliases::Off,
        )
        .unwrap();
    };
    land("2026102801");
    let twin = twin_under(&layout, "claude").unwrap();
    assert!(twin.is_file(), "atpkg laid no twin at {}", twin.display());
    let at = twin.clone();
    let mut mark = PolledMark::new(Box::new(move || Some(at.clone())));
    assert!(!mark.changed(), "the laid build is the baseline");
    land("2026102802");
    assert!(mark.changed(), "a newer build's landing moved the twin");
    assert!(!mark.changed(), "told once");
}

#[test]
fn a_fallback_tick_tells_a_side_listener_only_when_its_marker_changed() {
    let scratch_dir = aterm_tempfile::Builder::new()
        .prefix("aterm-accounts-side-poll-")
        .tempdir()
        .unwrap();
    let root = scratch_dir.path();
    let store = root.join("accounts");
    let marker = store.join("wake");

    let (mut none, heard_none) = poll(None);
    let (mut side, heard) = poll(Some(marker.clone()));
    for _ in 0..5 {
        assert!(!side.look() && !none.look());
    }
    assert_eq!((heard(), heard_none()), (0, 0), "nothing there: no news");

    std::fs::create_dir_all(&store).unwrap();
    replace(&marker);
    assert!(side.look(), "the marker was made");
    assert_eq!(heard(), 1);
    for _ in 0..5 {
        assert!(!side.look());
    }
    assert_eq!(heard(), 1, "left alone: no tick tells it again");
    replace(&marker);
    assert!(side.look(), "replaced");
    assert_eq!(heard(), 2);
    std::fs::write(store.join("state.json"), b"{}").unwrap();
    assert!(!side.look());
    assert_eq!(heard(), 2, "a write beside the marker is not it");
    let (mut late, heard_late) = poll(Some(marker.clone()));
    assert!(!late.look());
    assert_eq!(heard_late(), 0, "the baseline is no news");
    std::fs::remove_file(&marker).unwrap();
    assert!(side.look() && !side.look());
    assert_eq!(heard(), 3);
}
