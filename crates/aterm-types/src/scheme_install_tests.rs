// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

const KITTY: &str = "## name: Harbour\nforeground #c0caf5\nbackground #1a1b26\n";

#[test]
fn windows_replace_waits_for_a_scheduled_file_holder() {
    let scratch = scratch("scheduled-windows-replace");
    let dir = scratch.path();
    let staged = dir.join("staged");
    let target = dir.join("target");
    std::fs::write(&staged, b"complete").unwrap();
    std::fs::write(&target, b"before").unwrap();
    let mut calls = 0;
    let result = again_while_refused_windows_at(
        || {
            calls += 1;
            if calls > 31 {
                std::fs::rename(&staged, &target)
            } else {
                Err(std::io::Error::from_raw_os_error(32))
            }
        },
        std::time::Instant::now() - std::time::Duration::from_secs(1),
        |_| {},
    );
    result.expect("the Windows sharing refusal lifts before the hang detector");
    assert_eq!(calls, 32);
    assert_eq!(std::fs::read(&target).unwrap(), b"complete");
    assert!(!staged.exists());
}

#[test]
fn windows_replace_preserves_the_last_refusal_at_the_hang_detector() {
    for code in HELD_OS_ERRORS {
        let mut calls = 0;
        let result: std::io::Result<()> = again_while_refused_windows_at(
            || {
                calls += 1;
                Err(std::io::Error::from_raw_os_error(code))
            },
            std::time::Instant::now() - std::time::Duration::from_secs(61),
            |_| panic!("an exhausted hang detector must not wait again"),
        );
        assert_eq!(result.unwrap_err().raw_os_error(), Some(code));
        assert_eq!(calls, 1);
    }
    let result: std::io::Result<()> = again_while_refused_windows_at(
        || Err(std::io::ErrorKind::NotFound.into()),
        std::time::Instant::now(),
        |_| panic!("an unrelated error must return immediately"),
    );
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

fn scratch(tag: &str) -> aterm_tempfile::TempDir {
    crate::test_scratch_dir(tag)
}

/// A refused replace is tried again on Windows until it goes through,
/// and any other error is returned at once.
#[test]
fn a_refused_replace_is_tried_again_only_on_windows_and_only_so_often() {
    use std::io::{Error, ErrorKind};
    let mut calls = 0u64;
    let out = again_while_refused(|| {
        calls += 1;
        if calls < 3 {
            Err(Error::from(ErrorKind::PermissionDenied))
        } else {
            Ok(calls)
        }
    });
    if cfg!(windows) {
        assert_eq!(out.unwrap(), 3, "two refusals, then the replace");
    } else {
        assert_eq!(out.unwrap_err().kind(), ErrorKind::PermissionDenied);
        assert_eq!(calls, 1);
    }

    for code in [32, 33] {
        let mut calls = 0u64;
        let out = again_while_refused(|| {
            calls += 1;
            if calls < 3 {
                Err(Error::from_raw_os_error(code))
            } else {
                Ok(calls)
            }
        });
        if cfg!(windows) {
            assert_eq!(
                out.unwrap(),
                3,
                "os error {code}: two holds, then the replace"
            );
        } else {
            assert_eq!(out.unwrap_err().raw_os_error(), Some(code));
            assert_eq!(calls, 1);
        }
    }

    let others: [fn() -> Error; 2] = [
        || Error::from(ErrorKind::NotFound),
        || Error::from_raw_os_error(2),
    ];
    for error in others {
        let mut calls = 0u64;
        let out: std::io::Result<()> = again_while_refused(|| {
            calls += 1;
            Err(error())
        });
        assert_eq!(out.unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(calls, 1);
    }
}

#[test]
fn a_failed_exclusive_publication_never_falls_back_to_overwriting() {
    let dir = scratch("exclusive");
    let root = dir.path();
    let tmp = root.join("pending");
    let dest = root.join("mine.conf");
    std::fs::write(&tmp, "new theme").unwrap();

    let failed = publish_new(&tmp, &dest, |_, dest| {
        std::fs::write(dest, "other writer")?;
        Err(std::io::ErrorKind::Unsupported.into())
    });
    assert!(failed.is_err());
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "other writer");
    assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "new theme");

    std::fs::remove_file(&dest).unwrap();
    let failed = publish_new(&tmp, &dest, |_, _| {
        Err(std::io::ErrorKind::Unsupported.into())
    });
    assert!(failed.is_err(), "exclusive publication was unavailable");
    assert!(
        !dest.exists(),
        "a failed publication must not install a theme"
    );
    assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "new theme");
}

