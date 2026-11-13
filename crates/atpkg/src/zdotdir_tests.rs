// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsString;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use super::{ZshDir, from_env, resolve};
#[cfg(unix)]
use super::{ask_zsh, zshenv_files, zshenv_safe};

fn never() -> Result<PathBuf, String> {
    panic!("zsh was asked where its answer was already known")
}

/// A `ZDOTDIR` this process's environment names at or under `$HOME` is the answer, and zsh is
/// not asked; empty or relative, it names no folder (the control: with no zshenv file, `$HOME`).
#[test]
fn the_environment_s_absolute_zdotdir_under_home_is_the_answer() {
    let scratch = crate::testdir::dir("atpkg-zdotdir-env-");
    let home = scratch.as_path();
    let zdot = home.join(".config/zsh");
    for (env, want) in [
        (Some(zdot.as_os_str()), ZshDir::At(zdot.clone())),
        (Some(home.as_os_str()), ZshDir::Home),
        (Some(std::ffi::OsStr::new("")), ZshDir::Home),
        (Some(std::ffi::OsStr::new("dots")), ZshDir::Home),
        (None, ZshDir::Home),
    ] {
        let env = from_env(home, None, env.map(OsString::from));
        assert_eq!(resolve(home, env.clone(), &[], never), want, "{env:?}");
    }
}

/// A `ZDOTDIR` outside `$HOME` in this process's environment is the account's when `$HOME` is
/// the account's own home, and another home's when `$HOME` was pointed elsewhere: a scratch
/// install or a test's home inherits the shell's, and taking it would wire the real home's
/// `.zshrc` from a scratch run. zsh's own answer decides there instead.
#[test]
fn an_inherited_zdotdir_outside_a_redirected_home_is_not_this_home_s() {
    let root = crate::testdir::dir("atpkg-zdotdir-explicit-");
    let account = root.join("Users/someone");
    let scratch = root.join("scratch/home");
    let outside = root.join("opt/dots/zsh");
    let real = || Some(account.join(".config/zsh").into_os_string());
    assert_eq!(from_env(&scratch, Some(&account), real()), None);
    assert_eq!(
        from_env(&scratch, None, real()),
        None,
        "no account record: not taken"
    );
    assert_eq!(
        from_env(
            &account,
            Some(&account),
            Some(outside.clone().into_os_string())
        ),
        Some(outside.clone()),
        "the control: in the account's own home a ZDOTDIR anywhere is its own"
    );
    assert_eq!(
        from_env(
            &scratch,
            Some(&account),
            Some(scratch.join("zdot").into_os_string())
        ),
        Some(scratch.join("zdot")),
        "under the redirected home it is that home's"
    );
    assert_eq!(
        resolve(&scratch, None, &[], never),
        ZshDir::Home,
        "no zshenv file sets one for this home"
    );
    let own = root.join("own");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join(".zshenv"), "").unwrap();
    assert_eq!(
        resolve(&own, None, &[own.join(".zshenv")], || Ok(outside.clone())),
        ZshDir::At(outside),
        "zsh's answer, from this home's zshenv, may name a folder outside it"
    );
}

