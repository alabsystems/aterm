// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A home whose git settings would answer a credential prompt (`core.askPass`) and name
/// the person (`user.name`).
#[cfg(unix)]
fn home_that_answers(tmp: &Path) -> std::path::PathBuf {
    let home = tmp.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".gitconfig"),
        b"[core]\n\taskPass = /bin/echo\n[user]\n\tname = marker\n",
    )
    .unwrap();
    home
}

/// `git credential fill` for a host no helper knows, under `home`: whether something
/// answered it. Nothing reaches a network; `/bin/echo` echoes git's prompt back.
#[cfg(unix)]
fn credential_answered(mut git: aterm_uds::spawnfd::Command, home: &Path) -> bool {
    use std::io::Write as _;
    let mut child = git
        .args(["credential", "fill"])
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"protocol=https\nhost=example.invalid\n\n")
        .unwrap();
    child.wait().unwrap().success()
}

/// Every git abrew builds asks nobody: a person's `core.askPass` does not answer it, and a
/// run that would have prompted fails. Control: the same git without the guards takes the
/// askpass program's answer, so the fixture can ask.
#[cfg(unix)]
#[test]
fn a_git_child_asks_nobody() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = home_that_answers(tmp.path());
    assert!(!credential_answered(git(&[]), &home));
    assert!(!credential_answered(command("git", &[]), &home));
    let mut open = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
    open.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_ASKPASS")
        .env_remove("GIT_CONFIG_GLOBAL");
    assert!(
        credential_answered(open, &home),
        "control: askpass answers where it is left on"
    );
    for (name, _) in GIT_ASKS_NOBODY {
        assert!(
            command("curl", &[]).get_envs().all(|(set, _)| set != *name),
            "control: {name} is git's alone"
        );
    }
}

/// A run that reaches a tap's remote reads no global configuration. Control: a local read
/// under the same home sees the person's settings.
#[cfg(unix)]
#[test]
fn a_remote_git_reads_no_global_configuration() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = home_that_answers(tmp.path());
    let name = |mut git: aterm_uds::spawnfd::Command| {
        let out = git
            .env("HOME", &home)
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let read = ["config", "--global", "--get", "user.name"];
    assert_eq!(name(git_remote(&read)), "");
    let mut local = git(&read);
    local.env_remove("GIT_CONFIG_GLOBAL");
    assert_eq!(
        name(local),
        "marker",
        "control: the fixture names the person"
    );
}

#[test]
fn download_setup_errors_name_the_path() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, b"file").unwrap();
    let dest = blocker.join("bottle.tar.gz");
    let error = curl_to_file("https://example.invalid/bottle.tar.gz", &dest, &[])
        .expect_err("setup must fail");
    assert!(
        error.contains(&blocker.display().to_string()),
        "error names the path: {error}"
    );
}

