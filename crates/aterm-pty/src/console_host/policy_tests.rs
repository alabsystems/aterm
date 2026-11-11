// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A path under this host's filesystem root, so the same test reads the
/// same on Windows and off it.
fn rooted(parts: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" });
    for part in parts {
        path.push(part);
    }
    path
}

fn exe(origin: Origin) -> OpenConsoleExe {
    OpenConsoleExe {
        path: rooted(&["oc", "OpenConsole.exe"]),
        origin,
        version: Some("1.24.2607.10001".to_string()),
    }
}

#[test]
fn the_inbox_choice_consults_nothing_and_logs_nothing() {
    let mut host = HostState::new();
    let program = rooted(&["Windows", "System32", "cmd.exe"]);
    let none = host.host_for_spawn(
        &program,
        |_| panic!("the inbox host resolves nothing"),
        |_| panic!("the inbox host probes no file"),
    );
    assert_eq!(none, None);
    assert_eq!(host.take_unlogged(), None);
    let report = host.report(|_| panic!("still nothing"));
    assert_eq!(report.kind, ConsoleHostKind::Inbox);
    assert!(
        !report.fell_back,
        "the inbox host asked for is not a fallback"
    );
    host.set_choice(ConsoleHostChoice::from_config(Some("inbox")));
    assert_eq!(
        host.host_for_spawn(&program, |_| panic!("no"), |_| false),
        None
    );
    assert_eq!(
        host.report(|_| panic!("no")).line,
        "Windows console host (aterm.toml conpty = \"inbox\")"
    );
}

#[test]
fn the_default_runs_the_bundled_host_or_says_once_that_there_is_none() {
    let dir = rooted(&["Programs", "aterm"]);
    assert_eq!(
        bundled_source(Some(&dir), |p| p == dir.join("OpenConsole.exe")),
        Ok(dir.join("OpenConsole.exe"))
    );
    assert_eq!(
        bundled_source(Some(&dir), |_| false),
        Err(NOT_BUNDLED.to_string())
    );
    assert_eq!(bundled_source(None, |_| true), Err(NOT_BUNDLED.to_string()));

    let program = rooted(&["pwsh", "pwsh.exe"]);
    let mut host = HostState::new();
    host.set_choice(ConsoleHostChoice::from_config(None));
    assert_eq!(
        host.host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false),
        Some(exe(Origin::Bundled))
    );
    assert_eq!(
        host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
        Some(exe(Origin::Bundled))
    );
    assert_eq!(host.take_unlogged(), None);
    let found = host.report(|_| panic!("kept"));
    assert_eq!(found.kind, ConsoleHostKind::OpenConsole);
    assert!(!found.fell_back, "{found}");

    let mut bare = HostState::new();
    bare.set_choice(ConsoleHostChoice::Bundled);
    assert_eq!(
        bare.host_for_spawn(&program, |_| Err(NOT_BUNDLED.to_string()), |_| false),
        None
    );
    let note = bare.take_unlogged().expect("a missing host is noted");
    assert_eq!(note, format!("Windows console host; {NOT_BUNDLED}"));
    assert_eq!(
        bare.host_for_spawn(&program, |_| panic!("kept"), |_| false),
        None
    );
    assert_eq!(bare.take_unlogged(), None, "once");
    let report = bare.report(|_| panic!("kept"));
    assert_eq!(report.kind, ConsoleHostKind::Inbox);
    assert!(
        report.fell_back,
        "the default named a host it lacks: {report}"
    );
}

#[test]
fn an_opted_in_choice_resolves_once_and_logs_a_failure_once() {
    let program = rooted(&["pwsh", "pwsh.exe"]);
    let mut host = HostState::new();
    host.set_choice(ConsoleHostChoice::Auto);
    let got = host.host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false);
    assert_eq!(got, Some(exe(Origin::Bundled)));
    let again = host.host_for_spawn(&program, |_| panic!("kept"), |_| false);
    assert_eq!(again, Some(exe(Origin::Bundled)));
    assert_eq!(host.take_unlogged(), None);

    let path = rooted(&["missing", "OpenConsole.exe"]);
    host.set_choice(ConsoleHostChoice::Path(path.clone()));
    let none = host.host_for_spawn(&program, |_| Err("it is not a file".into()), |_| false);
    assert_eq!(none, None);
    let note = host
        .take_unlogged()
        .expect("the failure is handed to the log");
    assert!(
        note.starts_with("Windows console host; it is not a file"),
        "{note}"
    );
    assert!(note.contains(&path.display().to_string()), "{note}");
    assert_eq!(host.take_unlogged(), None, "once");
    assert_eq!(
        host.host_for_spawn(&program, |_| panic!("the failure is kept"), |_| false),
        None
    );
    assert_eq!(host.take_unlogged(), None, "a second tab logs nothing new");
    let report = host.report(|_| panic!("kept"));
    assert!(report.fell_back, "{report}");
}