/// An arbitrary export filename can be our usual temporary basename.
#[test]
fn publication_keeps_a_destination_named_like_its_temporary_file() {
    const BARE_NAME: &str = "ATERM_TYPES_SCHEME_INSTALL_BARE_NAME";
    const LAST: &str = "foreground = #000000\nbackground = #ffffff\n";
    let id = format!("export-alias-{:?}", std::thread::current().id());
    let name = format!(".{id}.conf.import-{}", std::process::id());
    let publish = |parent: &Path, dest: &Path| {
        for (replace, text) in [
            (false, "foreground = #ffffff\nbackground = #000000\n"),
            (true, LAST),
        ] {
            write_atomically(parent, dest, &id, text.as_bytes(), replace).unwrap();
            let actual = std::fs::read_to_string(dest).expect("published destination remains");
            assert_eq!(actual, text);
            assert_eq!(
                scheme::parse_theme_str(&actual).unwrap(),
                scheme::parse_theme_str(text).unwrap()
            );
        }
    };
    if std::env::var_os(BARE_NAME).is_some() {
        let bare = PathBuf::from(&name);
        assert_eq!(
            std::fs::symlink_metadata(&bare).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        publish(Path::new("."), &bare);
        return;
    }
    let temp = scratch("temp-destination");
    let dir = temp.path();
    publish(dir, &dir.join(&name));
    let cwd = dir.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let test = format!(
        "{}::publication_keeps_a_destination_named_like_its_temporary_file",
        module_path!().split_once("::").map_or("", |(_, rest)| rest)
    );
    let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args(["--exact", &test, "--nocapture", "--test-threads=1"])
        .env(BARE_NAME, "1")
        .current_dir(&cwd)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("re-exec the test binary");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the re-exec failed:\n{said}");
    assert!(
        said.contains("1 passed"),
        "the re-exec ran no test:\n{said}"
    );
    let left: Vec<_> = std::fs::read_dir(&cwd)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        left[0]
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".export-alias-")),
        "{left:?}"
    );
    assert_eq!(std::fs::read_to_string(&left[0]).unwrap(), LAST);
}

/// A window's import keeps the theme's own name, steps around a
/// built-in's id, and never replaces an existing theme.
#[test]
fn import_as_new_keeps_the_display_name_and_steps_around_builtins() {
    let dir = scratch("as-new");
    let root = dir.path();
    let themes = root.join("themes");
    let source = root.join("Nord.conf");
    std::fs::write(&source, KITTY).unwrap();
    let installed = install_theme_file_as_new(&source, Some(&themes)).unwrap();
    assert_eq!(installed.id, "Nord (imported)");
    assert_eq!(installed.dest, themes.join("Nord (imported).conf"));
    let parsed =
        scheme::parse_theme_str(&std::fs::read_to_string(&installed.dest).unwrap()).unwrap();
    assert_eq!(parsed.scheme.name, "Harbour", "the source's own name stays");

    let again = install_theme_file_as_new(&source, Some(&themes));
    assert_eq!(again, Err(InstallError::Exists(installed.dest.clone())));
    let named = install_theme_file(&source, Some("mine"), false, Some(&themes)).unwrap();
    let parsed = scheme::parse_theme_str(&std::fs::read_to_string(&named.dest).unwrap()).unwrap();
    assert_eq!(parsed.scheme.name, "mine");
}

/// An import whose id is an existing theme's in another case writes
/// nothing, even with `replace`: the pair would disable both themes.
#[test]
fn import_refuses_a_case_variant_of_an_existing_theme() {
    let dir = scratch("case");
    let root = dir.path();
    let themes = root.join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    let existing = themes.join("Harbour.conf");
    std::fs::write(&existing, KITTY).unwrap();
    let source = root.join("harbour.conf");
    std::fs::write(&source, KITTY).unwrap();
    for replace in [false, true] {
        let out = install_theme_file(&source, None, replace, Some(&themes));
        assert!(
            matches!(&out, Err(InstallError::Failed(m)) if m.contains("only in case")),
            "replace={replace}: {out:?}"
        );
    }
    let names: Vec<_> = std::fs::read_dir(&themes)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["Harbour.conf"], "nothing was written");
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), KITTY);
    install_theme_file(&source, Some("Lighthouse"), false, Some(&themes)).unwrap();
}

#[test]
fn a_refused_request_reads_and_writes_nothing() {
    let dir = scratch("refused");
    let root = dir.path();
    let themes = root.join("themes");
    for (path, name) in [
        ("https://example.com/x.conf", None),
        ("/nonexistent/x.conf", Some("Default")),
        ("/nonexistent/x.conf", Some("dark:x")),
    ] {
        let out = install_theme_file(Path::new(path), name, false, Some(&themes));
        assert!(
            matches!(out, Err(InstallError::Refused(_))),
            "{path} {name:?}: {out:?}"
        );
    }
    assert!(!themes.exists());
}

/// Publishing must not remove a preexisting staging-name collision;
/// a failed commit must clean only the stage this call created.
#[test]
fn atomic_writes_keep_foreign_stage_entries_and_clean_owned_failures() {
    let owned = scratch("owned");
    let dir = owned.path().join("themes");
    std::fs::create_dir(&dir).unwrap();
    let id = "owned-stage";
    let collision = dir.join(format!(".{id}.conf.import-{}", std::process::id()));
    std::fs::write(&collision, b"keep collision").unwrap();
    let dest = dir.join("ordinary.conf");
    write_atomically(&dir, &dest, id, b"first", false).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"first");
    assert!(matches!(
        write_atomically(&dir, &dest, id, b"refused", false),
        Err(InstallError::Exists(_))
    ));
    assert_eq!(std::fs::read(&dest).unwrap(), b"first");
    write_atomically(&dir, &dest, id, b"replacement", true).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"replacement");

    let directory = dir.join("cannot-replace-directory");
    std::fs::create_dir(&directory).unwrap();
    assert!(write_atomically(&dir, &directory, "failure", b"not published", true).is_err());
    assert!(directory.is_dir());
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    let mut wanted = vec![
        collision.file_name().unwrap().to_os_string(),
        dest.file_name().unwrap().to_os_string(),
        directory.file_name().unwrap().to_os_string(),
    ];
    wanted.sort();
    assert_eq!(
        names, wanted,
        "no owned stage remains and the collision stays"
    );
    assert_eq!(std::fs::read(&collision).unwrap(), b"keep collision");
}
