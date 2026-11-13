// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn the_sh_shim_execs_the_real_path_and_round_trips() {
    let target = Path::new("/prefix/store/trust/5520/bin/targo");
    let body = sh_shim_content(target);
    assert!(body.starts_with("#!/bin/sh\n"), "{body}");
    assert!(
        body.contains("exec '/prefix/store/trust/5520/bin/targo' \"$@\""),
        "the stub must EXEC (replacing the process image) so the tool authenticates \
         at its real path, and forward args verbatim: {body}"
    );
    assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
}

#[test]
fn a_tombstone_is_not_an_installed_shim() {
    let notice = "#!/bin/sh\necho 'atpkg: ay was yanked' 1>&2\nexit 70\n";
    assert_eq!(parse_sh_shim_target(notice), None);
}

#[test]
fn the_sh_shim_with_env_exports_then_execs_and_round_trips() {
    let target = Path::new("/prefix/store/claude/2026082701/bin/claude");
    let env = crate::shim_env::ShimEnv::admit(&[
        "DISABLE_AUTOUPDATER=1".to_string(),
        "B=it's two words".to_string(),
    ])
    .unwrap();
    let body = sh_shim_content_env(target, &env);
    assert!(
        body.contains(
            "\nexport DISABLE_AUTOUPDATER='1'\n\
             export B='it'\\''s two words'\n\
             exec '/prefix/store/claude/2026082701/bin/claude' \"$@\"\n"
        ),
        "{body}"
    );
    assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
    assert_eq!(parse_sh_shim_env(&body), env);
    assert_eq!(
        sh_shim_content_env(target, &crate::shim_env::ShimEnv::NONE),
        sh_shim_content(target),
        "no env: the shim every manifest without the key gets, byte for byte"
    );
    assert_eq!(
        parse_sh_shim_env(&sh_shim_content(target)),
        crate::shim_env::ShimEnv::NONE
    );
    assert_eq!(
        parse_sh_shim_env("#!/bin/sh\necho 'atpkg: ay was yanked' 1>&2\nexit 70\n"),
        crate::shim_env::ShimEnv::NONE
    );
    assert_eq!(
        parse_sh_shim_env(
            "#!/bin/sh\nexport DISABLE_AUTOUPDATER='1'\nexport PATH='/x'\nexec '/a' \"$@\"\n"
        ),
        crate::shim_env::ShimEnv::NONE
    );
}

/// The exports reach the exec'd program, and a plain re-lay drops them.
#[cfg(unix)]
#[test]
fn the_laid_wrapper_exports_into_the_exec_d_program() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::testdir::dir("atpkg-sh-env-shim-");
    let target = root.join("claude");
    std::fs::write(
        &target,
        b"#!/bin/sh\nprintf '%s|%s\\n' \"${DISABLE_AUTOUPDATER:-unset}\" \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shim = root.join("shim");
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    install_shim_to_env(&shim, &target, &env).unwrap();
    assert_eq!(resolve_shim(&shim).as_deref(), Some(target.as_path()));
    assert_eq!(shim_env_of(&shim), env);
    let run_shim = || {
        aterm_spec::wait::past_text_busy(|| {
            std::process::Command::new(&shim)
                .arg("arg")
                .env_remove("DISABLE_AUTOUPDATER")
                .output()
        })
        .unwrap()
    };
    let out = run_shim();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1|arg\n");
    install_shim_to(&shim, &target).unwrap();
    assert_eq!(shim_env_of(&shim), crate::shim_env::ShimEnv::NONE);
    let out = run_shim();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "unset|arg\n");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn an_embedded_quote_cannot_escape_the_stub() {
    let nasty = Path::new("/prefix/store/o'ny/1/bin/x");
    let body = sh_shim_content(nasty);
    assert!(body.contains(r"'\''"), "quote is POSIX-escaped: {body}");
    assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(nasty));
}

#[test]
fn the_routed_shim_is_exact_and_parses_as_the_plain_one() {
    let target = Path::new("/p/store/trust/8595/bin/tippy");
    let route = Path::new("/p/compat/trust/8595/bin/tippy");
    let none = crate::shim_env::ShimEnv::NONE;
    let body = sh_shim_content_routed(target, &none, Some(route));
    assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
    assert_eq!(parse_sh_shim_env(&body), none);
    assert_eq!(
        sh_shim_content_routed(target, &none, None),
        sh_shim_content(target),
        "no route: today's shim, byte for byte"
    );

    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    let body = sh_shim_content_routed(target, &env, Some(route));
    assert!(
        body.find("export DISABLE_AUTOUPDATER='1'").unwrap()
            < body.find("exec '/p/compat/").unwrap(),
        "both execs inherit the exports: {body}"
    );
    assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
    assert_eq!(parse_sh_shim_env(&body), env);
    assert_eq!(
        sh_shim_content_routed(target, &env, None),
        sh_shim_content_env(target, &env)
    );
    assert_eq!(
        body.lines()
            .filter(|l| l.trim_start().starts_with("exec '"))
            .count(),
        1
    );
}

#[test]
fn a_quote_in_the_prefix_is_escaped_in_every_slot_of_the_guard() {
    let target = Path::new("/it's/store/trust/1/bin/targo");
    let route = Path::new("/it's/compat/trust/1/bin/targo");
    let body = sh_shim_content_routed(target, &crate::shim_env::ShimEnv::NONE, Some(route));
    assert_eq!(
        body.matches(r"'/it'\''s/compat/trust/1/bin/targo'").count(),
        4
    );
    assert_eq!(
        body.matches(r"'/it'\''s/store/trust/1/bin/targo'").count(),
        2
    );
    let marker = if cfg!(windows) {
        r"'/it'\''s/compat/trust/1\.atpkg-root'"
    } else {
        r"'/it'\''s/compat/trust/1/.atpkg-root'"
    };
    assert_eq!(body.matches(marker).count(), 2);
    assert!(!body.contains("/it's/"), "no raw quote survives: {body}");
    assert_eq!(parse_sh_shim_target(&body).as_deref(), Some(target));
}