#[test]
fn a_launch_failure_sticks_until_the_choice_changes() {
    let program = rooted(&["pwsh", "pwsh.exe"]);
    let mut host = HostState::new();
    host.set_choice(ConsoleHostChoice::Auto);
    let oc = host
        .host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false)
        .expect("resolved");
    host.note_launch_failure(&oc, "it exited with code 0x00000001 after 20 ms");
    let note = host.take_unlogged().expect("logged");
    assert_eq!(
        note,
        "Windows console host; OpenConsole did not start (it exited with code 0x00000001 \
         after 20 ms)"
    );
    assert_eq!(
        host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
        None
    );
    host.note_launch_failure(&exe(Origin::Configured), "late");
    assert_eq!(host.take_unlogged(), None);
    host.set_choice(ConsoleHostChoice::Path(rooted(&["oc", "OpenConsole.exe"])));
    assert_eq!(
        host.host_for_spawn(&program, |_| Ok(exe(Origin::Configured)), |_| false),
        Some(exe(Origin::Configured))
    );
}

#[test]
fn a_silent_host_costs_one_tab_until_it_is_silent_three_times_running() {
    let program = rooted(&["pwsh", "pwsh.exe"]);
    let mut host = HostState::new();
    host.set_choice(ConsoleHostChoice::Bundled);
    let oc = host
        .host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false)
        .expect("resolved");
    let why = "it wrote nothing in 250 ms";
    let silent = |host: &mut HostState| {
        host.note_silent_start(&oc, why);
        host.take_unlogged().expect("each fallback is logged")
    };
    for _ in 0..2 {
        for _ in 1..SILENT_STARTS {
            assert_eq!(
                silent(&mut host),
                "Windows console host for one tab; OpenConsole did not start (it wrote \
                 nothing in 250 ms) (the next tab tries it again)"
            );
            assert_eq!(
                host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
                Some(oc.clone()),
                "the next tab gets it again"
            );
        }
        host.note_started(&oc);
    }
    for _ in 1..SILENT_STARTS {
        silent(&mut host);
    }
    assert_eq!(
        silent(&mut host),
        "Windows console host; OpenConsole did not start (it wrote nothing in 250 ms)"
    );
    assert_eq!(
        host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
        None,
        "three in a row stick"
    );
    host.set_choice(ConsoleHostChoice::Auto);
    let oc = host
        .host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false)
        .expect("resolved afresh");
    host.note_silent_start(&oc, why);
    assert_eq!(
        host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
        Some(oc)
    );
}

#[test]
fn auto_gives_posix_runtime_programs_openconsole_only_when_guarded() {
    let bash = rooted(&["Git", "usr", "bin", "bash.exe"]);
    let dll = rooted(&["Git", "usr", "bin", "msys-2.0.dll"]);
    let is_file = |p: &Path| p == dll;
    let mut host = HostState::new();
    for choice in [ConsoleHostChoice::Bundled, ConsoleHostChoice::Auto] {
        host.set_guarded(false);
        host.set_choice(choice.clone());
        assert_eq!(
            host.host_for_spawn(&bash, |_| Ok(exe(Origin::Bundled)), is_file),
            None,
            "{choice:?}, no guard client: Git Bash keeps conhost"
        );
        assert!(host.report(|_| panic!("kept")).line.contains("Git Bash"));
        host.set_guarded(true);
        assert_eq!(
            host.host_for_spawn(&bash, |_| panic!("kept"), is_file),
            Some(exe(Origin::Bundled)),
            "{choice:?}, guarded: Git Bash takes OpenConsole too"
        );
        assert!(!host.report(|_| panic!("kept")).line.contains("Git Bash"));
    }
    host.set_guarded(false);
    host.set_choice(ConsoleHostChoice::Path(rooted(&["oc", "OpenConsole.exe"])));
    assert_eq!(
        host.host_for_spawn(&bash, |_| Ok(exe(Origin::Configured)), is_file),
        Some(exe(Origin::Configured))
    );
}