/// Inherited environment paths cannot make a redirected-home rc pass reach another scratch
/// home, whether the spelling escapes through `..` or an existing descendant symlink.
#[cfg(unix)]
#[test]
fn a_redirected_home_never_wires_an_inherited_path_that_escapes_physically() {
    use crate::hooks::{RcHome, RcWiring, ensure_rc_sources_hooks};
    let root = crate::testdir::dir("atpkg-zdotdir-physical-");
    let mut failures = Vec::new();
    for kind in [
        "outside",
        "dotdot",
        "symlink",
        "symlink-missing",
        "relative-link",
        "link-dotdot",
        "missing-dotdot-link",
        "link-chain",
        "link-cycle",
    ] {
        let base = root.join(kind);
        let home = base.join("home");
        let account = base.join("account");
        let outside = base.join("outside");
        for dir in [&home, &account, &outside] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let sentinel = "# another scratch home's rc must remain unchanged\n";
        let outside_rc = outside.join(".zshrc");
        std::fs::write(&outside_rc, sentinel).unwrap();
        let selected = match kind {
            "outside" => outside.clone(),
            "dotdot" => home.join("../outside"),
            "symlink" | "symlink-missing" => {
                let link = home.join("link");
                std::os::unix::fs::symlink(&outside, &link).unwrap();
                if kind == "symlink" {
                    link
                } else {
                    link.join("missing/nested")
                }
            }
            "relative-link" => {
                let link = home.join("relative");
                std::os::unix::fs::symlink("../outside", &link).unwrap();
                link
            }
            "link-dotdot" => {
                std::os::unix::fs::symlink(&outside, home.join("link")).unwrap();
                home.join("link/../home")
            }
            "missing-dotdot-link" => {
                std::os::unix::fs::symlink(&outside, home.join("link")).unwrap();
                home.join("missing/../link")
            }
            "link-chain" => {
                std::os::unix::fs::symlink("second", home.join("first")).unwrap();
                std::os::unix::fs::symlink(&outside, home.join("second")).unwrap();
                home.join("first")
            }
            "link-cycle" => {
                std::os::unix::fs::symlink("second", home.join("first")).unwrap();
                std::os::unix::fs::symlink("first", home.join("second")).unwrap();
                home.join("first")
            }
            _ => unreachable!(),
        };
        let env = from_env(&home, Some(&account), Some(selected.into_os_string()));
        let accepted = env.is_some();
        let zsh = resolve(&home, env, &[], never);
        let rcs = RcHome {
            home: home.clone(),
            zsh,
        };
        let outcomes = ensure_rc_sources_hooks(&rcs, RcWiring::HonorOptOut, Some(".zshrc"));
        let changed = std::fs::read_to_string(&outside_rc).unwrap() != sentinel
            || outside.join("missing/nested/.zshrc").exists();
        if accepted || changed {
            failures.push((kind, accepted, changed, outcomes));
        }
    }
    assert!(
        failures.is_empty(),
        "redirected HOME accepted/wired another scratch tree: {failures:?}"
    );
}

#[cfg(unix)]
#[test]
fn contained_missing_paths_and_explicit_zsh_choices_still_work() {
    use crate::hooks::{RcHome, RcWiring, ensure_rc_sources_hooks};
    let root = crate::testdir::dir("atpkg-zdotdir-contained-");
    let home = root.join("home");
    let account = root.join("account");
    let outside = root.join("outside");
    for dir in [&home, &account, &outside, &home.join("existing")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::os::unix::fs::symlink("existing", home.join("inside-link")).unwrap();
    std::os::unix::fs::symlink("missing/target", home.join("missing-link")).unwrap();
    for selected in [
        &home,
        &home.join("new/nested"),
        &home.join("new/../existing"),
        &home.join("inside-link"),
        &home.join("missing-link/child"),
    ] {
        assert_eq!(
            from_env(&home, Some(&account), Some(selected.as_os_str().to_owned())),
            Some(selected.to_path_buf()),
            "{selected:?}"
        );
    }
    assert_eq!(
        from_env(&home, Some(&home), Some(outside.clone().into_os_string())),
        Some(outside.clone()),
        "the normal account may choose a folder anywhere"
    );
    std::fs::write(home.join(".zshenv"), "").unwrap();
    assert_eq!(
        resolve(&home, None, &[home.join(".zshenv")], || Ok(outside.clone())),
        ZshDir::At(outside),
        "this home's explicit zshenv answer is not inherited environment"
    );
    let sentinel = "# HOME rc is not selected while zsh uses a contained symlink\n";
    std::fs::write(home.join(".zshrc"), sentinel).unwrap();
    std::fs::write(home.join("existing/.zshrc"), "# selected rc\n").unwrap();
    let selected = home.join("inside-link");
    let env = from_env(&home, Some(&account), Some(selected.into_os_string()));
    let rcs = RcHome {
        home: home.clone(),
        zsh: resolve(&home, env, &[], never),
    };
    let _ = ensure_rc_sources_hooks(&rcs, RcWiring::HonorOptOut, Some(".zshrc"));
    assert!(
        std::fs::read_to_string(home.join("existing/.zshrc"))
            .unwrap()
            .contains(crate::hooks::RC_BEGIN)
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".zshrc")).unwrap(),
        sentinel
    );
}

