// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native callback custody and host dispatch, with no credentials or network.

#[cfg(all(any(target_os = "macos", target_os = "linux"), debug_assertions))]
mod native {
    use agh::git::{HelperCommand, HelperHost};
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::spawnfd::command;
    use std::path::{Path, PathBuf};

    const WORLD: &str = "AGH_HELPER_IMAGE_WORLD";
    const HOST: &str = "AGH_HELPER_IMAGE_HOST";
    const MODE: &str = "AGH_HELPER_IMAGE_MODE";
    const RECEIPT: &[u8] = b"recorded credential helper executed\n";

    struct Owned(aterm_uds::spawnfd::Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn host() -> (HelperHost, Vec<String>) {
        match std::env::var(HOST).as_deref().unwrap_or("direct") {
            "direct" => (HelperHost::Direct, vec![]),
            "subcommand" => (HelperHost::Subcommand, vec!["agh".into()]),
            "verifier" => (
                HelperHost::Verifier,
                vec!["--remote-agh".into(), "credential".into()],
            ),
            _ => panic!("unknown fixture host"),
        }
    }

    fn read(world: &Path, name: &str) -> String {
        until_within(MUST_HAPPEN, name, || {
            let s = std::fs::read_to_string(world.join(name)).ok()?;
            s.ends_with('\n').then_some(s)
        })
    }

    fn write(world: &Path, name: &str, value: &str) {
        use std::io::Write as _;
        let mut file = std::fs::File::create(world.join(name)).unwrap();
        writeln!(file, "{value}").unwrap();
    }

    fn callback(helper: &HelperCommand) {
        // The same shell grammar Git uses for a leading-! credential helper.
        let text = format!(
            "{} get",
            helper.config_value().unwrap().strip_prefix('!').unwrap()
        );
        let mut child = command("/bin/sh");
        child.args(["-c", &text]);
        let output = aterm_uds::deadline::output_within(&mut child, MUST_HAPPEN).unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, RECEIPT);
    }

