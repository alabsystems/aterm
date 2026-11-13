// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
fn permission_changes_recover_unreadable_entries_without_following_links() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let root = crate::testdir::dir("atpkg-chmod-nofollow-");
    let dir = root.join("directory");
    let file = root.join("file");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(&file, b"private").unwrap();
    for (entry, restored) in [(&dir, 0o700), (&file, 0o600)] {
        std::fs::set_permissions(entry, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = set_mode(entry, restored);
        // Keep the fixture removable even if recovery regresses.
        let observed = std::fs::metadata(entry).unwrap().permissions().mode() & 0o7777;
        std::fs::set_permissions(entry, std::fs::Permissions::from_mode(restored)).unwrap();
        result.unwrap();
        assert_eq!(observed, restored);
        let link = root.join(format!("link-{restored:o}"));
        symlink(entry, &link).unwrap();
        assert!(set_mode(&link, 0o777).is_err());
        assert_eq!(
            std::fs::metadata(entry).unwrap().permissions().mode() & 0o7777,
            restored
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The clamp's decision.
#[cfg(unix)]
#[test]
fn only_an_absolute_executable_runs_behind_the_utility_clamp() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::testdir::dir("atpkg-clamp-");
    let launcher = dir.join("taskpolicy");
    std::fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let plain = dir.join("plain");
    std::fs::write(&plain, "").unwrap();
    let argv = |program: &Path, launcher: &Path| -> Vec<String> {
        utility_clamped_with(program.as_os_str(), launcher)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    };
    let l = launcher.to_string_lossy().into_owned();
    assert_eq!(
        argv(Path::new("/bin/sh"), &launcher),
        [l.as_str(), "-c", "utility", "/bin/sh"]
    );
    let missing = dir.join("no-such-tool");
    for (program, why) in [
        (Path::new("sh"), "a relative program"),
        (missing.as_path(), "a missing program"),
        (plain.as_path(), "a file with no execute bit"),
        (&*dir, "a directory"),
    ] {
        assert_eq!(
            argv(program, &launcher),
            [program.to_string_lossy().into_owned()],
            "{why} is started bare, so it fails as itself"
        );
    }
    assert_eq!(
        argv(Path::new("/bin/sh"), &dir.join("no-launcher")),
        ["/bin/sh"],
        "no launcher, no clamp"
    );
}

/// The live contract.
#[cfg(target_os = "macos")]
#[test]
fn a_detached_program_starts_at_utility_and_a_waited_one_at_the_default_band() {
    type Spawn =
        fn(&std::ffi::OsStr, &[std::ffi::OsString], Option<(&str, &str)>) -> io::Result<()>;
    let pri_of = |spawn: Spawn| -> i32 {
        let dir = crate::testdir::dir("atpkg-detached-band-");
        let said = dir.join("pri");
        let script = format!(
            "/bin/ps -o pri= -p $$ > '{0}.tmp' && mv '{0}.tmp' '{0}'",
            said.display()
        );
        spawn("/bin/sh".as_ref(), &["-c".into(), script.into()], None).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let pri = loop {
            if let Ok(text) = std::fs::read_to_string(&said) {
                break text;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the detached program never ran"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        pri.trim()
            .parse()
            .unwrap_or_else(|_| panic!("no priority in {pri:?}"))
    };
    let pri = pri_of(spawn_detached);
    assert!(
        pri <= 20,
        "a detached pass ran at pri {pri}, not utility (20)"
    );
    let pri = pri_of(spawn_detached_waited);
    assert!(
        pri > 20,
        "a first run's detached pass ran at pri {pri}, below the default band (31)"
    );
}

/// Observed at creation: the temp is private from the instant it exists.
#[cfg(unix)]
#[test]
fn a_private_temp_is_never_created_wider_than_0600() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::testdir::dir("atpkg-private-new-");
    let path = dir.join(".zshrc.atpkg-1.tmp");
    let f = create_private_new(&path).unwrap();
    assert_eq!(
        f.metadata().unwrap().permissions().mode() & 0o777,
        0o600,
        "the temp must be private from the instant it exists"
    );
    drop(f);
    assert!(
        create_private_new(&path).is_err(),
        "exclusive: an existing temp is never opened and truncated"
    );
}

#[test]
fn cmd_shim_content_wraps_target_and_forwards_args() {
    let c = cmd_shim_content(Path::new("C:\\store\\ay\\18\\bin\\ay.exe"));
    let mut want = cmd_frame();
    want.push_str("@\"C:\\store\\ay\\18\\bin\\ay.exe\" %* & @exit /b\r\n");
    assert_eq!(c, want);
}

#[test]
fn cmd_shim_round_trips_through_parse() {
    let target = PathBuf::from("C:\\store\\ay\\18\\bin\\ay.exe");
    let content = cmd_shim_content(&target);
    assert_eq!(parse_cmd_shim_target(&content), Some(target));
}

#[test]
fn tombstone_content_has_no_forward_target_and_exits_70() {
    let c = cmd_tombstone_content("atpkg: ay was yanked/revoked — run `aterm pkg update`");
    assert!(c.contains("1>&2"), "notice goes to stderr: {c}");
    assert!(c.contains("exit /b 70"), "exits 70: {c}");
    assert!(c.is_ascii(), "a .cmd tombstone is ASCII: {c}");
    assert_eq!(parse_cmd_shim_target(&c), None);

    let multiline = cmd_tombstone_content("bad \u{26a0}\n@\"C:\\evil.exe\" %*\r\nmore");
    assert!(
        multiline.is_ascii(),
        "all non-ASCII neutralized: {multiline}"
    );
    assert_eq!(parse_cmd_shim_target(&multiline), None);
    assert_eq!(
        multiline
            .lines()
            .filter(|l| l.starts_with("@echo "))
            .count(),
        1
    );
}

#[test]
fn cmd_echo_escape_neutralizes_metacharacters() {
    let esc = cmd_echo_escape("a&b|c>d<e^f%g\"h");
    assert_eq!(esc, "a^&b^|c^>d^<e^^f%%g^\"h");
}

#[test]
fn cmd_shim_with_env_sets_then_forwards_and_round_trips() {
    let target = PathBuf::from("C:\\store\\claude\\2026082701\\bin\\claude.exe");
    let env = crate::shim_env::ShimEnv::admit(&[
        "DISABLE_AUTOUPDATER=1".to_string(),
        "B=two words".to_string(),
    ])
    .unwrap();
    let c = cmd_shim_content_env(&target, &env);
    let mut want = cmd_frame();
    want.push_str(
        "@set \"DISABLE_AUTOUPDATER=1\"\r\n@set \"B=two words\"\r\n\
         @\"C:\\store\\claude\\2026082701\\bin\\claude.exe\" %* & @exit /b\r\n",
    );
    assert_eq!(c, want);
    assert_eq!(parse_cmd_shim_target(&c), Some(target.clone()));
    assert_eq!(parse_cmd_shim_env(&c), env);
    assert_eq!(
        cmd_shim_content_env(&target, &crate::shim_env::ShimEnv::NONE),
        cmd_shim_content(&target),
        "no env: the shim every manifest without the key gets"
    );
    assert_eq!(
        parse_cmd_shim_env(&cmd_shim_content(&target)),
        crate::shim_env::ShimEnv::NONE
    );
    assert_eq!(
        parse_cmd_shim_env(&cmd_tombstone_content("atpkg: x was yanked")),
        crate::shim_env::ShimEnv::NONE
    );
    assert_eq!(
        parse_cmd_shim_env(
            "@set \"DISABLE_AUTOUPDATER=1\"\r\n@set \"PATH=C:\\x\"\r\n@\"C:\\a.exe\" %* & @exit /b\r\n"
        ),
        crate::shim_env::ShimEnv::NONE
    );
    assert!(cmd_env_is_injection_safe(&env));
    assert!(cmd_env_is_injection_safe(&crate::shim_env::ShimEnv::NONE));
    let root = crate::testdir::dir("atpkg-cmd-shim-env-");
    let path = root.join("claude.cmd");
    std::fs::write(&path, &c).unwrap();
    assert_eq!(read_cmd_shim_env(&path), env);
    assert_eq!(read_cmd_shim_target(&path), Some(target));
    assert_eq!(
        read_cmd_shim_env(&root.join("absent.cmd")),
        crate::shim_env::ShimEnv::NONE
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// On a real Windows host the backend writes, resolves and reads the env-carrying `.cmd`.
#[cfg(windows)]
#[test]
fn windows_backend_lays_and_reads_an_env_shim() {
    let root = crate::testdir::dir("atpkg-win-env-shim-");
    let target = root.join("claude.exe");
    std::fs::write(&target, b"x").unwrap();
    let shim = root.join("claude.cmd");
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    install_shim_to_env(&shim, &target, &env).unwrap();
    assert_eq!(
        std::fs::read_to_string(&shim).unwrap(),
        cmd_shim_content_env(&target, &env)
    );
    assert_eq!(resolve_shim(&shim), Some(target.clone()));
    assert_eq!(shim_env_of(&shim), env);
    install_shim_to(&shim, &target).unwrap();
    assert_eq!(shim_env_of(&shim), crate::shim_env::ShimEnv::NONE);
    assert_eq!(resolve_shim(&shim), Some(target));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn parse_cmd_shim_target_keys_on_the_forward_tail_and_reads_the_legacy_one() {
    assert_eq!(parse_cmd_shim_target("not a shim\r\n"), None);
    assert_eq!(parse_cmd_shim_target(""), None);
    let target = PathBuf::from("C:\\a.exe");
    assert_eq!(
        parse_cmd_shim_target("@\"C:\\a.exe\" %* & @exit /b\r\n"),
        Some(target.clone())
    );
    assert_eq!(
        parse_cmd_shim_target("@\"C:\\a.exe\" %*\r\n"),
        Some(target),
        "a legacy shim still resolves until it is re-laid"
    );
    for not_a_forward in [
        "@\"C:\\a.exe\"\r\n",
        "@\"C:\\a.exe\" %1\r\n",
        "@\"C:\\a.exe\" %* & @exit /b 3\r\n",
        "@\"C:\\a.exe\" %* & exit /b\r\n",
        "@\"C:\\a.exe\" %*& @exit /b\r\n",
        "@\"C:\\a.exe\" __landing \"claude\" \"C:\\pkg\" -- %* & @exit /b\r\n",
    ] {
        assert_eq!(
            parse_cmd_shim_target(not_a_forward),
            None,
            "{not_a_forward:?}"
        );
    }
}

#[test]
fn a_legacy_cmd_twin_with_the_landing_prelude_still_resolves_to_the_store() {
    let prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
    let marker = Path::new("C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude");
    let atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
    let target = Path::new(
        "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
    );
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    let prelude = cmd_landing_prelude("claude", prefix, marker, atpkg);
    assert_eq!(
        prelude,
        "@rem atpkg agents twin: while a newer build of this program is landing, atpkg __landing \
         waits for it and then runs the new one (aterm help pkg).\r\n\
         @if not exist \"C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude\" goto store\r\n\
         @if not exist \"C:\\Program Files (x86)\\aterm\\app\\atpkg.exe\" goto store\r\n\
         @\"C:\\Program Files (x86)\\aterm\\app\\atpkg.exe\" __landing \"claude\" \
         \"C:\\Program Files (x86)\\aterm\\pkg\" -- %* & @exit /b\r\n\
         :store\r\n"
    );
    assert_eq!(prelude.matches("atpkg.exe").count(), 2);
    let twin = cmd_shim_content_twin(target, &env, &prelude);
    let mut want = cmd_frame();
    want.push_str(&prelude);
    want.push_str("@set \"DISABLE_AUTOUPDATER=1\"\r\n");
    want.push_str(
        "@\"C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe\" \
         %* & @exit /b\r\n",
    );
    assert_eq!(twin, want);
    for line in twin.split("\r\n").filter(|l| !l.is_empty()) {
        assert!(
            line.starts_with('@') || line.starts_with(':'),
            "silent: {line}"
        );
        assert!(
            !line.ends_with('(') && !line.starts_with(')'),
            "no block: {line}"
        );
        if line.starts_with("@\"") {
            assert!(
                line.ends_with(CMD_FORWARD_TAIL),
                "ends the batch on the line that runs the program: {line}"
            );
        } else {
            assert!(
                !line.contains("exit /b") || line == CMD_PADDING_EXIT.trim_end(),
                "no exit line of its own past the frame's: {line}"
            );
        }
        assert!(!line.starts_with("@call"), "no call: {line}");
    }
    assert!(!twin.contains("errorlevel"), "{twin}");
    assert_eq!(
        twin.matches(" & @exit /b").count(),
        2,
        "the hand-over and the forward, each ending the batch: {twin}"
    );
    assert_eq!(
        twin.matches("\r\n@exit /b\r\n").count(),
        1,
        "the frame's padding exit, the one exit line of its own: {twin}"
    );
    assert!(!twin.contains('\n') || twin.matches("\r\n").count() == twin.matches('\n').count());
    assert!(!twin.contains("where atpkg"));
    assert!(!twin.contains("\\\""), "{twin}");
    assert_eq!(parse_cmd_shim_target(&twin), Some(target.to_path_buf()));
    assert_eq!(parse_cmd_shim_env(&twin), env);
    assert_eq!(
        parse_cmd_shim_target(
            "@\"C:\\app\\atpkg.exe\" __landing \"claude\" \"C:\\pkg\" -- %* & @exit /b\r\n"
        ),
        None,
        "the hand-over line alone is no forward line"
    );
    assert_eq!(
        parse_cmd_shim_target("@\"C:\\a.exe\"\r\n"),
        None,
        "a quoted program with no forward tail is not the shim's forward"
    );
    assert_eq!(
        strip_verbatim_prefix(Path::new("\\\\?\\UNC\\srv\\share\\aterm\\atpkg.exe")),
        PathBuf::from("\\\\srv\\share\\aterm\\atpkg.exe")
    );
    assert_eq!(
        strip_verbatim_prefix(Path::new("\\\\?\\C:\\aterm\\atpkg.exe")),
        PathBuf::from("C:\\aterm\\atpkg.exe")
    );
    assert_eq!(
        strip_verbatim_prefix(Path::new("/usr/local/aterm/atpkg")),
        PathBuf::from("/usr/local/aterm/atpkg")
    );
    assert!(twin.find("__landing").unwrap() < twin.find("@set ").unwrap());
    assert!(twin.trim_end().ends_with("\" %* & @exit /b"));
    assert!(!cmd_shim_content_env(target, &env).contains("__landing"));
    assert_eq!(
        cmd_shim_content_twin(target, &env, ""),
        cmd_shim_content_env(target, &env)
    );
    let root = crate::testdir::dir("atpkg-cmd-twin-");
    let path = root.join("claude.cmd");
    std::fs::write(&path, &twin).unwrap();
    assert_eq!(read_cmd_shim_target(&path), Some(target.to_path_buf()));
    assert_eq!(read_cmd_shim_env(&path), env);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_cmd_twin_renders_no_prelude_and_is_the_plain_framed_shim() {
    let prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
    let atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
    let target = Path::new(
        "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
    );
    if cfg!(windows) {
        assert_eq!(
            twin_prelude(
                "claude",
                prefix,
                Some(atpkg),
                &["update", "upgrade", "install"],
                &crate::yolo::policy_of("claude")
            ),
            "",
            "on Windows the twin carries no prelude"
        );
    }
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    let twin = cmd_shim_content_twin(target, &env, "");
    assert_eq!(
        twin,
        cmd_shim_content_env(target, &env),
        "the plain framed shim"
    );
    assert_eq!(twin.matches(" & @exit /b").count(), 1, "{twin}");
    assert_eq!(twin.matches("\r\n@exit /b\r\n").count(), 1, "{twin}");
    assert!(!twin.contains("%~1"), "{twin}");
    assert!(!twin.contains(crate::selfupdate::HIDDEN_VERB), "{twin}");
    assert!(!twin.contains(crate::landing::HIDDEN_VERB), "{twin}");
    assert_eq!(parse_cmd_shim_target(&twin), Some(target.to_path_buf()));
    assert_eq!(parse_cmd_shim_env(&twin), env);
}

#[test]
fn a_legacy_cmd_re_laid_while_executing_resumes_into_the_frame_at_every_offset() {
    fn legacy(target: &Path, env: &crate::shim_env::ShimEnv) -> String {
        let mut s = String::new();
        for (n, v) in env.entries() {
            s.push_str("@set \"");
            s.push_str(n);
            s.push('=');
            s.push_str(v);
            s.push_str("\"\r\n");
        }
        s.push_str("@\"");
        s.push_str(&target.to_string_lossy());
        s.push('"');
        s.push_str(CMD_LEGACY_FORWARD_TAIL);
        s.push_str("\r\n");
        s
    }
    let prefix = Path::new("C:\\Program Files (x86)\\aterm\\pkg");
    let marker = Path::new("C:\\Program Files (x86)\\aterm\\pkg\\landing\\claude");
    let atpkg = Path::new("C:\\Program Files (x86)\\aterm\\app\\atpkg.exe");
    let target = Path::new(
        "C:\\Program Files (x86)\\aterm\\pkg\\store\\claude\\2026091601\\bin\\claude.exe",
    );
    let none = crate::shim_env::ShimEnv::NONE;
    let env = crate::shim_env::ShimEnv::admit(&[
        "DISABLE_AUTOUPDATER=1".to_string(),
        "B=two words".to_string(),
    ])
    .unwrap();
    let longest_raw: Vec<String> = (0..crate::shim_env::MAX_SHIM_ENV)
        .map(|i| format!("V{i}={}", "x".repeat(crate::shim_env::MAX_ENTRY_BYTES - 3)))
        .collect();
    let longest_env = crate::shim_env::ShimEnv::admit(&longest_raw).unwrap();
    let longest_target = PathBuf::from(format!(
        "C:\\{}\\claude.exe",
        "p".repeat(CMD_LEGACY_TARGET_BOUND_BYTES - "C:\\\\claude.exe".len())
    ));
    assert_eq!(
        longest_target.to_string_lossy().len(),
        CMD_LEGACY_TARGET_BOUND_BYTES
    );
    let prelude = cmd_landing_prelude("claude", prefix, marker, atpkg);
    assert!(!prelude.is_empty());
    let cases = [
        (
            "legacy plain twin -> framed twin behind the prelude",
            legacy(target, &none),
            cmd_shim_content_twin(target, &none, &prelude),
        ),
        (
            "legacy env shim -> framed env shim",
            legacy(target, &env),
            cmd_shim_content_env(target, &env),
        ),
        (
            "longest legacy shim -> its framed successor",
            legacy(&longest_target, &longest_env),
            cmd_shim_content_env(&longest_target, &longest_env),
        ),
    ];
    assert_eq!(
        cases[2].1.len(),
        CMD_LEGACY_FILE_BOUND_BYTES,
        "the longest case IS the derived bound"
    );
    for (what, old, new) in &cases {
        assert_eq!(
            parse_cmd_shim_target(old),
            parse_cmd_shim_target(new),
            "{what}"
        );
        assert_eq!(parse_cmd_shim_env(old), parse_cmd_shim_env(new), "{what}");
        let old = old.as_bytes();
        let new = new.as_bytes();
        assert!(
            old.len() <= CMD_PADDING_END_BYTES,
            "{what}: {} bytes",
            old.len()
        );
        let ends: Vec<usize> = old
            .iter()
            .enumerate()
            .filter(|&(_, &b)| b == b'\n')
            .map(|(i, _)| i + 1)
            .collect();
        assert_eq!(ends.last().copied(), Some(old.len()), "{what}");
        assert!(
            ends.iter().all(|&e| e >= CMD_LEGACY_MIN_LINE_BYTES),
            "{what}: {ends:?}"
        );
        for offset in 0..=old.len() {
            let line = cmd_resumed_line(new, offset);
            if offset == 0 {
                assert_eq!(
                    line.as_deref(),
                    Some("@goto :main"),
                    "{what}: the fresh run"
                );
                continue;
            }
            if offset < CMD_FRAME_HEAD.len() - 2 {
                assert!(
                    !ends.contains(&offset),
                    "{what}: an old line ends at {offset}"
                );
                continue;
            }
            assert!(
                cmd_resumed_line_is_inert(line.as_deref()),
                "{what}: a resume at offset {offset} of {} reads {line:?}",
                old.len()
            );
        }
        let at_eof = cmd_resumed_line(new, old.len()).unwrap();
        assert!(
            !at_eof.is_empty() && at_eof.bytes().all(|b| b == b':'),
            "{what}: {at_eof:?}"
        );
        for end in &ends {
            assert!(cmd_resumed_line_is_inert(
                cmd_resumed_line(new, *end).as_deref()
            ));
        }
    }
    let framed = cmd_shim_content(target);
    for offset in CMD_FRAME_HEAD.len() - 2..=CMD_PADDING_END_BYTES {
        let line = cmd_resumed_line(framed.as_bytes(), offset);
        assert!(
            cmd_resumed_line_is_inert(line.as_deref()),
            "{offset}: {line:?}"
        );
    }
    assert_eq!(
        cmd_resumed_line(framed.as_bytes(), CMD_PADDING_END_BYTES).as_deref(),
        Some("@exit /b")
    );
    assert_eq!(
        cmd_resumed_line(framed.as_bytes(), CMD_FRAME_HEAD.len() - 2).as_deref(),
        Some(""),
        "the head's CRLF reads empty"
    );
    assert_eq!(
        cmd_resumed_line(framed.as_bytes(), 1).as_deref(),
        Some("goto :main"),
        "why the head must sit where no old line can end"
    );
    let padding_end = framed.find("@exit /b").unwrap();
    assert!(cases[2].1.len() <= padding_end, "{}", cases[2].1.len());
    assert_eq!(
        cmd_resumed_line(framed.as_bytes(), cases[2].1.len()).as_deref(),
        Some(&":".repeat(78)[(cases[2].1.len() - CMD_FRAME_HEAD.len()) % 80..]),
        "the bound's own end-of-file offset, inside the padding"
    );
    let mut bare = prelude.clone();
    bare.push_str(&cmd_shim_body_env(target, &none));
    let old_len = cases[0].1.len();
    let line = cmd_resumed_line(bare.as_bytes(), old_len).unwrap();
    assert!(
        !cmd_resumed_line_is_inert(Some(&line)),
        "laid bare, the resume lands in the prelude: {line:?}"
    );
    assert!(framed.trim_end().ends_with(CMD_FORWARD_TAIL));
}

#[test]
fn cmd_target_injection_guard_rejects_quote_percent_newline() {
    assert!(cmd_target_is_injection_safe(Path::new(
        "C:\\Users\\me\\AppData\\Local\\aterm\\pkg\\store\\ay\\18\\bin\\ay.exe"
    )));
    assert!(cmd_target_is_injection_safe(Path::new(
        "/managed/store/ay/18/bin/ay"
    )));
    assert!(!cmd_target_is_injection_safe(Path::new(
        "C:\\x\\\"&calc.exe\"\\ay.exe"
    )));
    assert!(!cmd_target_is_injection_safe(Path::new(
        "C:\\x\\%APPDATA%\\ay.exe"
    )));
    assert!(!cmd_target_is_injection_safe(Path::new(
        "C:\\x\\ay.exe\r\n@calc"
    )));
}

#[test]
fn cmd_shim_reader_rejects_sparse_oversize() {
    let root = crate::testdir::dir("atpkg-cmd-shim-sparse-");
    let path = root.join("ay.cmd");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len((MAX_SHIM_BYTES + 1) as u64).unwrap();
    assert!(read_cmd_shim_target(&path).is_none());
    std::fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn cmd_shim_reader_rejects_fifo_and_symlink_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let root = crate::testdir::dir("atpkg-cmd-shim-special-");
    let path = root.join("ay.cmd");
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    assert!(read_cmd_shim_target(&path).is_none());
    std::fs::remove_file(&path).unwrap();
    let target = root.join("target.cmd");
    std::fs::write(&target, cmd_shim_content(Path::new("C:\\ay.exe"))).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(read_cmd_shim_target(&path).is_none());
    std::fs::remove_dir_all(&root).unwrap();
}
