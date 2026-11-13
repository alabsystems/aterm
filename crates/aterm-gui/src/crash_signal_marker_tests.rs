// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::MarkerOwner;
use super::markers::{self, Owner};
use crate::crash_journal::tests::{reaped_pid, scratch};
use std::path::Path;

fn let_go(fd: i32) {
    // SAFETY: `fd` is an open descriptor owned by the caller; LOCK_UN never waits.
    #[expect(
        clippy::disallowed_methods,
        reason = "the release: LOCK_UN before the close"
    )]
    unsafe {
        libc::flock(fd, libc::LOCK_UN)
    };
    // SAFETY: closes the descriptor once.
    unsafe { libc::close(fd) };
}

fn dead_marker(dir: &Path, pid: u32, owner: MarkerOwner) -> std::path::PathBuf {
    let path = dir.join(markers::file_name(pid, 7, owner));
    std::fs::write(&path, b"").unwrap();
    path
}

fn sweep_as_before(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("crash-")
            && name.ends_with(".log")
            && entry.metadata().is_ok_and(|m| m.len() == 0)
        {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }
}

#[test]
fn names_round_trip_and_nothing_else_parses() {
    let name = markers::file_name(4242, 1_790_309_523_499_676_000, MarkerOwner::App);
    assert_eq!(name, "crash-marker-4242-1790309523499676000-app.log");
    assert_eq!(
        markers::parse(&name),
        Some(markers::MarkerName {
            pid: 4242,
            nanos: 1_790_309_523_499_676_000,
            owner: MarkerOwner::App
        })
    );
    let other = markers::file_name(1, 2, MarkerOwner::Other);
    assert_eq!(markers::parse(&other).unwrap().owner, MarkerOwner::Other);
    let lab = markers::file_name(1, 2, MarkerOwner::Lab);
    assert_eq!(lab, "crash-marker-1-2-lab.log");
    assert_eq!(markers::parse(&lab).unwrap().owner, MarkerOwner::Lab);
    for stranger in [
        "crash-signal-4242-7.log",
        "crash-marker-4242-7-app.log.seen",
        ".crash-marker-4242-7-app.log.pending",
        "crash-marker-x-7-app.log",
        "crash-marker-4242--app.log",
        "crash-marker-4242-7-root.log",
        "crash-4242.log",
        "aterm.log",
    ] {
        assert_eq!(markers::parse(stranger), None, "{stranger}");
    }
    let path = Path::new("/l").join(&other);
    assert_eq!(
        markers::renamed_for(&path, MarkerOwner::App).unwrap(),
        Path::new("/l/crash-marker-1-2-app.log")
    );
}

#[test]
fn a_sweep_keeps_a_live_owners_marker_and_removes_a_dead_ones() {
    let dir = scratch("marker-live");
    let own = std::process::id();
    let (fd, live) =
        markers::create_locked(dir.path(), &markers::file_name(own, 1, MarkerOwner::Other))
            .expect("arm a locked marker");
    let dead = dead_marker(dir.path(), reaped_pid(), MarkerOwner::Other);
    assert_eq!(markers::probe(&live), Owner::Live);
    assert_eq!(markers::probe(&dead), Owner::Dead);

    markers::sweep(dir.path(), own.wrapping_add(1), MarkerOwner::Other);
    assert!(
        live.exists(),
        "a live owner's marker must survive the sweep"
    );
    assert!(!dead.exists(), "a dead owner's empty marker is swept");

    sweep_as_before(dir.path());
    assert!(
        !live.exists(),
        "control: the old sweep unlinks the live marker the new one keeps"
    );
    let_go(fd);
}