/// The guard under a real `/bin/sh` (and `/bin/dash` where present).
#[cfg(unix)]
#[test]
fn the_guard_runs_the_root_only_when_the_marked_root_stands() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::testdir::dir("atpkg-sh-route-");
    let base = scratch.join("pre fix's");
    let store_bin = base.join("store").join("trust").join("8595").join("bin");
    let root_bin = base.join("compat").join("trust").join("8595").join("bin");
    std::fs::create_dir_all(&store_bin).unwrap();
    std::fs::create_dir_all(&root_bin).unwrap();
    std::fs::create_dir_all(base.join("bin")).unwrap();
    let store = store_bin.join("tippy");
    let route = root_bin.join("tippy");
    std::fs::write(
        &store,
        b"#!/bin/sh\nprintf '%s|%s|%s|%s|%s\\n' \"$0\" \"$#\" \"$1\" \"$2\" \
          \"${DISABLE_AUTOUPDATER:-unset}\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    let shim = base.join("bin").join("tippy");
    std::fs::write(&shim, sh_shim_content_routed(&store, &env, Some(&route))).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

    let shells: Vec<&str> = ["/bin/sh", "/bin/dash"]
        .into_iter()
        .filter(|s| Path::new(s).is_file())
        .collect();
    assert!(shells.contains(&"/bin/sh"), "every Unix has /bin/sh");
    let run = |shell: &str| -> String {
        let out = std::process::Command::new(shell)
            .arg(&shim)
            .arg("a b")
            .arg("it's \"q\"")
            .env_clear()
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let ran = |path: &Path| format!("{}|2|a b|it's \"q\"|1\n", path.display());

    let marker = crate::compat::root_marker(&base.join("compat").join("trust").join("8595"));
    for (what, marked, runs_route) in [
        ("clone", true, true),
        ("byte copy", true, true),
        ("clone", false, false),
        ("clone behind a symlinked marker", false, false),
        ("symlink", true, false),
        ("not executable", true, false),
        ("missing", true, false),
    ] {
        let _ = std::fs::remove_file(&route);
        let _ = std::fs::remove_file(&marker);
        match what {
            "clone" | "clone behind a symlinked marker" => {
                crate::clone::clone_file(&store, &route).unwrap();
            }
            "byte copy" => std::fs::copy(&store, &route).map(|_| ()).unwrap(),
            "symlink" => std::os::unix::fs::symlink(&store, &route).unwrap(),
            "not executable" => {
                std::fs::copy(&store, &route).unwrap();
                std::fs::set_permissions(&route, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            _ => {}
        }
        if marked {
            std::fs::write(&marker, b"atpkg exec root v1\n").unwrap();
        } else if what == "clone behind a symlinked marker" {
            std::os::unix::fs::symlink(&store, &marker).unwrap();
        }
        let expected = if runs_route { &route } else { &store };
        for shell in &shells {
            assert_eq!(
                run(shell),
                ran(expected),
                "{shell}, route is a {what}, marker {marked}"
            );
        }
    }
    std::fs::remove_dir_all(base.join("compat")).unwrap();
    for shell in &shells {
        assert_eq!(run(shell), ran(&store), "{shell}, no root");
    }
    std::fs::remove_dir_all(base.parent().unwrap()).unwrap();
}

#[test]
fn the_self_update_block_is_exact_is_the_whole_prelude_and_breaks_no_reader() {
    let prefix = Path::new("/Users//u/Library/Application Support/aterm/pkg");
    let atpkg = Path::new("/Applications/aterm.app/Contents/MacOS/atpkg");
    let target = Path::new(
        "/Users//u/Library/Application Support/aterm/pkg/store/claude/2026091902/bin/claude",
    );
    let verbs = crate::selfupdate::verbs_of("claude");
    let block = sh_selfupdate_prelude("claude", prefix, atpkg, verbs);
    for code in [
        "\ncase \"$1\" in\n  update|upgrade|install)\n",
        "\n    __atpkg='/Applications/aterm.app/Contents/MacOS/atpkg'\n",
        "__ATPKG_SELFUPDATE_PID=$$ exec \"$__atpkg\" __selfupdate 'claude' \
         '/Users//u/Library/Application Support/aterm/pkg' -- \"$@\"; fi\n    ;;\nesac\n",
    ] {
        assert!(block.contains(code), "{code:?} in {block}");
    }
    let codex = sh_selfupdate_prelude("codex", prefix, atpkg, crate::selfupdate::verbs_of("codex"));
    assert!(codex.contains("\n  update)\n"), "{codex}");
    assert!(codex.contains("__selfupdate 'codex' "), "{codex}");
    assert_eq!(sh_selfupdate_prelude("claude", prefix, atpkg, &[]), "");
    assert_eq!(
        sh_selfupdate_prelude("ay", prefix, atpkg, crate::selfupdate::verbs_of("ay")),
        "",
        "a program with no row has no verbs and no block"
    );
    assert_eq!(
        sh_selfupdate_prelude("muse", prefix, atpkg, crate::selfupdate::verbs_of("muse")),
        "",
        "a rostered program with no updater has no block"
    );
    assert_eq!(
        sh_selfupdate_prelude("claude", prefix, atpkg, &["update", "up|date)"]),
        ""
    );
    assert_eq!(
        sh_selfupdate_prelude("claude", prefix, atpkg, &["Update"]),
        ""
    );
    assert_eq!(
        sh_selfupdate_prelude("claude", prefix, atpkg, &["esac"]),
        ""
    );
    assert_eq!(
        sh_selfupdate_prelude("claude", prefix, atpkg, &["update", "esac"]),
        ""
    );
    assert_eq!(sh_selfupdate_prelude("cl'aude", prefix, atpkg, verbs), "");
    assert_eq!(sh_selfupdate_prelude("cl aude", prefix, atpkg, verbs), "");
    assert_eq!(
        sh_selfupdate_prelude("sudo", prefix, atpkg, verbs),
        "",
        "not a tool name"
    );
    let quoted = sh_selfupdate_prelude(
        "claude",
        Path::new("/it's/pkg"),
        Path::new("/it's/app/atpkg"),
        verbs,
    );
    assert!(quoted.contains("__atpkg='/it'\\''s/app/atpkg'"), "{quoted}");
    assert!(
        quoted.contains("__selfupdate 'claude' '/it'\\''s/pkg' -- \"$@\"; fi"),
        "{quoted}"
    );
    assert!(!quoted.contains("/it's/"), "{quoted}");
    let want = block.clone();
    let twin_pre = twin_prelude(
        "claude",
        prefix,
        Some(atpkg),
        verbs,
        &crate::yolo::Policy::NONE,
    );
    if cfg!(windows) {
        assert_eq!(twin_pre, "", "the .cmd twin renders no prelude");
    } else {
        assert_eq!(twin_pre, want);
    }
    assert!(!want.contains(crate::landing::HIDDEN_VERB), "{want}");
    assert!(!want.contains("landing"), "{want}");
    assert_eq!(
        twin_prelude(
            "claude",
            prefix,
            Some(atpkg),
            &[],
            &crate::yolo::Policy::NONE
        ),
        "",
        "no verbs, no yolo policy: no prelude — the twin is the plain shim"
    );
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    let twin = sh_shim_content_twin(target, &env, None, &want);
    assert_eq!(
        twin.lines()
            .filter(|l| l.trim().starts_with("exec '"))
            .count(),
        1,
        "{twin}"
    );
    assert!(
        !want.lines().any(|l| l.trim().starts_with("export ")),
        "{want}"
    );
    assert_eq!(parse_sh_shim_target(&twin).as_deref(), Some(target));
    assert_eq!(parse_sh_shim_env(&twin), env);
    assert!(!twin.contains("pending-program stub"), "{twin}");
    assert!(
        twin.find("case \"$1\"").unwrap() < twin.find("export DISABLE_AUTOUPDATER").unwrap(),
        "{twin}"
    );
    assert!(twin.trim_end().ends_with("\"$@\""));
    assert!(!sh_shim_content_env(target, &env).contains("__selfupdate"));
    assert_eq!(
        sh_shim_content_twin(target, &env, None, ""),
        sh_shim_content_env(target, &env)
    );
}

/// THE HARNESS HAND-OFF EXECUTED: grok's laid twin execs `<prefix>/bin/grok-alab run --` with
/// the argv its yolo block left once a LIVE grok-alab is installed (a shim into a ready store
/// build, or into its checkout while dev-linked), runs the store build alone under
/// `--no-alab-harness`, `--no-harness` or `ATERM_NO_HARNESS=1`, before grok-alab is installed,
/// and whenever `bin/grok-alab` is not live: a yanked/revoked tombstone, a shim whose build was
/// removed or never marked ready, a checkout shim with no dev link. No other agent's twin
/// carries the hand-off.
#[cfg(unix)]
#[test]
fn the_grok_twin_hands_its_session_to_grok_alab_once_installed() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::testdir::dir("atpkg-sh-harness-twin-");
    let script = |path: &Path, tag: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!("#!/bin/sh\nprintf '{tag}'\nprintf '<%s>' \"$@\"\nprintf '\\n'\n"),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::testdir::settled(path);
    };
    let target = root.join("store-build");
    script(&target, "V");
    let prefix = root.join("Application Support").join("pkg");
    let layout = crate::store::Layout {
        prefix: prefix.clone(),
    };
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let prelude = twin_prelude("grok", &prefix, None, &[], &crate::yolo::policy_of("grok"));
    // `ATERM_NO_HARNESS` is read twice, once per block that `--no-harness` takes away: the
    // hand-off's guard, and the sign-in block's guard (its own test below). Each is there once.
    let reads = |also: &str| {
        prelude
            .lines()
            .filter(|l| l.contains("ATERM_NO_HARNESS-") && l.contains(also))
            .count()
    };
    assert_eq!(reads("bin/grok-alab'"), 1, "the hand-off: {prelude}");
    assert_eq!(reads("ATERM_CHILD-"), 1, "the sign-in guard: {prelude}");
    assert_eq!(reads(""), 2, "{prelude}");
    let twin = root.join("grok");
    std::fs::write(
        &twin,
        sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, &prelude),
    )
    .unwrap();
    std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(resolve_shim(&twin).as_deref(), Some(target.as_path()));
    // The twin as laid (its `#!/bin/sh`), and under every other POSIX shell here (Linux's
    // /bin/sh is dash), each answering alike. Outside aterm and with no `aterm` on PATH, so
    // grok's sign-in block (its own test below) never runs here, and never reaches a real
    // `aterm` on a signed-out machine.
    let shells: Vec<&str> = ["/bin/dash", "/usr/bin/dash", "/bin/bash"]
        .into_iter()
        .filter(|s| Path::new(s).exists())
        .collect();
    let run = |args: &[&str], no_harness: bool| {
        let once = |shell: Option<&str>| {
            let out = aterm_spec::wait::past_text_busy(|| {
                let mut cmd = match shell {
                    Some(sh) => {
                        let mut c = std::process::Command::new(sh);
                        c.arg(&twin);
                        c
                    }
                    None => std::process::Command::new(&twin),
                };
                cmd.args(args)
                    .env_remove("ATERM_NO_HARNESS")
                    .env_remove("ATERM_CHILD")
                    .env("PATH", "/usr/bin:/bin");
                if no_harness {
                    cmd.env("ATERM_NO_HARNESS", "1");
                }
                cmd.output()
            })
            .unwrap();
            assert!(out.status.success(), "{shell:?} {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let laid = once(None);
        for sh in &shells {
            assert_eq!(once(Some(sh)), laid, "{sh} {args:?}");
        }
        laid
    };
    let argv = ["-m", "grok-4", "fix it"];
    let alone = run(&argv, false);
    assert!(
        alone.starts_with("V<"),
        "not installed: the store build: {alone}"
    );
    // A live grok-alab: atpkg's shim into a ready store build.
    let alab_bin = prefix.join("bin").join("grok-alab");
    let build_dir = layout.build_dir("grok-alab", 1_000);
    let build_exe = build_dir.join("bin").join("grok-alab");
    script(&build_exe, "H");
    let ready = prefix.join("store").join("grok-alab").join("1000.ready");
    std::fs::write(&ready, "ok\n").unwrap();
    let lay_shim = |to: &Path| {
        std::fs::write(&alab_bin, sh_shim_content(to)).unwrap();
        std::fs::set_permissions(&alab_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_shim(&alab_bin).as_deref(), Some(to));
        crate::testdir::settled(&alab_bin);
    };
    lay_shim(&build_exe);
    let handed = run(&argv, false);
    assert_eq!(
        handed,
        format!("H<run><-->{}", alone.strip_prefix('V').unwrap()),
        "the harness gets `run --` and the very argv the store build got"
    );
    assert_eq!(
        run(&["--no-alab-harness", "-m", "grok-4", "fix it"], false),
        alone
    );
    assert_eq!(
        run(&["--no-harness", "-m", "grok-4", "fix it"], false),
        alone
    );
    assert_eq!(run(&argv, true), alone);
    // A yanked or revoked grok-alab: atpkg's tombstone at bin/grok-alab (executable, exits 70,
    // no exec line) never stands between the person and grok.
    crate::platform::install_tombstone_shim(
        &alab_bin,
        "atpkg: grok-alab was yanked/revoked — run `aterm pkg update`",
    )
    .unwrap();
    crate::testdir::settled(&alab_bin);
    assert!(
        resolve_shim(&alab_bin).is_none(),
        "a tombstone execs nothing"
    );
    assert_eq!(run(&argv, false), alone, "a tombstone: grok alone");
    // A shim whose build is not marked ready, then one whose build was removed.
    lay_shim(&build_exe);
    assert_eq!(run(&argv, false), handed, "the control: live again");
    std::fs::remove_file(&ready).unwrap();
    assert_eq!(run(&argv, false), alone, "an unfinished build: grok alone");
    std::fs::write(&ready, "ok\n").unwrap();
    std::fs::remove_dir_all(&build_dir).unwrap();
    assert_eq!(run(&argv, false), alone, "a removed build: grok alone");
    // A shim into a checkout runs only while grok-alab is dev-linked.
    let checkout_exe = root.join("checkout").join("target").join("grok-alab");
    script(&checkout_exe, "H");
    lay_shim(&checkout_exe);
    assert_eq!(run(&argv, false), alone, "no dev link: grok alone");
    let marker = layout.link_marker("grok-alab");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, "program = \"grok-alab\"\n").unwrap();
    assert_eq!(run(&argv, false), handed, "dev-linked: the harness");
    for other in ["claude", "codex", "agy", "muse"] {
        let prelude = twin_prelude(other, &prefix, None, &[], &crate::yolo::policy_of(other));
        assert!(
            !prelude.contains("grok-alab") && !prelude.contains(" run -- "),
            "{other}"
        );
    }
}

/// THE YOLO BLOCK EXECUTED: laid twins over a store build that prints its argv.
#[cfg(unix)]
#[test]
fn the_yolo_block_expands_prints_and_reaches_the_exec() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::testdir::dir("atpkg-sh-yolo-twin-");
    let target = root.join("store-build");
    std::fs::write(&target, b"#!/bin/sh\nprintf '<%s>' \"$@\"\nprintf '\\n'\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let prefix = root.join("pkg");
    let lay = |program: &str| {
        let prelude = twin_prelude(
            program,
            &prefix,
            None,
            &[],
            &crate::yolo::policy_of(program),
        );
        assert!(
            !prelude.lines().any(|l| l.trim().starts_with("exec '")),
            "{prelude}"
        );
        assert!(
            !prelude.lines().any(|l| l.trim().starts_with("export ")),
            "{prelude}"
        );
        assert!(!prelude.contains("__selfupdate"), "{prelude}");
        let twin = root.join(program);
        std::fs::write(
            &twin,
            sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, &prelude),
        )
        .unwrap();
        std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_shim(&twin).as_deref(), Some(target.as_path()));
        twin
    };
    // Outside aterm, so grok's sign-in block (its own test below) stays out of these runs.
    let run = |twin: &Path, args: &[&str]| {
        let out = aterm_spec::wait::past_text_busy(|| {
            std::process::Command::new(twin)
                .args(args)
                .env_remove("ATERM_CHILD")
                .output()
        })
        .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let claude = lay("claude");
    assert_eq!(
        run(&claude, &["-p", "hello"]),
        (
            "<--dangerously-skip-permissions><-p><hello>\n".into(),
            "aterm: yolo (default): running 'claude' \
             '--dangerously-skip-permissions' '-p' 'hello'\n"
                .into()
        )
    );
    for args in [
        &["--permission-mode", "plan"][..],
        &["--permission-mode=auto"][..],
        &["--dangerously-skip-permissions", "-p", "x"][..],
    ] {
        let (stdout, stderr) = run(&claude, args);
        assert_eq!(stderr, "", "{args:?}");
        assert!(!stdout.contains("aterm:"), "{args:?}: {stdout}");
    }
    assert_eq!(
        run(&claude, &["--permission-mode", "plan"]).0,
        "<--permission-mode><plan>\n"
    );
    let codex = lay("codex");
    for args in [
        &["-sread-only", "exec", "x"][..],
        &["-a=never", "exec", "x"][..],
        &["-c", "sandbox_mode=\"read-only\"", "exec", "x"][..],
        &["--config", "approval_policy=\"on-request\""][..],
        &["--config=sandbox_mode=\"workspace-write\""][..],
        &["-csandbox_mode=\"read-only\""][..],
        &["-c=approval_policy=\"never\""][..],
        &["-c", " \tapproval_policy \t=\"on-request\""][..],
        &[
            "resume",
            "thread-id",
            "--config",
            "sandbox_mode=\"read-only\"",
        ][..],
        &[
            "-c",
            "model=opus",
            "--config=approval_policy=\"on-request\"",
        ][..],
    ] {
        let (stdout, stderr) = run(&codex, args);
        assert_eq!(stderr, "", "{args:?}");
        assert_eq!(
            stdout,
            format!(
                "{}\n",
                args.iter()
                    .map(|arg| format!("<{arg}>"))
                    .collect::<String>()
            ),
            "{args:?}"
        );
    }
    for args in [
        &["--model", "opus", "resume", "thread-id"][..],
        &["-m", "opus", "fork", "thread-id"][..],
        &["--model=opus", "resume", "thread-id"][..],
        &["-mopus", "fork", "thread-id"][..],
        &["--cd", "/owned/path", "resume", "thread-id"][..],
        &["resume", "thread-id"][..],
        &["fork", "thread-id"][..],
        &["-c", "model_reasoning_effort=high", "resume", "thread-id"][..],
        &[
            "--config=model=\"sandbox_mode=read-only\"",
            "fork",
            "thread-id",
        ][..],
        &["--profile", "work", "resume", "thread-id"][..],
    ] {
        let (stdout, stderr) = run(&codex, args);
        let expected = format!(
            "<--dangerously-bypass-approvals-and-sandbox>{}\n",
            args.iter()
                .map(|arg| format!("<{arg}>"))
                .collect::<String>()
        );
        assert_eq!(stdout, expected, "{args:?}");
        assert!(stderr.starts_with(crate::yolo::DEFAULT_ECHO_PREFIX));
    }
    let explicit = [
        "--yolo",
        "-c",
        "sandbox_mode=\"read-only\"",
        "resume",
        "thread-id",
    ];
    let (stdout, stderr) = run(&codex, &explicit);
    assert_eq!(
        stdout,
        "<--dangerously-bypass-approvals-and-sandbox><-c><sandbox_mode=\"read-only\"><resume><thread-id>\n"
    );
    assert!(stderr.starts_with(crate::yolo::ECHO_PREFIX));
    let (stdout, stderr) = run(
        &claude,
        &["--model", "opus", "--yolo", "-p", "it's $x `y`", ""],
    );
    assert_eq!(
        stdout,
        "<--model><opus><--dangerously-skip-permissions><-p><it's $x `y`><>\n"
    );
    let typed: Vec<String> = ["--model", "opus", "--yolo", "-p", "it's $x `y`", ""]
        .iter()
        .map(|w| (*w).to_string())
        .collect();
    let crate::yolo::Launch::Flag(expanded) = crate::yolo::decide("claude", &typed) else {
        panic!("flag launch");
    };
    assert_eq!(
        stderr,
        format!("{}\n", crate::yolo::echo_line("claude", &expanded))
    );
    let (stdout, stderr) = run(&claude, &["--", "--yolo"]);
    assert_eq!(stdout, "<--dangerously-skip-permissions><--><--yolo>\n");
    assert_eq!(
        stderr,
        "aterm: yolo (default): running 'claude' '--dangerously-skip-permissions' \
         '--' '--yolo'\n"
    );
    let muse = lay("muse");
    let behind = "<exec><--disable-approval><--disable-sandbox><--trust-workspace>\
                  <--disable-request-user-input><x>\n";
    assert_eq!(run(&muse, &["exec", "x"]).0, behind);
    assert_eq!(run(&muse, &["--yolo", "exec", "x"]).0, behind);
    assert_eq!(run(&muse, &["exec", "--yolo", "x"]).0, behind);
    assert_eq!(
        run(&muse, &["skills", "list"]),
        ("<skills><list>\n".into(), String::new())
    );
    assert_eq!(
        run(&muse, &["--approval-mode", "never"]),
        ("<--approval-mode><never>\n".into(), String::new())
    );
    let (stdout, stderr) = run(&muse, &[]);
    assert_eq!(
        stdout,
        "<--disable-approval><--disable-sandbox><--trust-workspace>\
         <--disable-request-user-input>\n"
    );
    assert_eq!(
        stderr,
        format!(
            "{}\n",
            crate::yolo::default_echo_line(
                "muse",
                &crate::yolo::flags_of("muse")
                    .iter()
                    .map(|f| (*f).to_string())
                    .collect::<Vec<_>>()
            )
        )
    );
    for args in [
        &["--new-option", "value", "resume", "thread-id"][..],
        &["--image", "one.png", "two.png", "resume", "thread-id"][..],
    ] {
        assert_eq!(
            run(&codex, args),
            (
                format!(
                    "{}\n",
                    args.iter().map(|a| format!("<{a}>")).collect::<String>()
                ),
                String::new()
            ),
            "{args:?}"
        );
    }
    assert_eq!(
        run(&codex, &["--model", "resume", "fix this"]).0,
        "<--dangerously-bypass-approvals-and-sandbox><--model><resume><fix this>\n"
    );
    for args in [
        &["--model", "exec", "exec", "x"][..],
        &["--yolo", "--model", "exec", "exec", "x"][..],
        &["--model", "exec", "--yolo", "exec", "x"][..],
    ] {
        assert_eq!(
            run(&muse, args).0,
            "<--model><exec><exec><--disable-approval><--disable-sandbox>\
             <--trust-workspace><--disable-request-user-input><x>\n",
            "{args:?}"
        );
    }
    assert_eq!(
        run(&muse, &["--new-option", "value", "--yolo", "exec", "x"]),
        (
            "<--new-option><value><--yolo><exec><x>\n".into(),
            String::new()
        )
    );
    let agy = lay("agy");
    assert_eq!(
        run(&claude, &["--no-harness", "-p", "hello"]).0,
        "<--dangerously-skip-permissions><-p><hello>\n"
    );
    assert_eq!(
        run(&agy, &["--no-harness", "hello"]).0,
        "<--dangerously-skip-permissions><hello>\n"
    );
    assert_eq!(
        run(&agy, &["--no-alab-harness", "--", "--no-harness"]).0,
        "<--dangerously-skip-permissions><--><--no-harness>\n"
    );
    // grok and kimi: the twin runs exactly what `decide` says, printing the line it prints
    // (their self-update verbs are the update intercept's, which this twin is laid without).
    let grok = lay("grok");
    let kimi = lay("kimi");
    for (program, twin, args) in [
        ("grok", &grok, &[][..]),
        ("grok", &grok, &["-m", "grok-4", "fix it"][..]),
        ("grok", &grok, &["--resume", "agent", "--json"][..]),
        ("grok", &grok, &["--yolo", "agent", "-p", "hi"][..]),
        ("grok", &grok, &["--cwd", "/w", "--yolo"][..]),
        ("grok", &grok, &["--permission-mode", "plan"][..]),
        ("grok", &grok, &["agent", "--sandbox=strict"][..]),
        ("grok", &grok, &["--unknown", "--yolo", "agent"][..]),
        ("grok", &grok, &["mcp", "list"][..]),
        ("grok", &grok, &["cursor-worker", "start"][..]),
        ("grok", &grok, &["--effort", "high", "fix it"][..]),
        ("grok", &grok, &["-w", "login"][..]),
        ("grok", &grok, &["-w", "feat", "agent", "-p", "hi"][..]),
        ("grok", &grok, &["-wfeat", "--resume", "agent"][..]),
        ("grok", &grok, &["-w", "--yolo"][..]),
        ("grok", &grok, &["-w", "--yolo", "agent", "-p", "hi"][..]),
        ("grok", &grok, &["-r", "--yolo", "--yolo", "fix it"][..]),
        ("grok", &grok, &["-r", "--no-plan", "--yolo", "agent"][..]),
        (
            "grok",
            &grok,
            &["--yolo", "--dangerously-skip-permissions", "agent"][..],
        ),
        ("kimi", &kimi, &[][..]),
        ("kimi", &kimi, &["--yolo"][..]),
        ("kimi", &kimi, &["-m", "k2", "--yolo", "--", "--yolo"][..]),
        ("kimi", &kimi, &["--agent", "coder", "hello"][..]),
        ("kimi", &kimi, &["-y"][..]),
        ("kimi", &kimi, &["-phi"][..]),
        ("kimi", &kimi, &["--session=abc"][..]),
        ("kimi", &kimi, &["--unknown", "--yolo"][..]),
        ("kimi", &kimi, &["acp"][..]),
        ("kimi", &kimi, &["--yolo", "-y"][..]),
        ("kimi", &kimi, &["--yes", "--yolo", "--auto-approve"][..]),
        (
            "kimi",
            &kimi,
            &["-y", "-m", "k2", "--yolo", "--add-dir", "/w"][..],
        ),
        ("kimi", &kimi, &["--yolo", "upgrade", "-y"][..]),
        ("kimi", &kimi, &["--yolo", "-y", "--", "-y"][..]),
        ("kimi", &kimi, &["--yolo", "-S", "-y", "login"][..]),
        ("kimi", &kimi, &["--yolo", "-S", "abc", "-y"][..]),
        ("kimi", &kimi, &["--unknown", "--yolo", "-y"][..]),
    ] {
        let typed: Vec<String> = args.iter().map(|w| (*w).to_string()).collect();
        let (ran, said) = match crate::yolo::decide(program, &typed) {
            crate::yolo::Launch::AsTyped => (typed, String::new()),
            crate::yolo::Launch::Flag(v) => {
                let line = format!("{}\n", crate::yolo::echo_line(program, &v));
                (v, line)
            }
            crate::yolo::Launch::Default(v) => {
                let line = format!("{}\n", crate::yolo::default_echo_line(program, &v));
                (v, line)
            }
        };
        let shown: String = ran.iter().map(|a| format!("<{a}>")).collect();
        assert_eq!(
            run(twin, args),
            (format!("{shown}\n"), said),
            "{program} {args:?}"
        );
    }
    assert_eq!(
        run(&grok, &["agent", "-p", "hi"]).0,
        "<agent><--always-approve><-p><hi>\n"
    );
    assert_eq!(run(&kimi, &["--yolo"]).0, "<--auto>\n");
    assert_eq!(run(&kimi, &["-y", "--yolo"]).0, "<--auto>\n");
    std::fs::remove_dir_all(&root).unwrap();
}