#[test]
fn the_host_arguments_are_the_five_stable_ones() {
    assert_eq!(
        host_arguments(120, 30, 0x2c4, 0x1a8),
        " --headless --width 120 --height 30 --signal 0x2c4 --server 0x1a8"
    );
}

#[test]
fn auto_prefers_the_bundled_host_then_windows_terminal() {
    let dir = rooted(&["aterm"]);
    let bundled = auto_source(Some(&dir), |_| true, || panic!("not consulted"));
    assert_eq!(
        bundled,
        Ok(AutoSource::Bundled(dir.join("OpenConsole.exe")))
    );
    let source = rooted(&["wt", "OpenConsole.exe"]);
    let wt = auto_source(
        Some(&dir),
        |_| false,
        || Some(("WT_1.24_x64__x".to_string(), source.clone())),
    );
    assert_eq!(
        wt,
        Ok(AutoSource::WindowsTerminal {
            package: "WT_1.24_x64__x".to_string(),
            source: source.clone(),
        })
    );
    let none = auto_source(None, |_| false, || None).expect_err("nothing to launch");
    assert!(none.contains("no OpenConsole.exe beside aterm"), "{none}");
    assert!(none.contains("no Windows Terminal"), "{none}");
}

#[test]
fn the_posix_runtimes_are_recognised_by_their_dll() {
    let files = [
        rooted(&["Git", "bin", "..", "usr", "bin", "msys-2.0.dll"]),
        rooted(&["Git", "usr", "bin", "msys-2.0.dll"]),
        rooted(&["msys64", "usr", "bin", "msys-2.0.dll"]),
        rooted(&["cygwin64", "bin", "cygwin1.dll"]),
    ];
    let is_file = |p: &Path| files.iter().any(|f| f == p);
    for runtime in [
        rooted(&["Git", "bin", "bash.exe"]),
        rooted(&["Git", "usr", "bin", "bash.exe"]),
        rooted(&["msys64", "usr", "bin", "zsh.exe"]),
        rooted(&["cygwin64", "bin", "bash.exe"]),
    ] {
        assert!(runs_a_posix_runtime(&runtime, is_file), "{runtime:?}");
    }
    for native in [
        rooted(&["PowerShell", "7", "pwsh.exe"]),
        rooted(&["Windows", "System32", "cmd.exe"]),
        rooted(&["tools", "bash.exe"]),
        PathBuf::from("bash.exe"),
    ] {
        assert!(!runs_a_posix_runtime(&native, is_file), "{native:?}");
    }
}

#[test]
fn a_configured_path_must_be_an_absolute_file() {
    let relative =
        configured(Path::new("OpenConsole.exe"), |_| true, |_| None).expect_err("relative");
    assert!(relative.contains("not an absolute path"), "{relative}");
    let missing = rooted(&["definitely", "not", "here", "OpenConsole.exe"]);
    let err = configured(&missing, |_| false, |_| None).expect_err("missing");
    assert!(err.contains("is not a file"), "{err}");
    let here = configured(&missing, |_| true, |_| Some("1.2.3.4".into())).expect("a file");
    assert_eq!(here.origin, Origin::Configured);
    assert_eq!(here.version.as_deref(), Some("1.2.3.4"));
}