#[test]
fn a_launch_that_cannot_lock_still_gets_an_unlocked_marker_that_is_never_news() {
    let dir = scratch("marker-arm");
    let own = std::process::id();
    let armed = markers::arm(dir.path(), own + 11, 5, MarkerOwner::App).expect("armed");
    assert!(armed.locked);
    assert_eq!(
        armed.path,
        dir.path()
            .join(markers::file_name(own + 11, 5, MarkerOwner::App))
    );
    assert_eq!(markers::probe(&armed.path), Owner::Live);
    let_go(armed.fd);
    std::fs::remove_file(&armed.path).unwrap();

    let name = markers::file_name(own + 12, 6, MarkerOwner::App);
    std::fs::create_dir(dir.path().join(format!(".{name}.pending"))).unwrap();
    assert!(
        markers::create_locked(dir.path(), &name).is_none(),
        "control: the locked arming alone leaves this launch without a marker"
    );
    let fallback = markers::arm(dir.path(), own + 12, 6, MarkerOwner::App).expect("fallback");
    assert!(!fallback.locked);
    assert_eq!(
        fallback.path,
        dir.path()
            .join(markers::file_name(own + 12, 6, MarkerOwner::Other)),
        "unlocked, so never named for the daily driver"
    );
    // SAFETY: `fallback.fd` is the open descriptor `arm` returned.
    let wrote = unsafe { libc::write(fallback.fd, b"x".as_ptr().cast(), 1) };
    assert_eq!(wrote, 1);
    assert_eq!(std::fs::read(&fallback.path).unwrap(), b"x");
    std::fs::write(&fallback.path, b"").unwrap();
    assert!(
        markers::dead_app_markers(dir.path(), own).is_empty(),
        "an empty unlocked marker is not a killed daily driver"
    );
    // SAFETY: closes the descriptor `arm` returned once.
    unsafe { libc::close(fallback.fd) };
}

#[test]
fn only_a_dead_app_marker_of_another_process_is_evidence() {
    let dir = scratch("marker-news");
    let own = std::process::id();
    let gone = reaped_pid();
    let killed = dead_marker(dir.path(), gone, MarkerOwner::App);
    let test_start = dead_marker(dir.path(), gone.wrapping_add(1), MarkerOwner::Other);
    let previous_image = dead_marker(dir.path(), own, MarkerOwner::App);
    let (fd, live_app) = markers::create_locked(
        dir.path(),
        &markers::file_name(own + 7, 3, MarkerOwner::App),
    )
    .unwrap();

    assert_eq!(
        markers::dead_app_markers(dir.path(), own),
        vec![killed.clone()]
    );
    markers::sweep(dir.path(), own, MarkerOwner::Other);
    assert!(killed.exists(), "left for the report");
    assert!(!test_start.exists(), "a test start's corpse is not news");
    assert!(
        !previous_image.exists(),
        "this process's own previous image"
    );
    assert!(live_app.exists());
    std::fs::write(&killed, b"aterm: fatal signal 11").unwrap();
    markers::sweep(dir.path(), own, MarkerOwner::Other);
    assert!(killed.exists());
    assert!(markers::dead_app_markers(dir.path(), own).is_empty());
    let_go(fd);
}

#[test]
fn a_lab_marker_is_news_to_a_lab_desks_sweep_alone() {
    for scan in [MarkerOwner::Lab, MarkerOwner::App, MarkerOwner::Other] {
        let dir = scratch("marker-lab");
        let own = std::process::id();
        let gone = reaped_pid();
        let lab = dead_marker(dir.path(), gone, MarkerOwner::Lab);
        let test_start = dead_marker(dir.path(), gone.wrapping_add(1), MarkerOwner::Other);
        let previous_image = dead_marker(dir.path(), own, MarkerOwner::Lab);
        assert!(markers::dead_app_markers(dir.path(), own).is_empty());
        markers::sweep(dir.path(), own, scan);
        let news = scan == MarkerOwner::Lab;
        assert_eq!(
            lab.exists(),
            news,
            "a dead lab marker under a {scan:?} sweep"
        );
        assert!(!test_start.exists(), "a test start's corpse is never news");
        assert!(
            !previous_image.exists(),
            "this process's own previous image"
        );
        assert_eq!(
            markers::dead_markers_of(dir.path(), own, MarkerOwner::Lab),
            if news { vec![lab.clone()] } else { Vec::new() }
        );
        assert!(markers::dead_app_markers(dir.path(), own).is_empty());
    }
}

