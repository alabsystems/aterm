// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A stale frontend name can execute different code; bootstrap custody must refuse it.

#[cfg(all(target_os = "macos", debug_assertions))]
mod native {
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::spawnfd::{Child, Command, command};
    use std::path::{Path, PathBuf};

    const WORLD: &str = "ATERM_BOOTSTRAP_CUSTODY_WORLD";
    const ROLE: &str = "ATERM_BOOTSTRAP_CUSTODY_ROLE";
    const CASE: &str = "ATERM_BOOTSTRAP_CUSTODY_CASE";

    fn line(world: &Path, name: &str, text: &str) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(world.join(name))
            .unwrap();
        writeln!(file, "{text}").unwrap();
    }

    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn finished(child: &mut Child) -> std::process::ExitStatus {
        until_within(MUST_HAPPEN, "owned bootstrap child exits", || {
            child.try_wait().unwrap()
        })
    }

    fn execute(mut cmd: Command, world: &Path) {
        cmd.env(ROLE, "payload");
        let mut child = Owned(cmd.spawn().unwrap());
        assert!(finished(&mut child.0).success());
        assert!(world.join("payload").is_file() || world.join("replacement-executed").is_file());
    }

    pub fn run() {
        let world = std::env::var_os(WORLD).map(PathBuf::from);
        if let Some(world) = &world {
            let case = std::env::var(CASE).unwrap();
            // In the lost-custody cases retention is blocked: a new inode is the
            // incompatible replacement. Healthy custody can use a legitimate copy,
            // whose distinct inode does not make it the original parent's image.
            if case != "healthy"
                && !aterm_uds::process::image_matches_file(
                    std::process::id() as i32,
                    &world.join("original-recorded"),
                )
                .expect("fixture loaded-image identity must be readable")
            {
                line(
                    world,
                    "replacement-executed",
                    "executed replacement frontend",
                );
                return;
            }
            let cache = if case == "healthy" {
                "images.noindex"
            } else {
                "blocked-cache.noindex"
            };
            aterm_uds::tether::image_cache_for_tests(world.join(cache)).unwrap();
        }
        if let Some(code) = aterm_uds::tether::entry() {
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            });
        }
        let exe = std::env::current_exe().unwrap();
        if let Some(world) = world {
            if std::env::var(ROLE).unwrap() == "payload" {
                line(&world, "payload", "executed owned payload");
                return;
            }
            let case = std::env::var(CASE).unwrap();
            line(&world, "original-executed", "executed original frontend");
            until_within(MUST_HAPPEN, "controller changes launch name", || {
                world.join("go").exists().then_some(())
            });
            if case != "healthy" {
                assert!(
                    aterm_uds::tether::lost(),
                    "blocked storage must not recover the old image"
                );
            }
            if case == "control" {
                // Negative control: the former fallback reopened current_exe without
                // proving that the file still named the running frontend.
                let mut cmd = command(&exe);
                cmd.arg0(aterm_uds::tether::NAME)
                    .arg(aterm_uds::tether::VERB)
                    .arg(std::process::id().to_string())
                    .arg(&exe)
                    .arg(&exe)
                    .process_group(0);
                execute(cmd, &world);
                line(&world, "result", "unowned control executed");
                return;
            }
            let env = aterm_verify::EnvSnapshot::capture();
            match aterm_verify::native_verifier::command(&exe, &world, &env) {
                Ok(cmd) => {
                    execute(cmd, &world);
                    line(&world, "result", "accepted");
                }
                Err(_) => line(&world, "result", "refused"),
            }
            return;
        }
        for case in ["healthy", "control", "lost"] {
            // A failure preserves every copied executable, even a killed child that
            // never reached its receipt and may still be assessed by macOS.
            let world = aterm_tempfile::Builder::new()
                .prefix("bootstrap-custody-")
                .suffix(".noindex")
                .tempdir()
                .unwrap()
                .keep();
            let image = world.join(exe.file_name().unwrap());
            std::fs::copy(&exe, &image).unwrap();
            std::fs::hard_link(&image, world.join("original-recorded")).unwrap();
            if case != "healthy" {
                std::fs::write(world.join("blocked-cache.noindex"), b"not a directory").unwrap();
            }
            let mut parent = Owned(
                command(&image)
                    .env(WORLD, &world)
                    .env(ROLE, "parent")
                    .env(CASE, case)
                    .process_group(0)
                    .spawn()
                    .unwrap(),
            );
            let receipt = until_within(MUST_HAPPEN, "original frontend executes", || {
                assert!(
                    parent.0.try_wait().unwrap().is_none(),
                    "frontend ended before startup receipt"
                );
                let text = std::fs::read_to_string(world.join("original-executed")).ok()?;
                text.ends_with('\n').then_some(text)
            });
            assert_eq!(receipt, "executed original frontend\n");
            if case != "healthy" {
                let next = world.join("next");
                std::fs::copy(&exe, &next).unwrap();
                // The old copied image already wrote its execution receipt. The new
                // one is published before it is offered to any child.
                std::fs::rename(next, &image).unwrap();
            }
            std::fs::write(world.join("go"), b"go").unwrap();
            assert!(finished(&mut parent.0).success());
            let result = std::fs::read_to_string(world.join("result")).unwrap();
            match case {
                "healthy" => {
                    assert_eq!(result, "accepted\n");
                    assert_eq!(
                        std::fs::read_to_string(world.join("payload")).unwrap(),
                        "executed owned payload\n"
                    );
                    assert!(!world.join("replacement-executed").exists());
                }
                "control" => {
                    assert_eq!(result, "unowned control executed\n");
                    assert_eq!(
                        std::fs::read_to_string(world.join("replacement-executed")).unwrap(),
                        "executed replacement frontend\n"
                    );
                }
                "lost" => {
                    assert_eq!(result, "refused\n");
                    assert!(!world.join("replacement-executed").exists());
                    assert!(!world.join("payload").exists());
                }
                _ => unreachable!(),
            }
            // Successful children proved execution; the refused replacement was never
            // offered to an exec. Keep any unproved cached helper a future policy adds.
            if !world.join("images.noindex").exists() {
                std::fs::remove_dir_all(&world).unwrap();
            }
            println!("bootstrap custody case {case}: passed");
        }
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn main() {
    native::run();
}

#[cfg(not(all(target_os = "macos", debug_assertions)))]
fn main() {
    println!("bootstrap custody: native macOS debug qualification not run on this target");
}
