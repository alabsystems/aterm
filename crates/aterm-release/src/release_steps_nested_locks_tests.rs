// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn git_records_are_complete_relative_and_bounded() {
    assert_eq!(paths(b"").unwrap(), Vec::<String>::new());
    assert_eq!(
        paths(b"nested/Cargo.lock\0").unwrap(),
        ["nested/Cargo.lock"]
    );
    for refused in [
        &b"nested/Cargo.lock"[..],
        &b"\0"[..],
        &b"a\0\0"[..],
        &b"a\0a\0"[..],
        &b"../outside/Cargo.lock\0"[..],
        &b"/outside/Cargo.lock\0"[..],
        &b"nested/../Cargo.lock\0"[..],
        &b"nested//Cargo.lock\0"[..],
        &b"nested/\xff/Cargo.lock\0"[..],
    ] {
        assert!(paths(refused).is_err(), "{refused:?}");
    }
    assert!(paths(&vec![b'x'; PATH_BYTES + 1]).is_err());
    let too_many = (0..=PATHS)
        .flat_map(|index| format!("nested{index}/Cargo.lock\0").into_bytes())
        .collect::<Vec<_>>();
    assert!(paths(&too_many).is_err());
}

#[cfg(unix)]
mod filesystem {
    use super::*;
    use std::time::{Duration, Instant};

    const BEFORE: &str = "[[package]]\nname = \"aterm-local\"\nversion = \"0.147.0\"\n";
    const AFTER: &str = "[[package]]\nname = \"aterm-local\"\nversion = \"0.148.0\"\n";

    fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
        let program = std::path::PathBuf::from(aterm_uds::fixture_git::git().get_program());
        let mut command = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(&program));
        // Preserve the requested command's identity when the kept stand-in has a salted name.
        command.arg0("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        crate::credential_wait::ask_no_one(&mut command)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .arg("-C")
            .arg(repo)
            .args(["-c", "commit.gpgsign=false"])
            .args(args);
        let out = aterm_uds::deadline::output_until_complete_stdout(
            &mut command,
            Instant::now() + Duration::from_secs(60),
        )
        .expect("private local Git fixture must finish");
        assert!(out.status.success(), "{args:?}: {:?}", out.stderr);
        out.stdout
    }

    #[test]
    fn actual_git_paths_with_quotes_tabs_and_newlines_survive_the_bump_comparison() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let repo = scratch.path().canonicalize().unwrap();
        let nested = "odd space\t\"quoted\"\nline/Cargo.lock";
        let path = repo.join(nested);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, BEFORE).unwrap();
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["add", "--", nested]);
        git(&repo, &["commit", "--quiet", "-m", "inert fixture"]);
        let listed = paths(&git(&repo, &["ls-files", "-z", "--", "*/Cargo.lock"])).unwrap();
        assert_eq!(listed, [nested]);
        let followed = bump(&repo, Ok(listed), "0.147.0", "0.148.0").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), AFTER);
        let changed = paths(&git(&repo, &["diff", "--name-only", "-z", "HEAD"])).unwrap();
        assert_eq!(
            changed, followed,
            "the producer and comparison use the same names"
        );
        let unexpected = "unexpected\nfile.txt";
        std::fs::write(repo.join(unexpected), "keep this stray\n").unwrap();
        let untracked = paths(&git(
            &repo,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        ))
        .unwrap();
        assert!(
            untracked
                .iter()
                .any(|path| path == unexpected && !followed.contains(path))
        );
    }

    #[test]
    fn linked_parents_and_path_escapes_never_reach_outside_payloads() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().canonicalize().unwrap();
        let repo = home.join("repo");
        let outside = home.join("outside");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("Cargo.lock"), BEFORE).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join("linked")).unwrap();
        for path in [
            "linked/Cargo.lock".to_owned(),
            "../outside/Cargo.lock".to_owned(),
            outside.join("Cargo.lock").display().to_string(),
        ] {
            assert!(bump(&repo, Ok(vec![path]), "0.147.0", "0.148.0").is_err());
            assert_eq!(
                std::fs::read_to_string(outside.join("Cargo.lock")).unwrap(),
                BEFORE
            );
        }
        assert_eq!(std::fs::read_link(repo.join("linked")).unwrap(), outside);
    }

    #[test]
    fn oversized_locks_hold_the_batch_without_replacing_earlier_files() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let repo = scratch.path().canonicalize().unwrap();
        for name in ["good", "huge"] {
            std::fs::create_dir(repo.join(name)).unwrap();
        }
        let good = repo.join("good/Cargo.lock");
        std::fs::write(&good, BEFORE).unwrap();
        let huge = std::fs::File::create(repo.join("huge/Cargo.lock")).unwrap();
        huge.set_len(32 * 1024 * 1024 + 1).unwrap();
        assert!(
            bump(
                &repo,
                Ok(vec!["good/Cargo.lock".into(), "huge/Cargo.lock".into()]),
                "0.147.0",
                "0.148.0"
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(good).unwrap(), BEFORE);
        assert_eq!(huge.metadata().unwrap().len(), 32 * 1024 * 1024 + 1);
        assert!(
            std::fs::read_dir(repo.join("good"))
                .unwrap()
                .all(|entry| { entry.unwrap().file_name() == "Cargo.lock" })
        );
    }

    #[test]
    fn an_observed_replacement_refuses_before_any_target_is_published() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let repo = scratch.path().canonicalize().unwrap();
        for name in ["first", "later"] {
            std::fs::create_dir(repo.join(name)).unwrap();
            std::fs::write(repo.join(name).join("Cargo.lock"), BEFORE).unwrap();
        }
        let result = super::super::unix::bump_on(
            &repo,
            Ok(vec!["first/Cargo.lock".into(), "later/Cargo.lock".into()]),
            "0.147.0",
            "0.148.0",
            || {
                std::fs::rename(repo.join("first/Cargo.lock"), repo.join("saved-original"))
                    .unwrap();
                std::fs::write(repo.join("first/Cargo.lock"), "concurrent replacement\n").unwrap();
            },
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(repo.join("first/Cargo.lock")).unwrap(),
            "concurrent replacement\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("saved-original")).unwrap(),
            BEFORE
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("later/Cargo.lock")).unwrap(),
            BEFORE
        );
        for name in ["first", "later"] {
            assert!(
                std::fs::read_dir(repo.join(name))
                    .unwrap()
                    .all(|entry| { entry.unwrap().file_name() == "Cargo.lock" })
            );
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_writerless_fifo_is_refused_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt as _;
        #[cfg(target_os = "macos")]
        type PipeMode = u16;
        #[cfg(target_os = "linux")]
        type PipeMode = u32;
        unsafe extern "C" {
            fn mkfifo(path: *const std::ffi::c_char, mode: PipeMode) -> i32;
        }
        let scratch = aterm_tempfile::tempdir().unwrap();
        let repo = scratch.path().canonicalize().unwrap();
        std::fs::create_dir(repo.join("pipe")).unwrap();
        let path =
            std::ffi::CString::new(repo.join("pipe/Cargo.lock").as_os_str().as_bytes()).unwrap();
        // SAFETY: the owned NUL-terminated path outlives this call; mode_t is u16 on macOS
        // and u32 on Linux, the only platforms compiling this fixture.
        assert_eq!(unsafe { mkfifo(path.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _ = send.send(bump(
                &repo,
                Ok(vec!["pipe/Cargo.lock".into()]),
                "0.147.0",
                "0.148.0",
            ));
        });
        let refused = receive
            .recv_timeout(Duration::from_secs(60))
            .expect("the owned FIFO refusal must finish; this is a hang detector");
        worker.join().unwrap();
        assert!(refused.is_err());
    }
}
