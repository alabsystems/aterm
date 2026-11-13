// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_foreign_file_at_the_stub_s_path_is_the_person_s() {
    use crate::doctor::Level;
    use aterm_converge::Class;
    let l = crate::testdir::layout("atpkg-abrew-row-");
    let (row, class) = doctor_row_with(&l, true, false);
    assert_eq!(
        (row.level, class),
        (Level::Warn, Class::Relay),
        "{}",
        row.text
    );
    assert!(row.text.contains("missing"), "{}", row.text);
    std::fs::create_dir_all(stub_path(&l).parent().unwrap()).unwrap();
    std::fs::write(
        stub_path(&l),
        "#!/bin/sh\nexec /opt/homebrew/bin/brew \"$@\"\n",
    )
    .unwrap();
    let (row, class) = doctor_row_with(&l, true, false);
    assert_eq!(
        (row.level, class),
        (Level::Warn, Class::Person),
        "{}",
        row.text
    );
    assert!(
        row.text.contains("foreign file; never touched"),
        "{}",
        row.text
    );
    let (row, class) = doctor_row_with(&l, false, false);
    assert_eq!(
        (row.level, class),
        (Level::Note, Class::Relay),
        "{}",
        row.text
    );
}

#[test]
fn legacy_brew_stub_needs_repair_even_when_native_routing_is_disabled() {
    use crate::doctor::Level;
    use aterm_converge::Class;
    let layout = crate::testdir::layout("atpkg-abrew-legacy-row-");
    let path = stub_path(&layout);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let old = format!("#!/bin/sh\n{LEGACY_STUB_MARKER}\nexec /old/atpkg __abrew \"$@\"\n");
    std::fs::write(&path, &old).unwrap();
    for (enabled, disabled) in [(true, false), (false, false), (true, true)] {
        let (row, class) = doctor_row_with(&layout, enabled, disabled);
        assert_eq!((row.level, class), (Level::Warn, Class::Relay));
        assert!(row.text.contains("native-only refresh"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
    }
}

#[cfg(unix)]
#[test]
fn stub_execs_only_native_abrew_and_refuses_real_brew_bypasses() {
    use std::os::unix::fs::PermissionsExt;
    let owned = aterm_tempfile::TempDir::new().unwrap();
    let tmp = owned.path().join("quoted ' paths");
    std::fs::create_dir_all(&tmp).unwrap();
    let fake_atpkg = tmp.join("atpkg ' backend");
    std::fs::write(
        &fake_atpkg,
        "#!/bin/sh\nprintf 'ATPKG\\n'\nprintf '<%s>\\n' \"$@\"\n",
    )
    .unwrap();
    let fake_brew = tmp.join("real-brew");
    std::fs::write(
        &fake_brew,
        "#!/bin/sh\nprintf 'called\\n' >> \"${BREW_CALLED:?}\"\nprintf 'REAL\\n'\n",
    )
    .unwrap();
    for f in [&fake_atpkg, &fake_brew] {
        std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let stub_file = tmp.join("brew");
    let disabled_file = tmp.join("disabled");
    let called = tmp.join("real-brew-called");
    let mut failures = Vec::new();
    for (label, args, env, disabled, missing, expected) in [
        (
            "native arguments",
            vec!["install", "a b", "'quoted'", "", "$(literal)"],
            None,
            false,
            false,
            Some("ATPKG\n<__abrew_native>\n<install>\n<a b>\n<'quoted'>\n<>\n<$(literal)>\n"),
        ),
        (
            "real flag",
            vec!["--real", "list"],
            None,
            false,
            false,
            None,
        ),
        (
            "real environment",
            vec!["list"],
            Some(("ABREW_REAL", "1")),
            false,
            false,
            None,
        ),
        (
            "no-reroute environment",
            vec!["list"],
            Some(("__ATERM_REROUTE_PASSTHROUGH", "1")),
            false,
            false,
            None,
        ),
        (
            "inherited child marker",
            vec!["list"],
            Some(("__ABREW_REROUTED", "1")),
            false,
            false,
            Some("ATPKG\n<__abrew_native>\n<list>\n"),
        ),
        (
            "disabled backend routing",
            vec!["list"],
            None,
            true,
            false,
            Some("ATPKG\n<__abrew_native>\n<list>\n"),
        ),
        (
            "disabled recovery",
            vec!["on"],
            None,
            true,
            false,
            Some("ATPKG\n<__abrew_native>\n<on>\n"),
        ),
        ("missing backend", vec!["list"], None, false, true, None),
        (
            "legacy env on recovery",
            vec!["on"],
            Some(("ABREW_REAL", "1")),
            false,
            false,
            Some("ATPKG\n<__abrew_native>\n<on>\n"),
        ),
        (
            "legacy env status recovery",
            vec!["status"],
            Some(("ABREW_REAL", "1")),
            false,
            false,
            Some("ATPKG\n<__abrew_native>\n<status>\n"),
        ),
        (
            "no-reroute on recovery",
            vec!["on"],
            Some(("__ATERM_REROUTE_PASSTHROUGH", "1")),
            false,
            false,
            Some("ATPKG\n<__abrew_native>\n<on>\n"),
        ),
        (
            "no-reroute status recovery",
            vec!["status"],
            Some(("__ATERM_REROUTE_PASSTHROUGH", "1")),
            false,
            false,
            Some("ATPKG\n<__abrew_native>\n<status>\n"),
        ),
    ] {
        let _ = std::fs::remove_file(&called);
        if disabled {
            std::fs::write(&disabled_file, "off\n").unwrap();
        } else {
            let _ = std::fs::remove_file(&disabled_file);
        }
        let missing_atpkg = tmp.join("missing-atpkg");
        let backend = if missing { &missing_atpkg } else { &fake_atpkg };
        std::fs::write(&stub_file, stub_body(backend)).unwrap();
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.arg(&stub_file).args(&args);
        cmd.env("ABREW_BREW_PATH", &fake_brew);
        cmd.env("BREW_CALLED", &called);
        cmd.env("PATH", &tmp);
        for name in [
            "ABREW_REAL",
            "__ABREW_REROUTED",
            "__ATERM_REROUTE_PASSTHROUGH",
        ] {
            cmd.env_remove(name);
        }
        if let Some((name, value)) = env {
            cmd.env(name, value);
        }
        let out = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let routed = match expected {
            Some(expected) => out.status.success() && stdout == expected,
            None => !out.status.success() && stdout.is_empty() && !stderr.is_empty(),
        };
        if !routed || called.exists() {
            failures.push((
                label,
                out.status.code(),
                stdout.into_owned(),
                stderr.into_owned(),
                called.exists(),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "(case, exit, stdout, stderr, real brew called): {failures:?}"
    );
}

#[cfg(unix)]
#[test]
fn prefixed_recovery_preserves_arguments_and_keeps_ordinary_commands_blocked() {
    use std::os::unix::fs::PermissionsExt;

    let temp = aterm_tempfile::TempDir::new().unwrap();
    let backend = temp.path().join("atpkg");
    std::fs::write(
        &backend,
        "#!/bin/sh\nprintf 'ATPKG\\n'\nprintf '<%s>\\n' \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = temp.path().join("brew");
    std::fs::write(&path, stub_body(&backend)).unwrap();
    let mut failures = Vec::new();
    for flag in ["--verbose", "-v", "--debug", "-d"] {
        for command in ["on", "status", "list"] {
            for env in ["ABREW_REAL", "__ATERM_REROUTE_PASSTHROUGH"] {
                let mut child = aterm_uds::spawnfd::command("/bin/sh");
                child.arg(&path).args([flag, command]);
                for name in ["ABREW_REAL", "__ATERM_REROUTE_PASSTHROUGH"] {
                    child.env_remove(name);
                }
                let output = child.env(env, "1").output().unwrap();
                let valid = if command == "list" {
                    !output.status.success()
                        && output.stdout.is_empty()
                        && !output.stderr.is_empty()
                } else {
                    output.status.success()
                        && output.stdout
                            == format!("ATPKG\n<{HIDDEN_VERB}>\n<{flag}>\n<{command}>\n").as_bytes()
                };
                if !valid {
                    failures.push(format!(
                        "{env}=1 {flag} {command}: status={:?}, stdout={:?}, stderr={:?}",
                        output.status,
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(unix)]
#[test]
fn stub_names_live_atpkg_rejects_empty_and_missing() {
    use std::os::unix::fs::PermissionsExt;
    let owned = aterm_tempfile::TempDir::new().expect("private stub fixture");
    let tmp = owned.path();
    let fake = tmp.join("atpkg");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let body = stub_body;
    assert!(stub_names_live_atpkg(&body(&fake)));
    assert!(!stub_names_live_atpkg(&body(Path::new(""))));
    assert!(!stub_names_live_atpkg(&body(&tmp.join("gone/atpkg"))));
    let dull = tmp.join("dull");
    std::fs::write(&dull, "not a program\n").unwrap();
    std::fs::set_permissions(&dull, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!stub_names_live_atpkg(&body(&dull)));

    let layout = crate::testdir::layout("native-brew-symlink-");
    let path = stub_path(&layout);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let target_stub = tmp.join("target-brew");
    std::fs::write(&target_stub, body(&fake)).unwrap();
    assert!(is_abrew_stub(&target_stub));
    std::os::unix::fs::symlink(&target_stub, &path).unwrap();
    assert!(
        !is_abrew_stub(&path),
        "symlink to an abrew stub is treated as foreign"
    );
    std::fs::remove_file(&target_stub).unwrap();
    let (row, class) = doctor_row_with(&layout, true, false);
    assert_eq!(class, aterm_converge::Class::Person);
    assert!(row.text.contains("is not ours"), "{}", row.text);
}

#[cfg(unix)]
#[test]
fn legacy_stub_refresh_does_not_call_an_old_backend_legacy_brew_route() {
    use std::os::unix::fs::PermissionsExt;

    let temp = aterm_tempfile::TempDir::new().unwrap();
    let old = temp.path().join("Legacy.app/Contents/MacOS/atpkg");
    let transient = temp.path().join("transient");
    let offered = transient.join("atpkg");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&transient).unwrap();
    std::fs::write(
        &old,
        "#!/bin/sh\nif [ \"${1:-}\" = __abrew ]; then\n  printf 'legacy brew fallback\\n' >> \"${LEGACY_BREW_CALLED:?}\"\n  exit 0\nfi\nprintf 'old backend: unknown verb\\n' >&2\nexit 2\n",
    )
    .unwrap();
    std::fs::write(&offered, "#!/bin/sh\nprintf 'new backend\\n' >&2\nexit 1\n").unwrap();
    for backend in [&old, &offered] {
        std::fs::set_permissions(backend, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let embedder = crate::stub::Embedder::offering(&offered, &[transient]);
    assert_eq!(embedder.embed(Some(&old)), Some(old.clone()));

    let mut failures = Vec::new();
    for disabled in [false, true] {
        let layout = crate::testdir::layout("native-brew-old-backend-");
        let prefix = Prefix::at(&layout.prefix.join("abrew"));
        if disabled {
            std::fs::create_dir_all(&prefix.root).unwrap();
            std::fs::write(prefix.disabled_path(), "off\n").unwrap();
        }
        let path = stub_path(&layout);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n{LEGACY_STUB_MARKER}\nATPKG='{}'\n",
                sh_single(&old.to_string_lossy())
            ),
        )
        .unwrap();
        assert!(lay_pending(&layout, &embedder));
        lay_with(&layout, &embedder).unwrap();
        let refreshed = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            crate::stub::sh_named_atpkg(&refreshed, "ATPKG"),
            Some(old.clone()),
            "repair preserves the more durable installed backend"
        );
        assert!(!lay_pending(&layout, &embedder));
        assert_eq!(prefix.is_disabled(), disabled);

        let called = layout.prefix.join("legacy-brew-called");
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command
            .arg(&path)
            .args(["services", "list"])
            .env("LEGACY_BREW_CALLED", &called)
            .env("ABREW_PREFIX", &prefix.root);
        for name in ["ABREW_REAL", "__ATERM_REROUTE_PASSTHROUGH"] {
            command.env_remove(name);
        }
        let output = command.output().unwrap();
        if output.status.success()
            || called.exists()
            || !String::from_utf8_lossy(&output.stderr).contains("old backend: unknown verb")
        {
            failures.push(format!(
                "disabled={disabled}: status={:?}, legacy_called={}, stdout={:?}, stderr={:?}",
                output.status,
                called.exists(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(unix)]
#[test]
fn legacy_stub_refresh_keeps_disabled_native_route() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD: &str = "ATERM_NATIVE_BREW_OFF_CONFIG_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let config = aterm_tempfile::TempDir::new().unwrap();
        let config_dir = config.path().join("aterm");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("aterm.toml"), "[abrew]\nenabled = false\n").unwrap();
        let output = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "abrew::stub::tests::legacy_stub_refresh_keeps_disabled_native_route",
                "--nocapture",
            ])
            .env(CHILD, config.path().join("child-completed"))
            .env("XDG_CONFIG_HOME", config.path())
            .env_remove("ABREW_PREFIX")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "disabled-config child failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(config.path().join("child-completed")).unwrap(),
            b"refreshed both stubs\n"
        );
        return;
    }
    assert!(
        !crate::config::load_abrew().enabled(),
        "the child really uses disabled configuration"
    );
    for disabled in [false, true] {
        let layout = crate::testdir::layout("native-brew-legacy-");
        let backend = layout.prefix.join("atpkg ' backend");
        std::fs::write(
            &backend,
            "#!/bin/sh\nprintf 'ATPKG\\n'\nprintf '<%s>\\n' \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755)).unwrap();
        let embedder = crate::stub::Embedder::offering(&backend, &[]);
        let prefix = Prefix::at(&layout.prefix.join("abrew"));
        if disabled {
            std::fs::create_dir_all(&prefix.root).unwrap();
            std::fs::write(prefix.disabled_path(), "off\n").unwrap();
        }
        let path = stub_path(&layout);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let legacy = format!(
            "#!/bin/sh\n{LEGACY_STUB_MARKER}\nATPKG='{}'\nprintf 'LEGACY\\n'\n",
            sh_single(&backend.to_string_lossy())
        );
        std::fs::write(&path, &legacy).unwrap();
        assert!(is_abrew_stub(&path));
        assert!(lay_pending(&layout, &embedder));
        lay_with(&layout, &embedder).unwrap();
        let refreshed = std::fs::read_to_string(&path).unwrap();
        assert_ne!(
            refreshed, legacy,
            "the old fallback-capable stub is refreshed"
        );
        assert!(is_abrew_stub(&path));
        assert_eq!(
            crate::stub::sh_named_atpkg(&refreshed, "ATPKG"),
            Some(backend.clone())
        );
        assert!(!lay_pending(&layout, &embedder));
        assert_eq!(
            prefix.is_disabled(),
            disabled,
            "stub repair preserves the off sentinel"
        );
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command.arg(&path).arg("status");
        for name in ["ABREW_REAL", "__ATERM_REROUTE_PASSTHROUGH"] {
            command.env_remove(name);
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"ATPKG\n<__abrew_native>\n<status>\n");

        let foreign = "#!/bin/sh\nprintf 'foreign\\n'\n";
        std::fs::write(&path, foreign).unwrap();
        lay_with(&layout, &embedder).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), foreign);
    }
    std::fs::write(std::env::var_os(CHILD).unwrap(), "refreshed both stubs\n").unwrap();
}