/// THE SIGN-IN BLOCK EXECUTED: grok's laid twin over a store build that prints its argv,
/// with a stand-in `aterm` on `PATH` that records each call and exits as told. Under aterm a
/// signed-out interactive launch runs `aterm harness signin --cli grok --wait` before the
/// exec and runs grok only on its exit 0; a one-shot option, a subcommand, a signed-in
/// environment or a launch outside aterm never calls it.
#[cfg(unix)]
#[test]
fn the_signin_block_gates_a_signed_out_interactive_grok_on_aterm() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::testdir::dir("atpkg-sh-signin-twin-");
    let target = root.join("store-build");
    std::fs::write(&target, b"#!/bin/sh\nprintf '<%s>' \"$@\"\nprintf '\\n'\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = root.join("aterm.calls");
    let code = root.join("aterm.exit");
    let aterm = bin.join("aterm");
    std::fs::write(
        &aterm,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit \"$(cat '{}')\"\n",
            calls.display(),
            code.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&aterm, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".grok")).unwrap();
    let prelude = twin_prelude(
        "grok",
        &root.join("pkg"),
        None,
        &[],
        &crate::yolo::policy_of("grok"),
    );
    assert!(prelude.contains("# atpkg: sign-in gate"), "{prelude}");
    assert!(
        !prelude.lines().any(|l| l.trim().starts_with("exec '")),
        "{prelude}"
    );
    assert!(
        !prelude.lines().any(|l| l.trim().starts_with("export ")),
        "{prelude}"
    );
    let twin = root.join("grok");
    std::fs::write(
        &twin,
        sh_shim_content_twin(&target, &crate::shim_env::ShimEnv::NONE, None, &prelude),
    )
    .unwrap();
    std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(resolve_shim(&twin).as_deref(), Some(target.as_path()));

    let path_with = format!("{}:/usr/bin:/bin", bin.display());
    // One launch: (exit code, stdout, stderr, aterm's calls).
    let run = |args: &[&str], env: &[(&str, &str)], exit: u8| {
        let _ = std::fs::remove_file(&calls);
        std::fs::write(&code, exit.to_string()).unwrap();
        let out = aterm_spec::wait::past_text_busy(|| {
            let mut cmd = std::process::Command::new(&twin);
            cmd.args(args)
                .env_clear()
                .env("PATH", &path_with)
                .env("HOME", &home);
            for (k, v) in env {
                cmd.env(k, v);
            }
            cmd.output()
        })
        .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            std::fs::read_to_string(&calls).unwrap_or_default(),
        )
    };
    let child = [("ATERM_CHILD", "1")];
    let signin = "harness signin --cli grok --wait\n";

    // Signed out, interactive: aterm runs first, and grok only on its exit 0.
    for args in [
        &[][..],
        &["fix it"][..],
        &["-m", "grok-4", "fix it"][..],
        &["-m", "login"][..],
        &["-w", "login"][..],
        &["-w", "feat", "--resume"][..],
        &["--effort=high", "fix it"][..],
        &["--", "login"][..],
        &["fix it", "login"][..],
        // Subcommands that open the interactive screen, or may, are gated too: `dashboard`
        // is in the yolo row's skip list (it takes no permission flag), not a local one.
        &["dashboard"][..],
        &["-m", "grok-4", "dashboard"][..],
        &["sessions"][..],
        &["-w", "feat", "sessions"][..],
        &["models"][..],
        &["agent", "stdio"][..],
        &["some-future-verb"][..],
    ] {
        let (exit, stdout, stderr, called) = run(args, &child, 0);
        assert_eq!(exit, Some(0), "{args:?}: {stderr}");
        assert_eq!(called, signin, "{args:?}");
        assert!(stdout.starts_with('<'), "{args:?}: grok ran: {stdout:?}");
        let (exit, stdout, stderr, called) = run(args, &child, 1);
        assert_eq!(exit, Some(69), "{args:?}: {stderr}");
        assert_eq!(called, signin, "{args:?}");
        assert_eq!(stdout, "", "{args:?}: grok never ran");
        assert_eq!(
            stderr,
            "grok: not signed in to Grok Build (aterm harness signin --cli grok says why \
             above); nothing was started\n",
            "{args:?}"
        );
    }
    // aterm missing: one line, exit 69, nothing run.
    let (exit, stdout, stderr, called) = {
        let _ = std::fs::remove_file(&calls);
        let out = std::process::Command::new(&twin)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("ATERM_CHILD", "1")
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            std::fs::read_to_string(&calls).unwrap_or_default(),
        )
    };
    assert_eq!(
        (exit, stdout.as_str(), called.as_str()),
        (Some(69), "", ""),
        "{stderr}"
    );
    assert_eq!(
        stderr,
        "grok: signed out of Grok Build, and aterm (which signs this machine in once) is \
         not on PATH; nothing was started\n"
    );

    // One-shot options, subcommands and `--no-harness` never call aterm, and grok runs.
    for args in [
        &["login"][..],
        &["logout"][..],
        &["login", "--device-auth"][..],
        &["--help"][..],
        &["-h"][..],
        &["--version"][..],
        &["-v"][..],
        &["-p", "hi"][..],
        &["--print=hi"][..],
        &["--single", "hi"][..],
        &["-m", "grok-4", "-p", "hi"][..],
        &["fix it", "-p", "hi"][..],
        &["update"][..],
        &["mcp", "list"][..],
        &["doctor"][..],
        &["leader", "kill"][..],
        &["-w", "feat", "inspect"][..],
        &["--no-harness"][..],
        &["--no-alab-harness", "fix it"][..],
    ] {
        let (exit, stdout, stderr, called) = run(args, &child, 1);
        assert_eq!(exit, Some(0), "{args:?}: {stderr}");
        assert_eq!(called, "", "{args:?}");
        assert!(stdout.starts_with('<'), "{args:?}: {stdout:?}");
    }

    // Signed in by its environment or files: aterm is never called.
    let auth = home.join(".grok").join("auth.json");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let pooled = elsewhere.join("pooled.json");
    std::fs::write(elsewhere.join("auth.json"), b"{}").unwrap();
    std::fs::write(&pooled, b"{}").unwrap();
    let elsewhere_s = elsewhere.display().to_string();
    let pooled_s = pooled.display().to_string();
    for env in [
        &[("GROK_AUTH", "x")][..],
        &[("XAI_API_KEY", "x")][..],
        &[("GROK_HOME", elsewhere_s.as_str())][..],
        &[("GROK_AUTH_PATH", pooled_s.as_str())][..],
        &[("ATERM_NO_HARNESS", "1")][..],
    ] {
        let mut all = child.to_vec();
        all.extend_from_slice(env);
        let (exit, stdout, _, called) = run(&[], &all, 1);
        assert_eq!((exit, called.as_str()), (Some(0), ""), "{env:?}");
        assert!(stdout.starts_with('<'), "{env:?}");
    }
    // An empty login file is signed out; the program's own home file signs it in.
    std::fs::write(&auth, b"").unwrap();
    assert_eq!(run(&[], &child, 1).0, Some(69));
    std::fs::write(&auth, b"{}").unwrap();
    let (exit, _, _, called) = run(&[], &child, 1);
    assert_eq!((exit, called.as_str()), (Some(0), ""));
    // A login grok refused (`auth.json.refused` at least as new) is signed out, and the gate
    // asks aterm; a newer login signs it in again.
    let marker = home.join(".grok").join("auth.json.refused");
    std::fs::write(&marker, b"").unwrap();
    let marked = std::fs::metadata(&marker).unwrap().modified().unwrap();
    std::fs::File::options()
        .write(true)
        .open(&auth)
        .unwrap()
        .set_modified(marked - std::time::Duration::from_secs(5))
        .unwrap();
    let (exit, stdout, _, called) = run(&[], &child, 1);
    assert_eq!(
        (exit, stdout.as_str(), called.as_str()),
        (Some(69), "", signin)
    );
    std::fs::File::options()
        .write(true)
        .open(&auth)
        .unwrap()
        .set_modified(marked + std::time::Duration::from_secs(5))
        .unwrap();
    let (exit, _, _, called) = run(&[], &child, 1);
    assert_eq!((exit, called.as_str()), (Some(0), ""), "a newer login");
    std::fs::remove_file(&marker).unwrap();
    // A named login file is authoritative: an empty one is signed out even beside a
    // signed-in home.
    let empty = root.join("empty.json");
    std::fs::write(&empty, b"").unwrap();
    let empty_s = empty.display().to_string();
    let (exit, _, _, called) = run(&[], &[child[0], ("GROK_AUTH_PATH", empty_s.as_str())], 1);
    assert_eq!((exit, called.as_str()), (Some(69), signin));
    std::fs::remove_file(&auth).unwrap();

    // Outside aterm the twin is unchanged: signed out, it runs grok and never calls aterm.
    for args in [&[][..], &["fix it"][..]] {
        let (exit, stdout, _, called) = run(args, &[], 1);
        assert_eq!((exit, called.as_str()), (Some(0), ""), "{args:?}");
        assert!(stdout.starts_with('<'), "{args:?}");
        let (_, stdout, _, _) = run(args, &[("ATERM_CHILD", "0")], 1);
        assert!(stdout.starts_with('<'), "{args:?}");
    }
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_signin_block_renders_for_grok_alone_and_fail_closed() {
    let gate = crate::vendor_direct::GROK_SIGNIN_GATE;
    let grok = sh_signin_prelude("grok", "Grok Build", &gate, &["login"]);
    assert!(grok.starts_with("# atpkg: sign-in gate"), "{grok}");
    assert!(
        grok.contains("harness signin --cli 'grok' --wait"),
        "{grok}"
    );
    for program in ["claude", "codex", "kimi", "muse", "agy"] {
        let prelude = twin_prelude(
            program,
            Path::new("/p"),
            None,
            &[],
            &crate::yolo::policy_of(program),
        );
        assert!(!prelude.contains("sign-in gate"), "{program}");
    }
    use crate::vendor_direct::SigninGate;
    for bad in [
        SigninGate {
            cli: "Grok",
            ..gate
        },
        SigninGate { cli: "g'k", ..gate },
        SigninGate {
            credential_env: &["GROK AUTH"],
            ..gate
        },
        SigninGate {
            credential_env: &["$(x)"],
            ..gate
        },
        SigninGate {
            login_file_env: "a-b",
            ..gate
        },
        SigninGate {
            home_env: "",
            ..gate
        },
        SigninGate {
            home_default: "..",
            ..gate
        },
        SigninGate {
            home_default: "a/b",
            ..gate
        },
        SigninGate {
            login_file: "a\"b",
            ..gate
        },
        SigninGate {
            one_shot: &[],
            ..gate
        },
        SigninGate {
            one_shot: &["--x'y"],
            ..gate
        },
        SigninGate {
            one_shot: &["*"],
            ..gate
        },
    ] {
        assert_eq!(
            sh_signin_prelude("grok", "Grok Build", &bad, &["login"]),
            "",
            "{bad:?}"
        );
    }
    assert_eq!(sh_signin_prelude("grok", "Grok's", &gate, &[]), "");
    assert_eq!(sh_signin_prelude("grok", "", &gate, &[]), "");
    assert_eq!(
        sh_signin_prelude("grok", "Grok Build", &gate, &["esac"]),
        ""
    );
    assert_eq!(sh_signin_prelude("grok", "Grok Build", &gate, &["a|b"]), "");
    assert_eq!(sh_signin_prelude("../x", "Grok Build", &gate, &[]), "");
}