#[test]
fn the_report_line_names_the_host_and_why() {
    let package = "Microsoft.WindowsTerminal_1.24.11911.0_x64__8wekyb3d8bbwe";
    let copied = OpenConsoleExe {
        path: rooted(&["s", "conpty", package, "OpenConsole.exe"]),
        origin: Origin::WindowsTerminal(package.to_string()),
        version: Some("1.24.2607.10001".to_string()),
    };
    let r = report(&ConsoleHostChoice::Auto, &Ok(copied.clone()));
    assert_eq!(r.kind, ConsoleHostKind::OpenConsole);
    assert!(!r.fell_back);
    assert_eq!(
        r.line,
        format!(
            "OpenConsole 1.24.2607.10001 (from Windows Terminal 1.24.11911.0, copied to {})",
            copied.path.display()
        )
    );
    let unguarded = report_guarded(&ConsoleHostChoice::Auto, &Ok(copied.clone()), false);
    assert_eq!(
        unguarded.line,
        format!(
            "OpenConsole 1.24.2607.10001 (from Windows Terminal 1.24.11911.0, copied to {}); \
             Git Bash and other MSYS2/Cygwin programs keep the Windows console host (no \
             aterm.exe beside aterm to guard their input after a resize)",
            copied.path.display()
        )
    );

    let inbox = report(&ConsoleHostChoice::Inbox, &Err(INBOX_REASON.to_string()));
    assert_eq!(inbox.kind, ConsoleHostKind::Inbox);
    assert!(!inbox.fell_back);
    assert_eq!(
        inbox.line,
        "Windows console host (aterm.toml conpty = \"inbox\")"
    );

    let bundled = OpenConsoleExe {
        path: rooted(&["aterm", "OpenConsole.exe"]),
        origin: Origin::Bundled,
        version: Some("1.25.2609.30003".to_string()),
    };
    let default = report(&ConsoleHostChoice::Bundled, &Ok(bundled.clone()));
    assert_eq!(default.kind, ConsoleHostKind::OpenConsole);
    assert_eq!(
        default.line,
        format!(
            "OpenConsole 1.25.2609.30003 (bundled beside aterm: {})",
            bundled.path.display()
        )
    );
    let none = report(&ConsoleHostChoice::Bundled, &Err(NOT_BUNDLED.to_string()));
    assert!(none.fell_back);
    assert_eq!(
        none.line,
        "Windows console host; no OpenConsole.exe beside aterm (reinstall aterm to restore \
         it; aterm.toml conpty = \"auto\" also tries Windows Terminal's)"
    );

    let path = rooted(&["oc", "OpenConsole.exe"]);
    let failed = report(
        &ConsoleHostChoice::Path(path.clone()),
        &Err(did_not_start("it exited with code 0xc0000142 after 20 ms")),
    );
    assert!(failed.fell_back);
    assert_eq!(
        failed.line,
        format!(
            "Windows console host; OpenConsole did not start (it exited with code 0xc0000142 \
             after 20 ms) — fix or remove aterm.toml conpty = \"{}\"",
            path.display()
        )
    );
}

#[test]
fn package_version_is_the_second_field_of_the_full_name() {
    assert_eq!(
        package_version("Microsoft.WindowsTerminal_1.24.11911.0_x64__8wekyb3d8bbwe"),
        "1.24.11911.0"
    );
    assert_eq!(package_version("odd"), "odd");
}