/// With no zshenv file nothing sets `ZDOTDIR`, so `$HOME` is zsh's folder and zsh is not run;
/// with one, zsh's answer decides: a folder, nothing (`$HOME`), a relative path or no answer
/// (unknown, never a guess).
#[test]
fn zsh_s_own_answer_decides_once_a_zshenv_file_exists() {
    let scratch = crate::testdir::dir("atpkg-zdotdir-answers-");
    let home = scratch.as_path();
    assert_eq!(resolve(home, None, &[], never), ZshDir::Home);
    let zshenv = [home.join(".zshenv")];
    std::fs::write(&zshenv[0], "").unwrap();
    assert_eq!(
        resolve(home, None, &zshenv, || Ok(home.join("zdot"))),
        ZshDir::At(home.join("zdot"))
    );
    assert_eq!(
        resolve(home, None, &zshenv, || Ok(PathBuf::new())),
        ZshDir::Home
    );
    assert_eq!(
        resolve(home, None, &zshenv, || Ok(home.to_path_buf())),
        ZshDir::Home
    );
    assert!(
        matches!(
            resolve(home, None, &zshenv, || Ok(PathBuf::from("zdot"))),
            ZshDir::Unknown(why) if why.contains("relative")
        ),
        "a relative ZDOTDIR names no folder"
    );
    assert_eq!(
        resolve(home, None, &zshenv, || Err("zsh hung".to_owned())),
        ZshDir::Unknown("zsh hung".to_owned())
    );
}

#[cfg(unix)]
#[test]
fn an_uninspectable_zshenv_is_unknown_rather_than_an_absent_startup_file() {
    let scratch = crate::testdir::dir("atpkg-zdotdir-inventory-error-");
    let home = scratch.join("home");
    std::fs::create_dir(&home).unwrap();
    let file = home.join(".zshenv");
    assert!(!zshenv_files(&home).contains(&file));

    // An ancestor cycle makes lstat fail even though the startup path is not absent.
    // This exercises a real discovery failure without depending on the runner's UID.
    std::fs::remove_dir(&home).unwrap();
    std::os::unix::fs::symlink("home", &home).unwrap();
    let error = std::fs::symlink_metadata(&file).unwrap_err();
    assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    let files = zshenv_files(&home);
    assert!(files.contains(&file), "discovery hid {error}: {files:?}");
    let answer = resolve(&home, None, &files, never);
    assert!(matches!(answer, ZshDir::Unknown(_)), "{answer:?}");
    assert_eq!(
        resolve(&home, Some(home.clone()), &files, never),
        ZshDir::Home,
        "an admitted environment answer still needs no startup-file inspection"
    );
}

/// A zshenv file that resolves under a folder macOS guards with a consent dialog is not run
/// (no atpkg pass opens one), so the folder is unknown; the control, a zshenv linked from an
/// ordinary folder, is asked.
#[cfg(unix)]
#[test]
fn a_zshenv_under_a_guarded_folder_is_never_run() {
    let scratch = crate::testdir::dir("atpkg-zdotdir-fence-");
    let home = scratch.as_path();
    std::fs::create_dir_all(home.join("Documents/dots")).unwrap();
    std::fs::create_dir_all(home.join("dots")).unwrap();
    std::fs::write(home.join("Documents/dots/zshenv"), "").unwrap();
    std::fs::write(home.join("dots/zshenv"), "").unwrap();
    let link = home.join(".zshenv");

    std::os::unix::fs::symlink(home.join("Documents/dots/zshenv"), &link).unwrap();
    assert_eq!(zshenv_files(home).last(), Some(&link));
    let fenced = resolve(home, None, std::slice::from_ref(&link), never);
    assert!(
        matches!(&fenced, ZshDir::Unknown(why) if why.contains("consent")),
        "{fenced:?}"
    );

    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(home.join("dots/zshenv"), &link).unwrap();
    let asked = std::cell::Cell::new(false);
    let answer = resolve(home, None, &[link], || {
        asked.set(true);
        Ok(PathBuf::new())
    });
    assert!(asked.get(), "an ordinary zshenv is run");
    assert_eq!(answer, ZshDir::Home);
}