#[test]
fn sha256_of_known_bytes() {
    assert_eq!(
        sha256_bytes(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
#[cfg(unix)]
fn unchecked_cache_still_requires_a_file_and_downloads_keep_foreign_partials() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let dest = tmp.path().join("download");
    assert!(verify_sha256(&dest, "no_check").is_err());
    std::fs::write(&dest, b"old").unwrap();
    assert!(verify_sha256(&dest, "no_check").is_ok());
    assert!(verify_sha256(&dest, "").is_err());
    let partial = tmp.path().join("download.partial");
    std::fs::write(&partial, b"another operation").unwrap();
    let source = tmp.path().join("source");
    std::fs::write(&source, b"new").unwrap();
    curl_to_file(&format!("file://{}", source.display()), &dest, &[]).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    assert_eq!(std::fs::read(&partial).unwrap(), b"another operation");
    assert!(
        curl_to_file(
            &format!("file://{}", tmp.path().join("absent").display()),
            &dest,
            &[]
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    assert_eq!(std::fs::read(&partial).unwrap(), b"another operation");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 3);
}

#[cfg(unix)]
#[test]
fn mounted_tree_copy_skips_escaping_alias_and_keeps_in_root_links() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let src = tmp.path().join("mnt");
    let dst = tmp.path().join("out");
    std::fs::create_dir_all(src.join("Demo.app/Contents")).unwrap();
    std::fs::write(src.join("Demo.app/Contents/Info.plist"), b"plist").unwrap();
    std::fs::write(src.join("Demo.app/Contents/tool"), b"executable payload").unwrap();
    std::fs::set_permissions(
        src.join("Demo.app/Contents/Info.plist"),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    std::fs::set_permissions(
        src.join("Demo.app/Contents/tool"),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    std::fs::set_permissions(
        src.join("Demo.app/Contents"),
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    super::super::prefix::symlink_dir(Path::new("/Applications"), &src.join("Applications"))
        .unwrap();
    super::super::prefix::symlink_file(
        Path::new("Demo.app/Contents/Info.plist"),
        &src.join("alias"),
    )
    .unwrap();
    let copied = copy_mounted_tree(&src, &dst);
    std::fs::set_permissions(
        src.join("Demo.app/Contents"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    copied.unwrap();
    for (path, mode) in [
        ("", 0o755),
        ("Demo.app/Contents", 0o755),
        ("Demo.app/Contents/Info.plist", 0o644),
        ("Demo.app/Contents/tool", 0o755),
    ] {
        assert_eq!(
            std::fs::metadata(dst.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            mode,
            "{path} uses the staged tree's safe mode"
        );
    }
    assert_eq!(
        std::fs::read(dst.join("Demo.app/Contents/Info.plist")).unwrap(),
        b"plist"
    );
    assert!(
        std::fs::symlink_metadata(dst.join("Applications")).is_err(),
        "escaping Finder alias not copied"
    );
    assert_eq!(std::fs::read(dst.join("alias")).unwrap(), b"plist");
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "owned helper process; the native detach parent invokes this exact test"]
fn dmg_owned_detach_child() {
    let ready = std::env::var_os("ATERM_TEST_DMG_DETACH_READY").expect("owned ready path");
    std::fs::write(ready, b"the actual detach fixture ran\n").unwrap();
    println!("native detach completed");
}

#[cfg(target_os = "macos")]
#[test]
fn required_dmg_cleanup_waits_for_its_native_helper() {
    for age in [
        std::time::Duration::from_secs(31),
        std::time::Duration::from_secs(61),
    ] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let observed = aterm_tempfile::tempdir().unwrap();
        let ready = observed.path().join("native-ready");
        let root = aterm_tempfile::tempdir().unwrap();
        let retained = root.path().to_path_buf();
        let image = root.path().join("owned.dmg");
        std::fs::write(&image, b"owned image fixture").unwrap();
        let point = root.path().join("mount");
        std::fs::create_dir(&point).unwrap();
        let expected_point = point.clone();
        let calls = std::cell::Cell::new(0);
        let mut run = |cmd: &mut aterm_uds::spawnfd::Command, within| {
            calls.set(calls.get() + 1);
            assert_eq!(cmd.get_program(), "/usr/bin/hdiutil");
            let args: Vec<_> = cmd.get_args().map(std::ffi::OsStr::to_os_string).collect();
            assert_eq!(
                args,
                [
                    std::ffi::OsString::from("detach"),
                    std::ffi::OsString::from("-force"),
                    std::ffi::OsString::from("-quiet"),
                    expected_point.as_os_str().to_os_string()
                ]
            );
            let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
            child
                .args([
                    "abrew::fetch::tests::dmg_owned_detach_child",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                ])
                .env("ATERM_TEST_DMG_DETACH_READY", &ready);
            run_dmg_helper_started(&mut child, within, std::time::Instant::now() - age)
        };
        let guard = MountGuard {
            root: RetainedImage(Some(root)),
            image,
            point,
            run: &mut run,
            needs_detach: true,
            uncertain: None,
        };
        let result = guard.finish(Ok(()));
        assert_eq!(
            calls.get(),
            1,
            "an uncertain native helper is never retried"
        );
        if !expired {
            assert_eq!(
                std::fs::read(&ready).unwrap(),
                b"the actual detach fixture ran\n"
            );
            assert!(
                result.is_ok(),
                "a scheduled native detach helper is not unconfirmed: {result:?}"
            );
            assert!(
                !retained.exists(),
                "only confirmed cleanup releases the image"
            );
        } else {
            assert!(matches!(result, Err(DmgError::Uncertain(_))));
            assert!(
                !ready.exists(),
                "an expired hang detector starts no new helper"
            );
            assert!(retained.join("owned.dmg").is_file());
            assert!(retained.join("mount").is_dir());
            std::fs::remove_dir_all(&retained).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn dmg_helpers_retain_uncertain_inputs_until_cleanup_is_confirmed() {
    use std::os::unix::process::ExitStatusExt as _;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn answer(code: i32) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: b"unsupported image".to_vec(),
        }
    }

    let tmp = aterm_tempfile::tempdir().unwrap();
    let original = tmp.path().join("caller.dmg");
    for scenario in [
        "copied",
        "refused",
        "attach-unknown",
        "detach-unknown",
        "backstop-unknown",
        "backstop-refused",
        "detach-refused",
        "detach-busy",
        "busy-backstop-refused",
        "attach-panic",
    ] {
        std::fs::write(&original, b"owned image bytes").unwrap();
        let out = tmp.path().join(scenario);
        let scratch = aterm_tempfile::TempDir::new_in(tmp.path()).unwrap();
        let root = scratch.path().to_path_buf();
        let mut calls = Vec::new();
        let mut run = |cmd: &mut aterm_uds::spawnfd::Command, _within| {
            assert_eq!(cmd.get_program(), "/usr/bin/hdiutil");
            let args: Vec<_> = cmd.get_args().map(std::ffi::OsStr::to_os_string).collect();
            if args[0] == "attach" {
                let point = std::path::PathBuf::from(&args[5]);
                let image = std::path::PathBuf::from(&args[6]);
                assert_ne!(image, original, "the caller's path is never attached");
                assert_eq!(image.parent(), point.parent());
                assert_eq!(std::fs::read(&image).unwrap(), b"owned image bytes");
                assert_eq!(point.parent(), Some(root.as_path()));
                calls.push("attach");
                std::fs::remove_file(&original).unwrap();
                assert_eq!(std::fs::read(&image).unwrap(), b"owned image bytes");
                if scenario == "attach-panic" {
                    panic!("injected helper unwind");
                }
                if scenario == "attach-unknown" {
                    return Err(aterm_uds::deadline::RunError::TimedOut);
                }
                if matches!(
                    scenario,
                    "refused" | "backstop-unknown" | "backstop-refused"
                ) {
                    return Ok(answer(1));
                }
                std::fs::create_dir_all(point.join("Demo.app/Contents")).unwrap();
                std::fs::write(point.join("Demo.app/Contents/tool"), b"payload").unwrap();
                return Ok(answer(0));
            }
            assert_eq!(args[0], "detach");
            let forced = args.iter().any(|arg| arg == "-force");
            calls.push(if forced { "force" } else { "detach" });
            assert!(root.join("image.dmg").is_file());
            if (forced && scenario == "backstop-unknown")
                || (!forced && scenario == "detach-unknown")
            {
                return Err(aterm_uds::deadline::RunError::TimedOut);
            }
            if !forced && matches!(scenario, "detach-busy" | "busy-backstop-refused") {
                return Ok(answer(libc::EBUSY));
            }
            Ok(answer(i32::from(
                (forced && matches!(scenario, "backstop-refused" | "busy-backstop-refused"))
                    || (!forced && scenario == "detach-refused"),
            )))
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            extract_dmg_with(&original, &out, scratch, &mut run)
        }));
        if scenario == "attach-panic" {
            assert!(result.is_err());
            assert_eq!(calls, ["attach"], "no late detach after an unknown helper");
        } else {
            let result = result.expect("only the injected panic unwinds");
            match scenario {
                "copied" => {
                    result.unwrap();
                    assert_eq!(calls, ["attach", "detach"]);
                }
                "refused" => {
                    assert!(matches!(result, Err(DmgError::Attach(_))));
                    assert_eq!(calls, ["attach", "force"]);
                    assert!(!out.exists(), "an attach refusal copies nothing");
                }
                "detach-refused" => {
                    assert!(matches!(result, Err(DmgError::Detach)));
                    assert_eq!(calls, ["attach", "detach", "force"]);
                }
                "detach-busy" => {
                    result.expect("a busy volume the forced detach took down");
                    assert_eq!(calls, ["attach", "detach", "force"]);
                }
                "busy-backstop-refused" => {
                    assert!(
                        matches!(result, Err(DmgError::Uncertain(_))),
                        "a busy volume the forced detach left attached"
                    );
                    assert_eq!(calls, ["attach", "detach", "force"]);
                }
                _ => {
                    assert!(matches!(result, Err(DmgError::Uncertain(_))));
                    assert_eq!(
                        calls,
                        if scenario == "attach-unknown" {
                            vec!["attach"]
                        } else if scenario == "detach-unknown" {
                            vec!["attach", "detach"]
                        } else {
                            vec!["attach", "force"]
                        },
                        "unknown cleanup is never retried in Drop"
                    );
                }
            }
        }
        let retained = matches!(
            scenario,
            "attach-unknown"
                | "detach-unknown"
                | "backstop-unknown"
                | "backstop-refused"
                | "busy-backstop-refused"
                | "attach-panic"
        );
        assert_eq!(root.exists(), retained, "{scenario}");
        if retained {
            assert_eq!(
                std::fs::read(root.join("image.dmg")).unwrap(),
                b"owned image bytes"
            );
            assert!(root.join("mount").is_dir());
            std::fs::remove_dir_all(&root).unwrap();
        }
        if matches!(
            scenario,
            "copied" | "detach-refused" | "detach-busy" | "detach-unknown"
        ) {
            assert_eq!(
                std::fs::read(out.join("Demo.app/Contents/tool")).unwrap(),
                b"payload"
            );
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn dmg_mount_copy_detaches_and_stages_app() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let src = tmp.path().join("image");
    std::fs::create_dir_all(src.join("Demo.app/Contents/MacOS")).unwrap();
    std::fs::write(src.join("Demo.app/Contents/MacOS/demo"), b"bin").unwrap();
    let mut make = aterm_uds::spawnfd::command("/usr/bin/hdiutil");
    make.args(["makehybrid", "-quiet", "-hfs", "-o"])
        .arg(tmp.path().join("demo.dmg"))
        .arg(&src)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    let mut retained = RetainedImage(Some(tmp));
    let made = run_dmg_helper(&mut make, std::time::Duration::from_secs(60)).unwrap();
    let tmp = retained.0.take().unwrap();
    assert!(
        made.status.success(),
        "hdiutil makehybrid fixture: {made:?}"
    );
    let out = tmp.path().join("payload");
    extract_dmg(&tmp.path().join("demo.dmg"), &out).unwrap();
    assert_eq!(
        std::fs::read(out.join("Demo.app/Contents/MacOS/demo")).unwrap(),
        b"bin"
    );
    // A process holding the volume at the plain detach (a system scanner, on a loaded Mac)
    // makes hdiutil refuse it busy: the forced detach takes it down and the payload stands.
    let busy = tmp.path().join("busy");
    let root = aterm_tempfile::tempdir().unwrap();
    let point = root.path().join("mount");
    let (mut held, mut refused) = (None, None);
    let staged = extract_dmg_with(
        &tmp.path().join("demo.dmg"),
        &busy,
        root,
        |cmd: &mut aterm_uds::spawnfd::Command, within| {
            let args: Vec<_> = cmd.get_args().map(std::ffi::OsStr::to_os_string).collect();
            let plain = args[0] == "detach" && !args.iter().any(|arg| arg == "-force");
            if plain {
                let point = std::path::PathBuf::from(args.last().unwrap());
                let file = point.join("Demo.app/Contents/MacOS/demo");
                held = Some(std::fs::File::open(file).unwrap());
            }
            let out = run_dmg_helper(cmd, within);
            if plain {
                refused = out.as_ref().ok().and_then(|out| out.status.code());
            }
            out
        },
    );
    drop(held);
    let left = point.exists();
    if left {
        // Still attached: taken down before the verdict, so a failing run leaves no volume.
        let mut force = aterm_uds::spawnfd::command("/usr/bin/hdiutil");
        force
            .args(["detach", "-force", "-quiet"])
            .arg(&point)
            .stdin(aterm_uds::spawnfd::Stdio::null());
        let _ = run_dmg_helper(&mut force, DMG_DETACH_LIMIT);
        let _ = std::fs::remove_dir_all(point.parent().unwrap());
    }
    assert_eq!(refused, Some(libc::EBUSY), "the held volume refused busy");
    staged.unwrap();
    assert!(
        !left,
        "the forced detach took the busy volume down and its folder went"
    );
    assert_eq!(
        std::fs::read(busy.join("Demo.app/Contents/MacOS/demo")).unwrap(),
        b"bin"
    );
    std::fs::write(tmp.path().join("junk.dmg"), b"not an image").unwrap();
    let err = extract_dmg(&tmp.path().join("junk.dmg"), &tmp.path().join("nope")).unwrap_err();
    assert!(
        matches!(err, DmgError::Attach(_) | DmgError::Uncertain(_)),
        "{err}"
    );
    assert!(!tmp.path().join("nope").exists());
}

#[test]
fn archives_refuse_traversal_and_escaping_links_before_writing() {
    use crate::extract::fixtures::{ZipMember, gzip_bytes, tar_bytes, zip_bytes, zstd_bytes};
    type Member<'a> = (&'a str, u8, &'a str, &'a [u8], u32);
    fn archives(entries: &[Member<'_>]) -> Vec<(&'static str, Vec<u8>)> {
        let tar = tar_bytes(entries);
        let members: Vec<_> = entries
            .iter()
            .map(|(name, kind, target, bytes, mode)| ZipMember {
                name,
                mode: match kind {
                    b'2' => 0o120_777,
                    b'5' => 0o040_755,
                    _ => 0o100_000 | mode,
                },
                data: if *kind == b'2' {
                    target.as_bytes()
                } else {
                    bytes
                },
                deflate: false,
            })
            .collect();
        vec![
            ("tar", tar.clone()),
            ("gz", gzip_bytes(&tar)),
            ("zst", zstd_bytes(&tar)),
            ("zip", zip_bytes(&members, false)),
        ]
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    std::fs::write(&outside, b"owner data").unwrap();
    let absolute = outside.to_string_lossy();
    for (index, (name, kind, link)) in [
        ("../outside", b'0', ""),
        (absolute.as_ref(), b'0', ""),
        ("escape", b'2', "../outside"),
        ("escape", b'2', absolute.as_ref()),
    ]
    .into_iter()
    .enumerate()
    {
        let content: &[u8] = if kind == b'0' { b"replaced" } else { b"" };
        for (format, bytes) in archives(&[(name, kind, link, content, 0o755)]) {
            let archive = tmp.path().join(format!("bad-{index}.{format}"));
            std::fs::write(&archive, bytes).unwrap();
            assert!(
                untar(
                    &archive,
                    &tmp.path().join(format!("stage-{index}-{format}"))
                )
                .is_err(),
                "accepted {format}: {name} -> {link}"
            );
            assert_eq!(std::fs::read(&outside).unwrap(), b"owner data");
        }
    }
    for (format, bytes) in archives(&[("inside", b'0', "", b"safe", 0o644)]) {
        let archive = tmp.path().join(format!("good.{format}"));
        let stage = tmp.path().join(format!("good-{format}"));
        std::fs::write(&archive, bytes).unwrap();
        untar(&archive, &stage).unwrap();
        assert_eq!(std::fs::read(stage.join("inside")).unwrap(), b"safe");
        assert!(untar(&archive, &stage).is_err());
        assert_eq!(std::fs::read(stage.join("inside")).unwrap(), b"safe");
        #[cfg(unix)]
        {
            let external = tmp.path().join(format!("external-{format}"));
            std::fs::create_dir(&external).unwrap();
            let alias = tmp.path().join(format!("alias-{format}"));
            std::os::unix::fs::symlink(&external, &alias).unwrap();
            assert!(untar(&archive, &alias).is_err());
            assert!(std::fs::read_dir(&external).unwrap().next().is_none());
        }
    }
    for (format, bytes) in [
        ("xz", b"\xfd7zXZ\0".as_slice()),
        ("bz2", b"BZh91AY".as_slice()),
        ("empty", b"".as_slice()),
    ] {
        let archive = tmp.path().join(format);
        let stage = tmp.path().join(format!("unsupported-{format}"));
        std::fs::write(&archive, bytes).unwrap();
        assert!(untar(&archive, &stage).is_err());
        assert!(!stage.exists());
    }
}

#[test]
fn headers_and_urls_with_line_breaks_or_nuls_are_refused_before_spawning_curl() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let dest = tmp.path().join("sub/out.bin");
    for bad_header in [
        "Authorization: Bearer a\noutput = /tmp/pwn",
        "X-Test: a\r\nX-Injected: b",
        "X-Test: a\0b",
    ] {
        assert!(curl_get("https://example.invalid/x", &[bad_header]).is_err());
        assert!(curl_to_file("https://example.invalid/x", &dest, &[bad_header]).is_err());
        assert!(head_size("https://example.invalid/x", &[bad_header]).is_none());
        assert!(!dest.parent().unwrap().exists());
    }
    for bad_url in [
        "https://example.invalid/a\nb",
        "https://example.invalid/a\rb",
        "https://example.invalid/a\0b",
    ] {
        assert!(curl_get(bad_url, &[]).is_err());
        assert!(curl_to_file(bad_url, &dest, &[]).is_err());
        assert!(head_size(bad_url, &[]).is_none());
    }
}