#[test]
fn copy_out_keeps_one_copy_and_prunes_other_packages() {
    let root = std::env::temp_dir().join(format!("aterm-conpty-copy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source.exe");
    std::fs::write(&source, b"MZ one").unwrap();
    let cache = root.join("conpty");
    let old = cache.join("Microsoft.WindowsTerminal_1.23.1.0_x64__8wekyb3d8bbwe");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("OpenConsole.exe"), b"MZ old").unwrap();
    std::fs::write(old.join("OpenConsole.exe.4242.partial"), b"MZ o").unwrap();
    let other = cache.join("Microsoft.WindowsTerminalPreview_1.25.1.0_x64__8wekyb3d8bbwe");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("OpenConsole.exe"), b"MZ preview").unwrap();
    std::fs::write(other.join("notes.txt"), b"not aterm's").unwrap();

    let pkg = "Microsoft.WindowsTerminal_1.24.11911.0_x64__8wekyb3d8bbwe";
    let dest = copy_out(&source, &cache, pkg).expect("copy");
    assert_eq!(dest, cache.join(pkg).join("OpenConsole.exe"));
    assert_eq!(std::fs::read(&dest).unwrap(), b"MZ one");
    assert!(
        !old.exists(),
        "an older package's copy and partial are pruned"
    );
    assert!(!other.join("OpenConsole.exe").exists(), "every other copy");
    assert_eq!(
        std::fs::read(other.join("notes.txt")).unwrap(),
        b"not aterm's",
        "a file copy_out never writes is never removed"
    );

    let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    std::fs::File::options()
        .write(true)
        .open(&dest)
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    copy_out(&source, &cache, pkg).expect("second copy");
    assert_eq!(
        std::fs::metadata(&dest).unwrap().modified().unwrap(),
        stamp,
        "an identical copy is not rewritten"
    );
    std::fs::write(&source, b"MZ two").unwrap();
    let pid_partial = cache
        .join(pkg)
        .join(format!("OpenConsole.exe.{}.partial", std::process::id()));
    std::fs::write(&pid_partial, b"in-flight").unwrap();
    copy_out(&source, &cache, pkg).expect("changed source tolerates existing pid partial");
    assert_eq!(std::fs::read(&dest).unwrap(), b"MZ two");
    assert_eq!(std::fs::read(&pid_partial).unwrap(), b"in-flight");
    std::fs::remove_file(&pid_partial).unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(cache.join(pkg))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "no partial file stays: {leftovers:?}");

    // A package that is not one folder name copies nothing anywhere and prunes nothing.
    for bad in [
        "",
        ".",
        "..",
        "../outside",
        "..\\outside",
        "sub/pkg",
        "sub\\pkg",
        "C:pkg",
        "/abs",
        "nul\0",
    ] {
        let err = copy_out(&source, &cache, bad).expect_err(bad);
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}: {err}");
    }
    assert!(
        !root.join("outside").exists(),
        "nothing was copied beside the cache"
    );
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        b"MZ two",
        "a refused package prunes nothing"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn the_prune_never_follows_a_link() {
    let root = std::env::temp_dir().join(format!("aterm-conpty-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (cache, outside) = (root.join("conpty"), root.join("outside"));
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("OpenConsole.exe"), b"MZ elsewhere").unwrap();
    std::os::unix::fs::symlink(&outside, cache.join("linked")).unwrap();
    let source = root.join("source.exe");
    std::fs::write(&source, b"MZ one").unwrap();
    assert!(copy_out(&source, &cache, "linked").is_err());
    assert_eq!(
        std::fs::read(outside.join("OpenConsole.exe")).unwrap(),
        b"MZ elsewhere"
    );
    copy_out(&source, &cache, "current").expect("copy");
    assert!(outside.join("OpenConsole.exe").exists());
    let current_exe = cache.join("current").join("OpenConsole.exe");
    std::fs::remove_file(&current_exe).unwrap();
    std::os::unix::fs::symlink(&source, &current_exe).unwrap();
    copy_out(&source, &cache, "current").expect("replaces symlinked dest with real file");
    assert!(
        std::fs::symlink_metadata(&current_exe)
            .unwrap()
            .file_type()
            .is_file()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_pruned_windows_terminal_copy_is_resolved_afresh() {
    let program = rooted(&["pwsh", "pwsh.exe"]);
    let copied = OpenConsoleExe {
        path: rooted(&["s", "conpty", "WT_1.23", "OpenConsole.exe"]),
        origin: Origin::WindowsTerminal("WT_1.23".to_string()),
        version: None,
    };
    let fresh = OpenConsoleExe {
        path: rooted(&["s", "conpty", "WT_1.24", "OpenConsole.exe"]),
        origin: Origin::WindowsTerminal("WT_1.24".to_string()),
        version: None,
    };
    let both = |p: &Path| p == copied.path || p == fresh.path;
    let mut host = HostState::new();
    host.set_choice(ConsoleHostChoice::Auto);
    let first = host.host_for_spawn(&program, |_| Ok(copied.clone()), both);
    assert_eq!(first, Some(copied.clone()));
    let kept = host.host_for_spawn(&program, |_| panic!("still there"), both);
    assert_eq!(kept, Some(copied.clone()));
    let pruned = |p: &Path| p == fresh.path;
    let again = host.host_for_spawn(&program, |_| Ok(fresh.clone()), pruned);
    assert_eq!(again, Some(fresh));
    assert_eq!(host.take_unlogged(), None, "nothing failed");

    let mut bundled = HostState::new();
    bundled.set_choice(ConsoleHostChoice::Auto);
    let _ = bundled.host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false);
    assert_eq!(
        bundled.host_for_spawn(&program, |_| panic!("kept"), |_| false),
        Some(exe(Origin::Bundled))
    );
}