/// Denying metadata under a simulated protected root witnesses that link text is refused
/// before the target is queried, including an ancestor link followed by `..`.
#[cfg(unix)]
#[test]
fn protected_zshenv_targets_are_rejected_before_a_filesystem_query() {
    use std::cell::{Cell, RefCell};
    let scratch = crate::testdir::dir("atpkg-zshenv-no-target-query-");
    let home = scratch.join("home");
    let protected = home.join("Documents");
    std::fs::create_dir_all(protected.join("dots")).unwrap();
    std::fs::create_dir_all(home.join("ordinary")).unwrap();
    std::fs::write(
        protected.join("dots/.zshenv"),
        "# protected scratch sentinel\n",
    )
    .unwrap();
    std::fs::write(home.join("ordinary/.zshenv"), "").unwrap();
    std::os::unix::fs::symlink(&protected, home.join("indirect")).unwrap();
    let direct = home.join(".zshenv");
    std::os::unix::fs::symlink(protected.join("dots/.zshenv"), &direct).unwrap();
    for file in [
        &direct,
        &home.join("indirect/dots/.zshenv"),
        &home.join("indirect/../ordinary/.zshenv"),
    ] {
        let queried = RefCell::new(Vec::new());
        let denied = Cell::new(false);
        let metadata = |path: &Path| {
            queried.borrow_mut().push(path.to_path_buf());
            if path.starts_with(&protected) {
                denied.set(true);
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                std::fs::symlink_metadata(path)
            }
        };
        let result = zshenv_safe(&home, file, metadata, |path| {
            assert!(
                !path.starts_with(&protected),
                "protected link target queried"
            );
            std::fs::read_link(path)
        });
        assert!(
            matches!(result, Err(why) if why.contains("consent")),
            "{file:?}"
        );
        assert!(
            !denied.get(),
            "the denied target metadata callback was reached"
        );
        assert!(
            queried
                .borrow()
                .iter()
                .all(|path| !path.starts_with(&protected))
        );
        // Negative control: the same injected boundary really denies a target query.
        assert!(metadata(&protected.join("dots/.zshenv")).is_err());
        assert!(denied.get());
    }
    assert_eq!(
        std::fs::read_to_string(protected.join("dots/.zshenv")).unwrap(),
        "# protected scratch sentinel\n"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn mac_alias_zshenv_targets_are_refused_before_metadata() {
    use std::cell::Cell;
    let scratch = crate::testdir::dir("atpkg-zshenv-mac-alias-");
    let root = std::fs::canonicalize(&scratch).unwrap();
    let mut failures = Vec::new();
    for (home_name, alias_name, real_root, alias_root) in [
        ("home0", "home0", "Documents", "documents"),
        ("home1", "HOME1", "Documents", "Documentſ"),
        (
            "Åhome2",
            "a\u{30a}HOME2",
            "Library/CloudStorage",
            "LIBRARY/CLOUDSTORAGE",
        ),
        (
            "Åhome3",
            "åHOME3",
            "Library/Group Containers",
            "library/group containers",
        ),
        ("ßhome4", "SSHOME4", "Documents", "DOCUMENTS"),
    ] {
        let home = root.join(home_name);
        let target = home.join(real_root).join(".zshenv");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "# owned alias sentinel\n").unwrap();
        let fence = root.join(alias_name).join(alias_root);
        let link = home.join(".zshenv");
        std::os::unix::fs::symlink(fence.join(".zshenv"), &link).unwrap();
        let queried = Cell::new(false);
        let metadata = |path: &Path| {
            if path.starts_with(&fence) {
                queried.set(true);
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                std::fs::symlink_metadata(path)
            }
        };
        assert!(metadata(&fence).is_err(), "the refusal witness is active");
        queried.set(false);
        let result = zshenv_safe(&home, &link, metadata, |path| std::fs::read_link(path));
        if queried.get() || !result.is_err_and(|why| why.contains("consent")) {
            failures.push((home_name, alias_name, alias_root, queried.get()));
        }
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "# owned alias sentinel\n"
        );
    }
    assert!(
        failures.is_empty(),
        "alias targets queried or not refused: {failures:?}"
    );
}