#[test]
fn the_yolo_block_renders_fail_closed() {
    use crate::yolo::Policy;
    let ok = Policy {
        flags: &["--ok"],
        markers: &["--ok"],
        ..Policy::NONE
    };
    assert_eq!(sh_yolo_prelude("claude", &Policy::NONE, &[]), "");
    assert_eq!(
        sh_yolo_prelude("claude", &Policy { markers: &[], ..ok }, &[]),
        "",
        "no markers: the default cannot tell a decided launch"
    );
    assert_eq!(
        sh_yolo_prelude(
            "claude",
            &Policy {
                flags: &["--ok", "not-a-flag"],
                ..ok
            },
            &[]
        ),
        ""
    );
    assert_eq!(
        sh_yolo_prelude(
            "claude",
            &Policy {
                markers: &["--yo lo"],
                ..ok
            },
            &[]
        ),
        ""
    );
    assert_eq!(
        sh_yolo_prelude(
            "claude",
            &Policy {
                after: &["ex'ec"],
                ..ok
            },
            &[]
        ),
        ""
    );
    assert_eq!(sh_yolo_prelude("Claude", &ok, &[]), "");
    assert_eq!(sh_yolo_prelude("../claude", &ok, &[]), "");
    let block = sh_yolo_prelude("muse", &crate::yolo::policy_of("muse"), &[]);
    let note = block.lines().next().unwrap();
    assert!(
        note.starts_with("# ")
            && note.contains(" muse ")
            && note.contains(
                "('--disable-approval' '--disable-sandbox' '--trust-workspace' \
                 '--disable-request-user-input')"
            ),
        "{note}"
    );
    assert!(block.contains(crate::yolo::ECHO_PREFIX), "{block}");
    assert!(block.contains(crate::yolo::DEFAULT_ECHO_PREFIX), "{block}");
}

#[cfg(unix)]
#[test]
fn rosetta_present_and_open_create_write_refuse_symlinks_and_enforce_mode() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let root = crate::testdir::dir("atpkg-platform-unix-harden-");
    let real = root.join("real");
    std::fs::write(&real, b"runtime").unwrap();
    assert_eq!(rosetta_present(&real), Ok(true));

    let link = root.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        rosetta_present(&link),
        Ok(false),
        "rosetta_present must not follow symlinks"
    );
    assert!(
        open_create_write(&link, 0o600).is_err(),
        "open_create_write must refuse symlinks"
    );
    assert_eq!(std::fs::read(&real).unwrap(), b"runtime");

    let loose = root.join("loose");
    std::fs::write(&loose, b"stale-longer-bytes").unwrap();
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut f = open_create_write(&loose, 0o600).unwrap();
    f.write_all(b"fresh").unwrap();
    drop(f);
    assert_eq!(std::fs::read(&loose).unwrap(), b"fresh");
    assert_eq!(
        std::fs::symlink_metadata(&loose)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "open_create_write must tighten permissions on an existing file"
    );

    let _ = std::fs::remove_dir_all(&root);
}
