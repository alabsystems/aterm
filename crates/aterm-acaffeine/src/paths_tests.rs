// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_file_lives_under_its_root() {
    let paths = Paths {
        state_dir: PathBuf::from("/s"),
        home: PathBuf::from("/h"),
        logs_dir: PathBuf::from("/l"),
        local_volume: |_| true,
    };
    assert_eq!(paths.state_file(), PathBuf::from("/s/acaffeine.state"));
    assert_eq!(paths.lock_file(), PathBuf::from("/s/acaffeine.lock"));
    assert_eq!(paths.bridge_file(), PathBuf::from("/s/acaffeine.bridge"));
    assert_eq!(paths.lid_file(), PathBuf::from("/s/acaffeine.lid"));
    assert_eq!(paths.code_dir(), PathBuf::from("/s/acaffeine-code.noindex"));
    assert_eq!(
        paths.session_plist("x.y"),
        PathBuf::from("/s/acaffeine/x.y.plist")
    );
    assert_eq!(
        paths.startup_plist("x.y"),
        PathBuf::from("/h/Library/LaunchAgents/x.y.plist")
    );
    assert_eq!(
        paths.startup_plist("x.y").parent(),
        Some(paths.launch_agents().as_path())
    );
    assert_eq!(paths.log_file(), PathBuf::from("/l/acaffeine.log"));
    assert_eq!(paths.trouble_file(), PathBuf::from("/s/acaffeine.trouble"));
    assert_eq!(
        paths.startup_off_file("x.y"),
        PathBuf::from("/s/acaffeine/x.y.startup-off")
    );
}

/// The login's paths read whether a folder is on this Mac's disk, as `statfs` says: a
/// temporary folder is, and on macOS a path `statfs` cannot read is not, so a home whose
/// LaunchAgents cannot be read is judged as a network one.
#[test]
fn the_logins_paths_read_the_volume() {
    let paths = Paths::from_env().expect("a home");
    let dir = crate::test_scratch_dir("paths");
    assert!((paths.local_volume)(dir.path()), "a temporary folder");
    #[cfg(target_os = "macos")]
    assert!(
        !(paths.local_volume)(&dir.path().join("missing")),
        "a path statfs cannot read"
    );
}

/// SYMLINKED `state_dir` IS REFUSED ACROSS ALL MUG FILES: `trouble`, `state`, `instance`,
/// `launchd`, `code`, and `sweep_*` refuse to read, create, overwrite, or unlink files or
/// subdirectories inside an outside directory aliased by a symlinked `state_dir`.
/// NEGATIVE CONTROL: a real `state_dir` owned by the current user reads, writes, locks, and
/// clears cleanly.
#[cfg(unix)]
#[test]
fn symlinked_state_dir_is_refused_across_mug_files() {
    use crate::model::{Clocks, Mode};

    let root = crate::test_scratch_dir("paths");
    let now = Clocks {
        since_boot: 100,
        wall: 1_700_000_000,
    };
    let label = "com.aterm.aterm.acaffeine.dev.0123abcd";
    let program = Path::new("/t/aterm");

    // Negative control: a real state directory works end to end.
    let real = Paths {
        state_dir: root.path().join("real-state"),
        home: root.path().join("home"),
        logs_dir: root.path().join("logs"),
        local_volume: |_| true,
    };
    crate::trouble::note_at(&real, "disk full", 10).expect("note real trouble");
    assert_eq!(crate::trouble::read(&real).map(|t| t.since), Some(10));
    crate::trouble::clear(&real);
    assert_eq!(crate::trouble::read(&real), None);
    crate::state::save_at(&real.state_file(), Mode::Off, now).expect("save real state");
    assert_eq!(
        crate::state::read(&real.state_file(), now).map(|s| s.mode),
        Some(Mode::Off)
    );
    let held = crate::instance::Instance::try_hold(&real.lock_file())
        .expect("hold real lock")
        .expect("free");
    assert_eq!(
        crate::instance::running_pid(&real.lock_file()),
        Some(std::process::id())
    );
    drop(held);

    // Adversarial case: `state_dir` is a symlink to `outside` holding pre-existing files.
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).expect("outside");
    std::fs::write(outside.join("acaffeine.trouble"), b"since=7\nwhy=keep\n")
        .expect("seed outside trouble");
    std::fs::write(outside.join("acaffeine.state"), b"mode=off\n").expect("seed outside state");
    std::fs::write(outside.join("acaffeine.lock"), b"99999\n").expect("seed outside lock");
    std::fs::write(outside.join("caffeine.bridge"), b"4242\n").expect("seed outside legacy bridge");
    let aliased_state = root.path().join("aliased-state");
    std::os::unix::fs::symlink(&outside, &aliased_state).expect("symlink state_dir");
    let aliased = Paths {
        state_dir: aliased_state,
        home: root.path().join("aliased-home"),
        logs_dir: root.path().join("aliased-logs"),
        local_volume: |_| true,
    };

    assert_eq!(crate::trouble::read(&aliased), None);
    assert!(crate::trouble::note_at(&aliased, "spoof", 99).is_err());
    crate::trouble::clear(&aliased);
    assert_eq!(
        std::fs::read(outside.join("acaffeine.trouble")).expect("outside trouble kept"),
        b"since=7\nwhy=keep\n"
    );

    assert_eq!(crate::state::read(&aliased.state_file(), now), None);
    assert!(crate::state::save_at(&aliased.state_file(), Mode::Forever, now).is_err());
    for (_, res) in crate::state::sweep_temps(&aliased) {
        assert!(res.is_err());
    }
    assert_eq!(
        std::fs::read(outside.join("acaffeine.state")).expect("outside state kept"),
        b"mode=off\n"
    );

    assert!(crate::instance::Instance::try_hold(&aliased.lock_file()).is_err());
    assert_eq!(crate::instance::running_holder(&aliased.lock_file()), None);
    assert_eq!(crate::instance::last_holder(&aliased.lock_file()), None);
    assert_eq!(
        std::fs::read(outside.join("acaffeine.lock")).expect("outside lock kept"),
        b"99999\n"
    );

    assert!(crate::launchd::choose_startup(&aliased, label, program, false).is_err());
    assert!(crate::launchd::choose_startup(&aliased, label, program, true).is_err());
    assert!(!crate::launchd::startup_turned_off(&aliased, label));
    assert!(!outside.join("acaffeine").exists());
    let plist_swept = crate::launchd::sweep_plist_temps(&aliased);
    assert_eq!(plist_swept.len(), 2);
    for (_, res) in plist_swept {
        assert!(res.is_err());
    }

    let app = root.path().join("Fake.app").join("Contents").join("MacOS");
    std::fs::create_dir_all(&app).expect("fake bundle");
    let exe = app.join("aterm");
    std::fs::write(&exe, b"bin").expect("fake exe");
    assert!(crate::code::prepare_copy(&exe, &aliased.code_dir(), label).is_err());
    assert!(crate::code::discard_copies(&aliased.code_dir()).is_err());
    assert!(!outside.join("acaffeine-code.noindex").exists());
    assert_eq!(
        std::fs::read(outside.join("caffeine.bridge")).expect("outside legacy bridge kept"),
        b"4242\n"
    );
}