#[test]
fn the_sweep_answers_the_dead_starts_it_removed() {
    let dir = scratch("marker-swept");
    let own = std::process::id();
    let gone = reaped_pid();
    let test_start = dead_marker(dir.path(), gone, MarkerOwner::Other);
    let previous_image = dead_marker(dir.path(), own, MarkerOwner::Other);
    let record = dir.path().join(markers::file_name(
        gone.wrapping_add(1),
        7,
        MarkerOwner::Other,
    ));
    std::fs::write(&record, b"aterm: fatal signal 11").unwrap();
    let (fd, live) = markers::create_locked(
        dir.path(),
        &markers::file_name(own + 3, 3, MarkerOwner::Other),
    )
    .unwrap();

    let swept = markers::sweep(dir.path(), own, MarkerOwner::Other);
    assert_eq!(
        swept,
        vec![markers::MarkerName {
            pid: gone,
            nanos: 7,
            owner: MarkerOwner::Other
        }]
    );
    assert!(
        !test_start.exists(),
        "control: the evidence left the directory"
    );
    assert!(!previous_image.exists());
    assert!(record.exists() && live.exists());
    let_go(fd);
}

#[test]
fn legacy_markers_follow_their_pid_and_pending_ones_their_lock() {
    let dir = scratch("marker-legacy");
    let own = std::process::id();
    let alive = dir.path().join(format!("crash-signal-{own}-5.log"));
    let gone = dir
        .path()
        .join(format!("crash-signal-{}-5.log", reaped_pid()));
    let record = dir
        .path()
        .join(format!("crash-signal-{}-6.log", reaped_pid()));
    std::fs::write(&alive, b"").unwrap();
    std::fs::write(&gone, b"").unwrap();
    std::fs::write(&record, b"aterm: fatal signal 6").unwrap();
    let name = markers::file_name(reaped_pid(), 9, MarkerOwner::Other);
    let stale_pending = dir.path().join(format!(".{name}.pending"));
    std::fs::write(&stale_pending, b"").unwrap();

    markers::sweep(dir.path(), own.wrapping_add(1), MarkerOwner::Other);
    assert!(
        alive.exists(),
        "an older build still running keeps its marker"
    );
    assert!(!gone.exists());
    assert!(record.exists(), "a crash record is never swept");
    assert!(!stale_pending.exists(), "an arming that died mid-way");
}

#[test]
fn a_killed_owner_releases_its_lock() {
    use std::io::Read as _;
    let dir = scratch("marker-kill");
    let Ok(perl) = aterm_uds::spawnfd::command("/usr/bin/perl")
        .arg("-e")
        .arg(
            "use Fcntl qw(:flock); open(my $f, '>', $ARGV[0]) or die; \
             flock($f, LOCK_EX|LOCK_NB) or die; $|=1; print \"locked\\n\"; sleep 60",
        )
        .arg(dir.path().join(markers::file_name(1, 1, MarkerOwner::App)))
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
    else {
        aterm_spec::host::could_not_run("/usr/bin/perl is not available to hold the lock");
    };
    let mut child = perl;
    let mut ready = [0u8; 7];
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .expect("the child locked its marker");
    let path = dir.path().join(markers::file_name(1, 1, MarkerOwner::App));
    assert_eq!(markers::probe(&path), Owner::Live);
    assert!(markers::dead_app_markers(dir.path(), 2).is_empty());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(markers::probe(&path), Owner::Dead);
    assert_eq!(markers::dead_app_markers(dir.path(), 2), vec![path]);
}