    pub fn run() {
        let args: Vec<String> = std::env::args_os()
            .skip(1)
            .map(|word| word.into_string().expect("fixture arguments are UTF-8"))
            .collect();
        if args == ["unregistered"] {
            assert!(HelperCommand::current().is_err());
            println!("unregistered dispatch refused");
            return;
        }
        let (host, prefix) = host();
        host.record().unwrap();
        if args == ["unrecorded"] {
            assert!(HelperCommand::current().is_err());
            assert!(HelperHost::Subcommand.record().is_err());
            println!("unrecorded image and conflicting dispatch refused");
            return;
        }
        let world = std::env::var_os(WORLD).map(PathBuf::from);
        if let Some(world) = &world {
            use std::os::unix::ffi::OsStringExt as _;
            let cache = if std::env::var(MODE).as_deref() == Ok("non-utf8") {
                world.join(std::ffi::OsString::from_vec(b"cache-\xff.noindex".to_vec()))
            } else {
                world.join("cache.noindex")
            };
            aterm_uds::tether::image_cache_for_tests(cache).unwrap();
        }
        if let Some(code) = aterm_uds::tether::entry() {
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            });
        }
        let expected: Vec<_> = prefix
            .iter()
            .map(String::as_str)
            .chain(["auth", "git-credential", "get"])
            .collect();
        if args == expected {
            print!("{}", std::str::from_utf8(RECEIPT).unwrap());
            return;
        }
        if args == ["parent"] {
            let world = world.unwrap();
            write(&world, "ready", "original image executed");
            read(&world, "go");
            let helper = HelperCommand::current();
            if std::env::var(MODE).unwrap() == "blocked" {
                assert!(helper.is_err(), "unavailable custody must refuse");
                write(&world, "done", "unavailable custody refused");
                return;
            }
            let helper = helper.unwrap();
            assert_eq!(helper.args, prefix);
            if std::env::var(MODE).as_deref() == Ok("non-utf8") {
                assert!(aterm_uds::tether::prepared_executable().unwrap().is_some());
                assert!(helper.exe.to_str().is_none());
                assert!(helper.config_value().is_err());
                write(&world, "done", "unrepresentable configuration refused");
                return;
            }
            assert_ne!(helper.exe, Path::new(read(&world, "launch").trim_end()));
            if std::env::var(MODE).unwrap() == "after-exit" {
                write(&world, "helper", helper.exe.to_str().unwrap());
            } else {
                callback(&helper);
            }
            write(&world, "done", "retained callback qualified");
            return;
        }

        let original = std::env::current_exe().unwrap();
        for role in ["unregistered", "unrecorded"] {
            let mut child = command(&original);
            child.arg(role).env_remove(HOST);
            assert!(
                aterm_uds::deadline::output_within(&mut child, MUST_HAPPEN)
                    .unwrap()
                    .status
                    .success()
            );
        }
        for host in ["direct", "subcommand", "verifier"] {
            for mode in ["replace", "unlink", "after-exit", "blocked"]
                .into_iter()
                .chain(cfg!(target_os = "linux").then_some("non-utf8"))
            {
                // Keep every unproved copied executable if this case fails.
                let world = aterm_tempfile::Builder::new()
                    .prefix("agh-helper-image-")
                    .suffix(".noindex")
                    .tempdir()
                    .unwrap()
                    .keep();
                let slot = world.join(original.file_name().unwrap());
                std::fs::copy(&original, &slot).unwrap();
                write(&world, "launch", slot.to_str().unwrap());
                if mode == "blocked" {
                    std::fs::write(world.join("cache.noindex"), b"unavailable").unwrap();
                }
                let mut launch = command(&slot);
                launch
                    .arg("parent")
                    .env(WORLD, &world)
                    .env(HOST, host)
                    .env(MODE, mode);
                let mut parent = Owned(launch.spawn().unwrap());
                read(&world, "ready");
                if matches!(mode, "replace" | "blocked") {
                    let replacement = world.join("replacement");
                    std::fs::copy("/bin/echo", &replacement).unwrap();
                    #[cfg(target_os = "macos")]
                    {
                        let mut sign = command("/usr/bin/codesign");
                        sign.args(["--force", "--sign", "-"]).arg(&replacement);
                        assert!(
                            aterm_uds::deadline::output_within(&mut sign, MUST_HAPPEN)
                                .unwrap()
                                .status
                                .success()
                        );
                    }
                    std::fs::rename(&replacement, &slot).unwrap();
                    // An explicit unsafe-path control must actually execute replacement code.
                    let mut control = command(std::fs::canonicalize(&slot).unwrap());
                    control.arg("replacement code executed");
                    let output =
                        aterm_uds::deadline::output_within(&mut control, MUST_HAPPEN).unwrap();
                    assert!(output.status.success());
                    assert_eq!(output.stdout, b"replacement code executed\n");
                } else {
                    std::fs::remove_file(&slot).unwrap();
                }
                write(&world, "go", "replacement settled");
                read(&world, "done");
                let status = until_within(MUST_HAPPEN, "parent exited", || {
                    parent.0.try_wait().unwrap()
                });
                assert!(status.success());
                if mode == "after-exit" {
                    let (_, args) = match host {
                        "direct" => (HelperHost::Direct, vec![]),
                        "subcommand" => (HelperHost::Subcommand, vec!["agh".into()]),
                        _ => (
                            HelperHost::Verifier,
                            vec!["--remote-agh".into(), "credential".into()],
                        ),
                    };
                    // The callback starts only after the original process was reaped.
                    let helper = HelperCommand {
                        exe: read(&world, "helper").trim_end().into(),
                        args,
                    };
                    let text = format!(
                        "{} get",
                        helper.config_value().unwrap().strip_prefix('!').unwrap()
                    );
                    let mut child = command("/bin/sh");
                    child
                        .args(["-c", &text])
                        .env(WORLD, &world)
                        .env(HOST, host)
                        .env(MODE, mode);
                    let output =
                        aterm_uds::deadline::output_within(&mut child, MUST_HAPPEN).unwrap();
                    assert!(output.status.success(), "{output:?}");
                    assert_eq!(output.stdout, RECEIPT);
                }
                std::fs::remove_dir_all(world).unwrap();
                println!("credential helper case {host}/{mode}: passed");
            }
        }
        println!("credential helper image custody: all native cases passed");
    }
}

fn main() {
    #[cfg(all(any(target_os = "macos", target_os = "linux"), debug_assertions))]
    native::run();
}