#[cfg(unix)]
#[test]
fn zshenv_checks_allow_clear_links_and_refuse_unknown_or_nonregular_paths() {
    let scratch = crate::testdir::dir("atpkg-zshenv-path-kinds-");
    let home = scratch.join("home");
    let outside = scratch.join("ordinary-outside");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let target = outside.join("zshenv");
    std::fs::write(&target, "").unwrap();
    let file = home.join(".zshenv");
    std::os::unix::fs::symlink(&target, &file).unwrap();
    let asked = std::cell::Cell::new(false);
    assert_eq!(
        resolve(&home, None, std::slice::from_ref(&file), || {
            asked.set(true);
            Ok(PathBuf::new())
        }),
        ZshDir::Home
    );
    assert!(
        asked.get(),
        "a clear outside-home zshenv link is still asked"
    );
    std::fs::remove_file(&file).unwrap();
    for kind in ["missing", "directory", "dangling", "cycle"] {
        match kind {
            "missing" => {}
            "directory" => std::fs::create_dir(&file).unwrap(),
            "dangling" => std::os::unix::fs::symlink(home.join("absent"), &file).unwrap(),
            "cycle" => std::os::unix::fs::symlink(".zshenv", &file).unwrap(),
            _ => unreachable!(),
        }
        assert!(
            matches!(
                resolve(&home, None, std::slice::from_ref(&file), never),
                ZshDir::Unknown(_)
            ),
            "{kind}"
        );
        if kind == "directory" {
            std::fs::remove_dir(&file).unwrap();
        } else if kind != "missing" {
            std::fs::remove_file(&file).unwrap();
        }
    }
}

/// The real zsh, in a scratch home with no `ZDOTDIR` in its environment, reports what its
/// zshenv file sets and nothing a later startup file does (`zsh -c` reads only the zshenv
/// files), past whatever the file prints; one that ends zsh first gives no answer.
#[cfg(unix)]
#[test]
fn zsh_reports_the_zdotdir_its_zshenv_sets() {
    let zsh = aterm_uds::host::need_tool("zsh");
    let zsh = zsh.as_path();
    let scratch = crate::testdir::dir("atpkg-zdotdir-zsh-");
    let home = scratch.as_path();
    std::fs::write(
        home.join(".zprofile"),
        "export ZDOTDIR=\"$HOME/from-zprofile\"\n",
    )
    .unwrap();
    std::fs::write(home.join(".zshrc"), "export ZDOTDIR=\"$HOME/from-zshrc\"\n").unwrap();

    assert_eq!(
        ask_zsh(zsh, home).as_deref(),
        Ok(Path::new("")),
        "the control: a home with no zshenv sets no ZDOTDIR"
    );

    std::fs::write(
        home.join(".zshenv"),
        "echo 'atpkg-zdotdir:/printed/before/the/answer'\n\
         builtin printf 'startup\\0chatter-without-newline'\n\
         TRAPEXIT() { builtin printf '\\0exit-chatter'; }\n\
         export ZDOTDIR=\"$HOME/zdot\"\n",
    )
    .unwrap();
    let want = home.join("zdot");
    assert_eq!(ask_zsh(zsh, home).as_deref(), Ok(want.as_path()));

    std::fs::write(
        home.join(".zshenv"),
        "echo 'atpkg-zdotdir:/startup-output'\nexit 0\n",
    )
    .unwrap();
    assert!(
        matches!(ask_zsh(zsh, home), Err(why) if why.contains("printed no ZDOTDIR")),
        "startup output is no answer from the probe command"
    );
    std::fs::write(home.join(".zshenv"), "exit 3\n").unwrap();
    let ended = ask_zsh(zsh, home);
    assert!(
        matches!(&ended, Err(why) if why.contains("printed no ZDOTDIR")),
        "{ended:?}"
    );
}

