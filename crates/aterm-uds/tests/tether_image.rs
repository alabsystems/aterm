// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native macOS image replacement and parent-death qualification in fresh single-threaded roles.

#[cfg(all(target_os = "macos", debug_assertions))]
mod native {
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::process::{Identity, read_identity};
    use aterm_uds::spawnfd::{Child, Tether as _, command};
    use std::path::{Path, PathBuf};

    const WORLD: &str = "ATERM_TETHER_IMAGE_TEST_WORLD";
    const DEFAULT_CACHE: &str = "ATERM_TETHER_IMAGE_DEFAULT_CACHE";

    fn line(world: &Path, name: &str, text: &str) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(world.join(name))
            .unwrap();
        writeln!(file, "{text}").unwrap();
    }

    fn read(world: &Path, name: &str) -> String {
        until_within(MUST_HAPPEN, name, || {
            let text = std::fs::read_to_string(world.join(name)).ok()?;
            text.ends_with('\n').then_some(text)
        })
    }

    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct EndKnown(Option<Identity>);
    impl Drop for EndKnown {
        fn drop(&mut self) {
            if let Some(identity) = self.0
                && read_identity(identity.pid) == Some(identity)
            {
                // SAFETY: this is the unchanged identity of this fixture's owned worker.
                unsafe {
                    libc::kill(identity.pid, libc::SIGKILL);
                }
            }
        }
    }

    pub fn run() {
        let role = std::env::args().nth(1).unwrap_or_default();
        let world = std::env::var_os(WORLD).map(PathBuf::from);
        if let Some(world) = &world
            && std::env::var_os(DEFAULT_CACHE).is_none()
        {
            aterm_uds::tether::image_cache_for_tests(world.join("cache.noindex")).unwrap();
        }
        if let Some(code) = aterm_uds::tether::entry() {
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            });
        }
        if let Some(world) = world {
            match role.as_str() {
                "exec-status-control" => {
                    aterm_uds::spawnfd::exec_status_observation_control(&world);
                    return;
                }
                "probe" => {
                    line(&world, "probe", "executed retained image");
                    return;
                }
                "worker" => {
                    let exe = std::env::current_exe().unwrap();
                    assert_eq!(
                        aterm_uds::process::image_matches_file(std::process::id() as i32, &exe),
                        Some(true)
                    );
                    if std::env::var_os(DEFAULT_CACHE).is_some() {
                        assert_eq!(
                            aterm_uds::tether::executable().unwrap(),
                            Path::new(read(&world, "retained-path").trim_end()),
                            "the same loaded code reuses its immutable helper"
                        );
                    }
                    line(&world, "worker-proof", "executed retained worker");
                    loop {
                        std::thread::park();
                    }
                }
                "parent" | "recover" | "prepared" | "qualified" => {
                    let prepared = match role.as_str() {
                        "prepared" => aterm_uds::tether::executable().map(Path::to_path_buf),
                        "qualified" => aterm_uds::tether::prepared_executable()
                            .expect("qualify helper before update")
                            .map(Path::to_path_buf),
                        _ => None,
                    };
                    if matches!(role.as_str(), "prepared" | "qualified") {
                        assert!(prepared.is_some(), "prepare helper before update");
                    }
                    line(&world, "parent-proof", "executed original frontend");
                    until_within(MUST_HAPPEN, "launch name changed", || {
                        world.join("go").exists().then_some(())
                    });
                    if role == "recover" {
                        assert!(
                            aterm_uds::tether::lost(),
                            "blocked test storage cannot retain an image"
                        );
                        std::fs::remove_file(world.join("cache.noindex")).unwrap();
                    }
                    let retained = prepared.as_deref().unwrap_or_else(|| {
                        aterm_uds::tether::executable().expect("retained helper image")
                    });
                    let default = std::env::var_os(DEFAULT_CACHE).is_some();
                    if default {
                        assert!(!retained.starts_with(&world));
                        assert!(!retained.starts_with(std::env::temp_dir()));
                        line(&world, "retained-path", retained.to_str().unwrap());
                        line(
                            &world,
                            "parent-scratch",
                            aterm_uds::test_binary::scratch_root()
                                .unwrap()
                                .to_str()
                                .unwrap(),
                        );
                    } else {
                        assert!(retained.starts_with(world.join("cache.noindex")));
                    }
                    assert!(!aterm_uds::tether::lost());
                    if !default {
                        aterm_uds::tether::image_cleanup_for_tests().unwrap();
                    }
                    assert!(retained.is_file(), "cleanup must retain an unproved image");
                    let mut probe = Owned(
                        command(retained)
                            .arg("probe")
                            .env(WORLD, &world)
                            .spawn()
                            .unwrap(),
                    );
                    assert!(
                        until_within(MUST_HAPPEN, "image probe exits", || probe
                            .0
                            .try_wait()
                            .unwrap())
                        .success()
                    );
                    assert_eq!(read(&world, "probe"), "executed retained image\n");
                    let mut worker = command(retained);
                    worker
                        .arg("worker")
                        .env(WORLD, &world)
                        .process_group(0)
                        .tether();
                    let mut worker = Owned(worker.spawn().unwrap());
                    line(&world, "worker-pid", &worker.0.id().to_string());
                    if default {
                        until_within(MUST_HAPPEN, "controller allows normal exit", || {
                            world.join("exit").exists().then_some(())
                        });
                        // Run test-root atexit cleanup while the retained worker remains alive.
                        std::process::exit(0);
                    }
                    worker.0.wait().unwrap();
                    panic!("worker ended while its original parent lived");
                }
                _ => panic!("unknown fixture role"),
            }
        }
        let exe = std::env::current_exe().unwrap();
        let filename = exe.file_name().unwrap();
        let mut bystander = Owned(command("/bin/sleep").arg("600").spawn().unwrap());
        for case in [
            "standalone-prepared",
            "qualified-prepared",
            "default-prepared",
            "replace",
            "unlink",
            "symlink",
            "recover",
            "bundle",
            "bundle-symlink",
            "bundle-prepared",
        ] {
            // A failure keeps every copied image: a killed, unproved child may still be assessed.
            let world = aterm_tempfile::Builder::new()
                .suffix(".noindex")
                .tempdir()
                .unwrap()
                .keep();
            let real = world.join("real");
            let alias = world.join("alias");
            std::fs::create_dir(&real).unwrap();
            std::fs::create_dir(&alias).unwrap();
            if case == "recover" {
                std::fs::write(world.join("cache.noindex"), b"block retention").unwrap();
            }
            let bundle = real.join("fixture.app");
            let image = if case.starts_with("bundle") {
                let macos = bundle.join("Contents/MacOS");
                std::fs::create_dir_all(&macos).unwrap();
                std::fs::write(bundle.join("Contents/Info.plist"), format!(
                    "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>test.aterm.tether-image</string><key>CFBundleExecutable</key><string>{}</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>", filename.to_str().unwrap()
                )).unwrap();
                std::fs::create_dir(bundle.join("Contents/Resources")).unwrap();
                std::fs::write(
                    bundle.join("Contents/Resources/sealed-resource"),
                    b"original resource",
                )
                .unwrap();
                macos.join(filename)
            } else {
                real.join(filename)
            };
            std::fs::copy(&exe, &image).unwrap();
            if case.starts_with("bundle") {
                assert!(
                    command("/usr/bin/codesign")
                        .args(["--force", "--sign", "-", "--options", "runtime"])
                        .arg(&bundle)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let launch = if case == "symlink" || case == "bundle-symlink" {
                let link = alias.join(filename);
                std::os::unix::fs::symlink(&image, &link).unwrap();
                link
            } else {
                image.clone()
            };
            let mut parent = command(&launch);
            if case == "default-prepared" {
                parent.env(DEFAULT_CACHE, "1");
            }
            let mut parent = Owned(
                parent
                    .arg(if case == "qualified-prepared" {
                        "qualified"
                    } else if case.ends_with("prepared") {
                        "prepared"
                    } else if case == "recover" {
                        "recover"
                    } else {
                        "parent"
                    })
                    .env(WORLD, &world)
                    .process_group(0)
                    .spawn()
                    .unwrap(),
            );
            assert_eq!(read(&world, "parent-proof"), "executed original frontend\n");
            match case {
                "bundle" | "bundle-symlink" | "bundle-prepared" => {
                    std::fs::remove_dir_all(&bundle).unwrap()
                }
                "replace" => {
                    std::fs::rename(&image, real.join("displaced")).unwrap();
                    std::fs::write(&image, b"not the running image").unwrap();
                }
                "symlink" => {
                    let other = world.join("other");
                    std::fs::write(&other, b"not the running image").unwrap();
                    let link = alias.join("next");
                    std::os::unix::fs::symlink(other, &link).unwrap();
                    std::fs::rename(link, &launch).unwrap();
                }
                _ => std::fs::remove_file(&image).unwrap(),
            }
            std::fs::write(world.join("go"), b"continue").unwrap();
            let proof = until_within(MUST_HAPPEN, "retained worker executes", || {
                assert!(
                    parent.0.try_wait().unwrap().is_none(),
                    "frontend ended before its worker proved execution"
                );
                let text = std::fs::read_to_string(world.join("worker-proof")).ok()?;
                text.ends_with('\n').then_some(text)
            });
            assert_eq!(proof, "executed retained worker\n");
            let worker: i32 = read(&world, "worker-pid").trim().parse().unwrap();
            let mut cleanup = EndKnown(Some(read_identity(worker).unwrap()));
            assert!(parent.0.try_wait().unwrap().is_none());
            if case == "default-prepared" {
                std::fs::write(world.join("exit"), b"exit normally").unwrap();
                assert!(
                    until_within(MUST_HAPPEN, "frontend exits normally", || parent
                        .0
                        .try_wait()
                        .unwrap())
                    .success()
                );
                let path = PathBuf::from(read(&world, "retained-path").trim_end());
                assert!(
                    path.is_file(),
                    "normal test exit must preserve helper custody"
                );
                assert!(
                    !Path::new(read(&world, "parent-scratch").trim_end()).exists(),
                    "the native test's normal-exit cleanup must actually run"
                );
            } else {
                parent.0.kill().unwrap();
                parent.0.wait().unwrap();
            }
            until_within(
                MUST_HAPPEN,
                "tied worker ended after original parent",
                || read_identity(worker).is_none().then_some(()),
            );
            cleanup.0 = None;
            assert!(bystander.0.try_wait().unwrap().is_none());
            // Original frontend, retained probe and retained worker each proved actual execution.
            std::fs::remove_dir_all(&world).unwrap();
            println!("retained image case {case}: worker ended, bystander survived");
        }
        bystander.0.kill().unwrap();
        bystander.0.wait().unwrap();
        println!("retained image custody: all native cases passed");
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn main() {
    native::run();
}

#[cfg(not(all(target_os = "macos", debug_assertions)))]
fn main() {
    println!("retained image custody: native macOS debug qualification not run on this target");
}