/// A zshenv-selected Unix pathname is wired byte for byte where the filesystem admits it.
/// An unrepresentable byte pathname must not select an existing replacement-name rc either.
#[cfg(unix)]
#[test]
fn zsh_answers_preserve_path_bytes_when_wiring_the_rc() {
    use crate::hooks::{RcHome, RcOutcome, RcWiring, ensure_rc_sources_hooks};
    use std::os::unix::ffi::OsStringExt as _;

    let zsh = aterm_uds::host::need_tool("zsh");
    let zsh = zsh.as_path();
    let scratch = crate::testdir::dir("atpkg-zdotdir-bytes-");
    let cases: [(&str, &[u8], &[u8], &str); 5] = [
        ("ordinary", b"dots", b"wrong-dots", "'dots'"),
        ("newline", b"dots\nsuffix", b"dots", "$'dots\\nsuffix'"),
        ("trailing-newline", b"dots\n", b"dots", "$'dots\\n'"),
        ("trailing-cr", b"dots\r", b"dots", "$'dots\\r'"),
        (
            "non-utf8",
            b"dots-\xff",
            b"dots-\xef\xbf\xbd",
            "$'dots-\\xff'",
        ),
    ];
    let mut failures = Vec::new();
    for (name, raw, wrong, spelling) in cases {
        let home = scratch.join(name);
        let target = home.join(OsString::from_vec(raw.to_vec()));
        let mistaken = home.join(OsString::from_vec(wrong.to_vec()));
        std::fs::create_dir_all(&home).unwrap();
        let target_supported = match std::fs::create_dir_all(&target) {
            Ok(()) => true,
            Err(error) if error.raw_os_error() == Some(libc::EILSEQ) => false,
            Err(error) => panic!("cannot create scratch target {target:?}: {error}"),
        };
        std::fs::create_dir_all(&mistaken).unwrap();
        let target_before = "# selected scratch rc\n";
        let sentinel = "# another scratch rc must remain unchanged\n";
        if target_supported {
            std::fs::write(target.join(".zshrc"), target_before).unwrap();
        }
        std::fs::write(mistaken.join(".zshrc"), sentinel).unwrap();
        let zshenv = home.join(".zshenv");
        std::fs::write(
            &zshenv,
            format!("print -r -- 'startup chatter'\nZDOTDIR=\"$HOME\"/{spelling}\n"),
        )
        .unwrap();
        let dir = resolve(&home, None, &[zshenv], || ask_zsh(zsh, &home));
        let exact = dir == ZshDir::At(target.clone());
        let outcomes = ensure_rc_sources_hooks(
            &RcHome {
                home: home.clone(),
                zsh: dir.clone(),
            },
            RcWiring::Rewire,
            Some(".zshrc"),
        );
        let untouched = std::fs::read(mistaken.join(".zshrc")).unwrap() == sentinel.as_bytes();
        let wired = !target_supported
            || std::fs::read(target.join(".zshrc")).unwrap() != target_before.as_bytes();
        // An rc whose path no line of the wiring ledger can hold is held, untouched, and said so.
        let ledgered = !raw.iter().any(|byte| matches!(byte, b'\n' | b'\r'));
        let done = if ledgered {
            wired
        } else {
            !wired && outcomes == [(".zshrc", RcOutcome::Unreadable)]
        };
        if !exact || !untouched || !done {
            failures.push((name, dir, exact, untouched, wired, outcomes));
        }
    }
    assert!(
        failures.is_empty(),
        "zsh reply decoding selected another scratch rc: {failures:?}"
    );
}

/// A NUL is not a pathname byte: the probe must refuse it without treating its first part as
/// a valid existing rc directory. zsh's own scalar strings can still contain that byte.
#[cfg(unix)]
#[test]
fn zsh_answers_refuse_embedded_nul_before_wiring_an_rc() {
    use crate::hooks::{RcHome, RcWiring, ensure_rc_sources_hooks};

    let zsh = aterm_uds::host::need_tool("zsh");
    let zsh = zsh.as_path();
    let home = crate::testdir::dir("atpkg-zdotdir-nul-");
    let prefix = home.join("dots");
    std::fs::create_dir(&prefix).unwrap();
    let sentinel = "# the prefix of an invalid pathname is not selected\n";
    std::fs::write(prefix.join(".zshrc"), sentinel).unwrap();
    let zshenv = home.join(".zshenv");
    std::fs::write(&zshenv, "ZDOTDIR=\"$HOME\"/$'dots\\0suffix'\n").unwrap();
    let answer = ask_zsh(zsh, home.as_path());
    assert!(
        matches!(&answer, Err(why) if why.contains("NUL")),
        "{answer:?}"
    );
    let dir = resolve(home.as_path(), None, &[zshenv], || answer);
    assert!(matches!(&dir, ZshDir::Unknown(_)), "{dir:?}");
    let _ = ensure_rc_sources_hooks(
        &RcHome {
            home: home.as_path().to_path_buf(),
            zsh: dir,
        },
        RcWiring::Rewire,
        Some(".zshrc"),
    );
    assert_eq!(
        std::fs::read(prefix.join(".zshrc")).unwrap(),
        sentinel.as_bytes()
    );
}
