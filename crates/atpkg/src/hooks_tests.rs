// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// The four files `write_hooks` lays are exactly what `hook_for_shell` names per shell,
/// and `hook_file` answers only for a hook that exists as a regular file.
#[cfg(unix)]
#[test]
fn hook_for_shell_and_hook_file_read_the_files_write_hooks_lays() {
    let home = tmp("hookfile");
    assert_eq!(hook_for_shell("zsh"), Some("00-atpkg.zsh"));
    assert_eq!(hook_for_shell("bash"), Some("00-atpkg.bash"));
    assert_eq!(hook_for_shell("sh"), None);
    assert_eq!(hook_for_shell("fish"), Some("00-atpkg.fish"));
    assert_eq!(hook_for_shell("pwsh"), Some("00-atpkg.ps1"));
    assert_eq!(hook_for_shell("powershell"), Some("00-atpkg.ps1"));
    assert_eq!(hook_for_shell("nu"), None);
    for sh in ["zsh", "bash", "fish", "pwsh", "nu"] {
        assert_eq!(hook_file(&home, sh), None, "{sh}");
    }
    let shell_d = home.join(".aterm/shell.d");
    fs::create_dir_all(&shell_d).unwrap();
    let written = write_hooks(
        &shell_d,
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
    )
    .unwrap();
    for sh in ["zsh", "bash", "fish", "pwsh"] {
        let (path, hook) = hook_file(&home, sh).unwrap_or_else(|| panic!("{sh}"));
        assert_eq!(path, shell_d.join(hook));
        assert!(
            written.contains(&hook.to_string()),
            "{hook} is a file write_hooks lays"
        );
    }
    assert_eq!(hook_file(&home, "nu"), None);
    assert_eq!(hook_file(&home, "sh"), None);
    fs::remove_file(shell_d.join("00-atpkg.zsh")).unwrap();
    std::os::unix::fs::symlink(shell_d.join("00-atpkg.bash"), shell_d.join("00-atpkg.zsh"))
        .unwrap();
    assert_eq!(hook_file(&home, "zsh"), None);
    let _ = fs::remove_dir_all(&home);
}

fn tmp(label: &str) -> crate::testdir::Dir {
    let base = std::env::temp_dir();
    let base = if base
        .canonicalize()
        .is_ok_and(|resolved| resolved.starts_with("/Volumes"))
    {
        PathBuf::from("/tmp")
    } else {
        base
    };
    crate::testdir::dir_in(&base, &format!("atpkg-hooks-{label}-"))
}

/// The rc block goes in exactly once, sources the hook, and is removable.
#[cfg(unix)]
#[test]
fn rc_wiring_is_idempotent_and_only_touches_existing_rc_files() {
    use std::os::unix::fs::MetadataExt as _;
    let home = tmp("rcwire");
    let zshrc = home.join(".zshrc");
    std::fs::write(&zshrc, "# pre-existing user content\nexport FOO=1\n").unwrap();

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    let once = std::fs::read_to_string(&zshrc).unwrap();
    let ino_once = fs::metadata(&zshrc).unwrap().ino();
    assert!(
        once.contains(RC_BEGIN) && once.contains(RC_END),
        "block written"
    );
    assert!(
        once.contains(".aterm/shell.d/00-atpkg.zsh"),
        "it must SOURCE the generated hook, not inline a PATH that can go stale"
    );
    assert!(
        once.starts_with("# pre-existing user content"),
        "the user's own content must be preserved, and preserved FIRST"
    );
    assert!(!home.join(".bashrc").exists(), "no rc was invented");

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    let twice = std::fs::read_to_string(&zshrc).unwrap();
    assert_eq!(twice, once, "a second pass must be a no-op");
    assert_eq!(twice.matches(RC_BEGIN).count(), 1, "exactly one block");
    assert_eq!(
        fs::metadata(&zshrc).unwrap().ino(),
        ino_once,
        "the rc was not rewritten: same inode, no temp+rename"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The opt-out the block documents lasts: a recorded rc without the block is left alone
/// by every unattended pass, `repair` ([`refresh_rewiring_rc`]) writes it back, and after
/// that the block can be deleted for good again.
#[cfg(unix)]
#[test]
fn a_deleted_rc_block_stays_deleted_and_repair_is_the_way_back() {
    let home = tmp("rcoptout");
    let zshrc = home.join(".zshrc");
    let user = "# pre-existing user content\nexport FOO=1\n";
    fs::write(&zshrc, user).unwrap();

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    assert!(
        fs::read_to_string(&zshrc).unwrap().contains(RC_BEGIN),
        "wired on the first pass, as before"
    );
    let real = fs::canonicalize(&zshrc).unwrap();
    assert!(
        read_rc_ledger(&home)
            .unwrap()
            .contains(real.to_string_lossy().as_ref()),
        "the wiring is recorded: {:?}",
        read_rc_ledger(&home)
    );
    assert_eq!(
        fs::metadata(rc_ledger_path(&home))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the ledger is private, like every other file this module writes"
    );

    fs::write(&zshrc, user).unwrap();
    for pass in 0..3 {
        ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
        assert_eq!(
            fs::read_to_string(&zshrc).unwrap(),
            user,
            "pass {pass}: an unattended pass must not undo the user's own edit"
        );
    }

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::Rewire, None);
    assert!(
        fs::read_to_string(&zshrc).unwrap().contains(RC_BEGIN),
        "repair puts the block back"
    );

    fs::write(&zshrc, user).unwrap();
    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    assert_eq!(
        fs::read_to_string(&zshrc).unwrap(),
        user,
        "a second opt-out is honoured just like the first"
    );

    let _ = fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn a_large_rc_history_keeps_a_removed_hook_off_and_preserves_the_ledger() {
    let home = tmp("large-rc-history");
    let zshrc = home.join(".zshrc");
    let user = "# user kept this shell config\nexport MY_SETTING=1\n";
    fs::write(&zshrc, user).unwrap();
    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    assert!(
        fs::read_to_string(&zshrc).unwrap().contains(RC_BEGIN),
        "ordinary wiring"
    );
    let real = fs::canonicalize(&zshrc).unwrap();
    assert!(
        read_rc_ledger(&home)
            .unwrap()
            .contains(real.to_string_lossy().as_ref())
    );
    fs::write(&zshrc, user).unwrap();
    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    assert_eq!(
        fs::read_to_string(&zshrc).unwrap(),
        user,
        "ordinary removed-block control"
    );

    let mut history = read_rc_ledger(&home).unwrap();
    for n in 0..300 {
        let path = home.join(format!("old-rc-{n:03}-{}", "x".repeat(200)));
        fs::write(&path, b"inert historical shell config\n").unwrap();
        let path = fs::canonicalize(path).unwrap();
        record_rc_wired(&home, &path, &mut history);
    }
    let before = fs::read(rc_ledger_path(&home)).unwrap();
    assert!(
        before.len() > 64 * 1024,
        "actual producer's aggregate history"
    );
    assert!(history.contains(real.to_string_lossy().as_ref()));
    let out = ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    assert_eq!(
        fs::read_to_string(&zshrc).unwrap(),
        user,
        "an unattended pass must honor the removed block, whatever the history size: {out:?}"
    );
    assert_eq!(
        fs::read(rc_ledger_path(&home)).unwrap(),
        before,
        "no forgotten history"
    );
}

#[cfg(unix)]
#[test]
fn unreadable_rc_history_holds_wiring_without_overwriting_any_evidence() {
    for case in ["utf8", "overlong", "directory"] {
        let home = tmp("refused-rc-history");
        let zshrc = home.join(".zshrc");
        let user = "# the person's shell config\nexport MY_SETTING=1\n";
        fs::write(&zshrc, user).unwrap();
        let path = rc_ledger_path(&home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = match case {
            "utf8" => vec![0xff],
            "overlong" => vec![b'x'; RC_HISTORY_LINE_BYTES + 1],
            _ => Vec::new(),
        };
        if case == "directory" {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, &bytes).unwrap();
        }
        assert!(read_rc_ledger(&home).is_err(), "{case}");
        let rcs = RcHome::at(&home);
        for mode in [RcWiring::HonorOptOut, RcWiring::Rewire] {
            let out = ensure_rc_sources_hooks(&rcs, mode, Some(".bash_profile"));
            assert!(
                out.iter()
                    .all(|(_, outcome)| *outcome == RcOutcome::Unreadable),
                "{case}: {out:?}"
            );
            assert_eq!(fs::read_to_string(&zshrc).unwrap(), user, "{case}");
            assert!(
                !home.join(".bash_profile").exists(),
                "{case}: no inferred fresh login rc"
            );
        }
        assert!(
            rc_wiring(&rcs)
                .iter()
                .all(|(_, state)| *state == RcState::HistoryUnreadable)
        );
        assert_eq!(
            absent_login_rc(&rcs, ".bash_profile"),
            Some(RcState::HistoryUnreadable)
        );
        if case == "directory" {
            assert!(path.is_dir());
        } else {
            assert_eq!(fs::read(&path).unwrap(), bytes, "{case}");
        }
    }
}

/// The one file a pass creates (design init-system §4.2): the login shell's absent rc,
/// holding only the block, private, recorded.
#[cfg(unix)]
#[test]
fn the_login_shell_s_absent_rc_is_created_and_no_other_shell_gains_one() {
    let home = tmp("rclogin");
    fs::create_dir_all(home.join(".config/fish")).unwrap();
    let login = login_rc("/bin/zsh", &RcHome::at(&home), None);
    assert_eq!(login, Some(".zshrc"));
    let zshrc = home.join(".zshrc");

    assert_eq!(
        ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, login),
        [(".zshrc", RcOutcome::Created)]
    );
    let body = fs::read_to_string(&zshrc).unwrap();
    assert!(body.starts_with(RC_BEGIN), "only the block: {body:?}");
    assert!(body.contains(".aterm/shell.d/00-atpkg.zsh"), "{body:?}");
    assert_eq!(body.matches(RC_BEGIN).count(), 1);
    assert_eq!(
        fs::metadata(&zshrc).unwrap().permissions().mode() & 0o777,
        0o600,
        "born private"
    );
    let real = fs::canonicalize(&zshrc).unwrap();
    assert!(
        read_rc_ledger(&home)
            .unwrap()
            .contains(real.to_string_lossy().as_ref()),
        "recorded like a wired rc"
    );
    for other in [".bashrc", ".bash_profile", ".config/fish/config.fish"] {
        assert!(
            fs::symlink_metadata(home.join(other)).is_err(),
            "{other} appeared for a shell this account does not log in with"
        );
    }

    assert_eq!(
        ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, login),
        [(".zshrc", RcOutcome::AlreadyWired)]
    );
    assert_eq!(fs::read_to_string(&zshrc).unwrap(), body);

    fs::remove_file(&zshrc).unwrap();
    assert_eq!(
        absent_login_rc(&RcHome::at(&home), ".zshrc"),
        Some(RcState::OptedOut)
    );
    for pass in 0..2 {
        assert_eq!(
            ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, login),
            [(".zshrc", RcOutcome::OptOutKept)],
            "pass {pass}"
        );
        assert!(fs::symlink_metadata(&zshrc).is_err(), "pass {pass}");
    }
    assert_eq!(
        ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::Rewire, login),
        [(".zshrc", RcOutcome::RelaidOverOptOut)]
    );
    assert_eq!(fs::read_to_string(&zshrc).unwrap(), body);
    assert_eq!(
        absent_login_rc(&RcHome::at(&home), ".zshrc"),
        None,
        "it stands again"
    );
}

/// Under a `ZDOTDIR`, zsh's row is `$ZDOTDIR/.zshrc` (round 13: a pass wired `~/.zshrc`,
/// which zsh never reads there): it is the login rc a pass creates, the file it wires and
/// reports, and a standing `~/.zshrc` is not zsh's, so it is left exactly as it was. The
/// control is the same home with zsh's folder at `$HOME`.
#[cfg(unix)]
#[test]
fn a_zdotdir_account_s_zsh_row_is_the_zshrc_zsh_reads() {
    let home = tmp("rczdot");
    let zdot = home.join(".config/zsh");
    fs::create_dir_all(&zdot).unwrap();
    let user = "# the person's own, from before ZDOTDIR\n";
    fs::write(home.join(".zshrc"), user).unwrap();
    let rcs = RcHome {
        home: home.to_path_buf(),
        zsh: ZshDir::At(zdot.clone()),
    };
    assert_eq!(rcs.shown(".zshrc"), "~/.config/zsh/.zshrc");
    let login = login_rc("/bin/zsh", &rcs, None);
    assert_eq!(login, Some(".zshrc"));
    assert_eq!(absent_login_rc(&rcs, ".zshrc"), Some(RcState::Unwired));

    assert_eq!(
        ensure_rc_sources_hooks(&rcs, RcWiring::HonorOptOut, login),
        [(".zshrc", RcOutcome::Created)]
    );
    let made = fs::read_to_string(zdot.join(".zshrc")).unwrap();
    assert!(made.starts_with(RC_BEGIN), "{made:?}");
    assert!(made.contains(".aterm/shell.d/00-atpkg.zsh"), "{made:?}");
    assert_eq!(
        fs::read_to_string(home.join(".zshrc")).unwrap(),
        user,
        "~/.zshrc is not zsh's under a ZDOTDIR"
    );
    assert_eq!(rc_wiring(&rcs), [(".zshrc", RcState::Wired)]);
    assert_eq!(
        ensure_rc_sources_hooks(&rcs, RcWiring::HonorOptOut, login),
        [(".zshrc", RcOutcome::AlreadyWired)]
    );

    let control = tmp("rczdotctl");
    fs::create_dir_all(control.join(".config/zsh")).unwrap();
    fs::write(control.join(".zshrc"), user).unwrap();
    assert_eq!(
        ensure_rc_sources_hooks(&RcHome::at(&control), RcWiring::HonorOptOut, login),
        [(".zshrc", RcOutcome::Appended)]
    );
    assert!(!control.join(".config/zsh/.zshrc").exists());
}

/// While zsh's folder is unknown no `.zshrc` is opened, made or reported as wired: a pass
/// says so, the login rc election names none for zsh, and a standing `~/.zshrc` is left as
/// it was; the other rows are wired as ever.
#[cfg(unix)]
#[test]
fn an_unknown_zsh_folder_opens_no_zshrc() {
    let home = tmp("rczunk");
    let user = "export FOO=1\n";
    fs::write(home.join(".zshrc"), user).unwrap();
    fs::write(home.join(".bashrc"), user).unwrap();
    let rcs = RcHome {
        home: home.to_path_buf(),
        zsh: ZshDir::Unknown("zsh hung".to_owned()),
    };
    assert_eq!(login_rc("/bin/zsh", &rcs, None), None);
    assert_eq!(absent_login_rc(&rcs, ".zshrc"), None);
    assert_eq!(rcs.shown(".zshrc"), "zsh's .zshrc");
    let outcomes = ensure_rc_sources_hooks(&rcs, RcWiring::Rewire, Some(".zshrc"));
    assert!(
        outcomes.contains(&(".zshrc", RcOutcome::ZshFolderUnknown))
            && outcomes.contains(&(".bashrc", RcOutcome::Appended)),
        "{outcomes:?}"
    );
    assert_eq!(fs::read_to_string(home.join(".zshrc")).unwrap(), user);
    assert!(
        rc_wiring(&rcs).contains(&(".zshrc", RcState::ZshFolderUnknown)),
        "{:?}",
        rc_wiring(&rcs)
    );
}

/// The login rc is selfsetup's `rc_target` election, and only a file atpkg wires: a
/// shell it does not know, and an election that picks a file no row names, get nothing.
#[cfg(unix)]
#[test]
fn the_login_rc_is_the_rc_target_election_s_and_only_a_row() {
    let home = tmp("rcelect");
    assert_eq!(
        login_rc("/bin/zsh", &RcHome::at(&home), None),
        Some(".zshrc")
    );
    assert_eq!(login_rc("zsh", &RcHome::at(&home), None), Some(".zshrc"));
    assert_eq!(
        login_rc("/opt/homebrew/bin/fish", &RcHome::at(&home), None),
        Some(".config/fish/config.fish")
    );
    assert_eq!(
        login_rc(
            "/opt/homebrew/bin/fish",
            &RcHome::at(&home),
            Some(&home.join(".config"))
        ),
        Some(".config/fish/config.fish")
    );
    assert_eq!(
        login_rc(
            "/opt/homebrew/bin/fish",
            &RcHome::at(&home),
            Some(&home.join("xdg"))
        ),
        None
    );
    for unknown in ["/bin/sh", "/bin/dash", "/usr/bin/nu", "/usr/bin/false", ""] {
        assert_eq!(
            login_rc(unknown, &RcHome::at(&home), None),
            None,
            "{unknown:?}"
        );
    }
    if cfg!(target_os = "macos") {
        assert_eq!(
            login_rc("/bin/bash", &RcHome::at(&home), None),
            Some(".bash_profile")
        );
        fs::write(home.join(".profile"), "").unwrap();
        assert_eq!(login_rc("/bin/bash", &RcHome::at(&home), None), None);
    } else {
        assert_eq!(
            login_rc("/bin/bash", &RcHome::at(&home), None),
            Some(".bashrc")
        );
    }
}

/// The created rc keeps the consent fence: a login rc whose directory resolves under a
/// guarded folder is neither created nor opened, and a missing directory is never made.
#[cfg(unix)]
#[test]
fn a_login_rc_under_a_guarded_folder_or_with_no_directory_is_not_created() {
    let home = tmp("rcloginfence");
    let login = login_rc("/usr/local/bin/fish", &RcHome::at(&home), None);
    assert_eq!(login, Some(".config/fish/config.fish"));
    assert_eq!(
        ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::Rewire, login),
        []
    );
    assert!(fs::symlink_metadata(home.join(".config")).is_err());
    assert_eq!(
        absent_login_rc(&RcHome::at(&home), ".config/fish/config.fish"),
        None
    );

    fs::create_dir_all(home.join("Documents/dotfiles/fish")).unwrap();
    std::os::unix::fs::symlink(home.join("Documents/dotfiles"), home.join(".config")).unwrap();
    assert_eq!(
        ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::Rewire, login),
        [(".config/fish/config.fish", RcOutcome::ConsentFenced)]
    );
    assert!(!home.join("Documents/dotfiles/fish/config.fish").exists());
    assert_eq!(
        absent_login_rc(&RcHome::at(&home), ".config/fish/config.fish"),
        Some(RcState::ConsentFenced)
    );
}

/// A machine wired before the ledger existed opts out on its first deletion: a pass
/// records an rc it finds already wired, rewriting nothing.
#[cfg(unix)]
#[test]
fn an_rc_wired_before_the_ledger_existed_is_recorded_by_the_next_pass() {
    let home = tmp("rcpreledger");
    let bashrc = home.join(".bashrc");
    let user = "export FOO=1\n";
    let hook = home.join(".aterm").join("shell.d").join("00-atpkg.bash");
    let pre = format!(
        "{user}\n{RC_BEGIN}\n[ -f \"{p}\" ] && . \"{p}\"\n{RC_END}\n",
        p = hook.display()
    );
    fs::write(&bashrc, &pre).unwrap();
    assert!(
        !rc_ledger_path(&home).exists(),
        "the pre-2026-09-16 shape: wired, with nothing recorded"
    );

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    assert_eq!(
        fs::read_to_string(&bashrc).unwrap(),
        pre,
        "an already-wired rc is still never rewritten"
    );
    assert!(
        rc_ledger_path(&home).is_file(),
        "the pass records what it found wired"
    );

    fs::write(&bashrc, user).unwrap();
    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    assert_eq!(
        fs::read_to_string(&bashrc).unwrap(),
        user,
        "the first deletion sticks on a machine wired by an older build"
    );

    let _ = fs::remove_dir_all(&home);
}

/// The `shell` a real-shell test here runs, as `aterm_spec::host::test_shell` finds
/// it (the resolver aterm-shell-integration's live tests use too), or `None` with
/// `shell` added to `absent`.
#[cfg(unix)]
fn real_shell<'a>(shell: &'a str, absent: &mut Vec<&'a str>) -> Option<PathBuf> {
    let found = aterm_spec::host::test_shell(shell);
    if found.is_none() {
        assert_ne!(shell, "bash", "bash must be found on a unix host");
        absent.push(shell);
    }
    found
}

/// A HOME with a SPACE still gets the toolchain, and no shell prints an error.
#[cfg(unix)]
#[test]
fn rc_wiring_quotes_the_hook_path_so_a_home_with_a_space_still_sources_it() {
    let root = tmp("rcspace");
    let home = root.join("Jane Doe");
    let shell_d = home.join(".aterm").join("shell.d");
    fs::create_dir_all(&shell_d).unwrap();
    for (rc, hook) in [
        (".zshrc", "00-atpkg.zsh"),
        (".bashrc", "00-atpkg.bash"),
        (".config/fish/config.fish", "00-atpkg.fish"),
    ] {
        let rc_path = home.join(rc);
        fs::create_dir_all(rc_path.parent().unwrap()).unwrap();
        fs::write(&rc_path, "# pre-existing user content\n").unwrap();
        let body = if hook.ends_with(".fish") {
            "set -g ATPKG_RC_TEST sourced\n"
        } else {
            "ATPKG_RC_TEST=sourced\n"
        };
        fs::write(shell_d.join(hook), body).unwrap();
    }

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);

    let line_of = |rc: &str| -> String {
        let body = fs::read_to_string(home.join(rc)).unwrap();
        let (_, after) = body.split_once(RC_BEGIN).expect("block written");
        let (block, _) = after.split_once(RC_END).expect("block closed");
        block
            .lines()
            .find(|l| l.contains("00-atpkg."))
            .unwrap_or_else(|| panic!("{rc}: no source line in the block"))
            .to_owned()
    };

    let mut absent = Vec::new();
    for (shell, args, rc) in [
        ("bash", &["--noprofile", "--norc", "-c"][..], ".bashrc"),
        ("zsh", &["-f", "-c"][..], ".zshrc"),
        (
            "fish",
            &["--no-config", "-c"][..],
            ".config/fish/config.fish",
        ),
    ] {
        let Some(program) = real_shell(shell, &mut absent) else {
            continue;
        };
        let line = line_of(rc);
        let out = aterm_spec::host::shell_command(&program, &home)
            .args(args)
            .arg(format!("{line}\nprintf 'X=%s\\n' \"$ATPKG_RC_TEST\""))
            .output()
            .unwrap_or_else(|error| panic!("{shell}: spawn: {error}"));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && stderr.is_empty(),
            "{shell}: a home with a space must not error at shell start: exit {:?}, stderr={stderr:?}, line={line:?}",
            out.status.code()
        );
        assert_eq!(
            stdout.trim_end(),
            "X=sourced",
            "{shell}: the hook must actually be sourced; line={line:?}"
        );
    }

    let fish = line_of(".config/fish/config.fish");
    let fish_quoted = format!("\"{}\"", shell_d.join("00-atpkg.fish").display());
    assert_eq!(
        fish.matches(fish_quoted.as_str()).count(),
        2,
        "fish: both interpolations must be double-quoted, got {fish:?}"
    );

    let _ = fs::remove_dir_all(&root);
    aterm_spec::host::could_not_run_without(&absent);
}

/// The block's line leaves `$?` = 0 when the hook is missing, replayed in the real shells
/// over a `shell.d` that holds no hook.
#[cfg(unix)]
#[test]
fn the_rc_line_is_status_neutral_when_the_hook_is_missing() {
    let home = tmp("rcstatus");
    for rc in [".zshrc", ".bashrc", ".config/fish/config.fish"] {
        let rc_path = home.join(rc);
        fs::create_dir_all(rc_path.parent().unwrap()).unwrap();
        fs::write(&rc_path, "# pre-existing user content\n").unwrap();
    }
    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);

    let line_of = |rc: &str| -> String {
        let body = fs::read_to_string(home.join(rc)).unwrap();
        let (_, after) = body.split_once(RC_BEGIN).expect("block written");
        let (block, _) = after.split_once(RC_END).expect("block closed");
        block
            .lines()
            .find(|l| l.contains("00-atpkg."))
            .unwrap_or_else(|| panic!("{rc}: no source line in the block"))
            .to_owned()
    };

    let mut absent = Vec::new();
    for (shell, args, rc) in [
        ("bash", &["--noprofile", "--norc", "-c"][..], ".bashrc"),
        ("zsh", &["-f", "-c"][..], ".zshrc"),
        (
            "fish",
            &["--no-config", "-c"][..],
            ".config/fish/config.fish",
        ),
    ] {
        let Some(program) = real_shell(shell, &mut absent) else {
            continue;
        };
        let line = line_of(rc);
        let out = aterm_spec::host::shell_command(&program, &home)
            .args(args)
            .arg(&line)
            .output()
            .unwrap_or_else(|error| panic!("{shell}: spawn: {error}"));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{shell}: an ABSENT hook must still leave $? = 0 — this line is the last \
             thing the rc runs; line={line:?}"
        );
    }

    let fish = line_of(".config/fish/config.fish");
    assert!(
        fish.starts_with("if test -f ") && fish.ends_with("; end"),
        "fish: the line must be the status-neutral `if …; end` form, got {fish:?}"
    );

    let _ = fs::remove_dir_all(&home);
    aterm_spec::host::could_not_run_without(&absent);
}

/// A pass whose hook write failed wires no rc: a block naming a missing hook sources
/// nothing and is never rewritten.
#[cfg(unix)]
#[test]
fn a_failed_hook_write_wires_no_rc_block() {
    let home = tmp("rcnohook");
    let zshrc = home.join(".zshrc");
    let user = "# pre-existing user content\nexport FOO=1\n";
    fs::write(&zshrc, user).unwrap();
    let shell_d = home.join(".aterm").join("shell.d");
    fs::create_dir_all(&shell_d).unwrap();
    let blocked = shell_d.join(format!("{HOOK_BASENAME}.zsh"));
    fs::create_dir_all(&blocked).unwrap();

    refresh_at(
        &RcHome::at(&home),
        Path::new("/opt/atpkg-fixture/bin"),
        Path::new("/opt/atpkg-fixture/agents"),
        Path::new("/opt/atpkg-fixture/reroute"),
        RcWiring::HonorOptOut,
        None,
    );

    assert!(
        !shell_d.join("00-atpkg.zsh").is_file(),
        "the hook write must have failed for this test to mean anything"
    );
    assert_eq!(
        fs::read_to_string(&zshrc).unwrap(),
        user,
        "no block may be wired when the hook it names was never written"
    );

    let _ = fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn rc_wiring_preserves_occupied_temporary_names() {
    for kind in ["file", "symlink", "directory"] {
        let home = tmp("rc-temp-owner");
        let rc = home.join(".bashrc");
        fs::write(&rc, "# existing rc\n").unwrap();
        let target = home.join("another-file");
        fs::write(&target, b"another file's bytes").unwrap();
        let occupied = home.join(format!(".bashrc.atpkg-{}.tmp", std::process::id()));
        match kind {
            "file" => fs::write(&occupied, b"someone else's bytes").unwrap(),
            "symlink" => std::os::unix::fs::symlink(&target, &occupied).unwrap(),
            "directory" => fs::create_dir(&occupied).unwrap(),
            _ => unreachable!(),
        }
        let outcomes = ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
        assert!(
            outcomes.contains(&(".bashrc", RcOutcome::Appended)),
            "{kind}: {outcomes:?}"
        );
        let body = fs::read_to_string(&rc).unwrap();
        assert!(body.starts_with("# existing rc\n"));
        assert_eq!(body.matches(RC_BEGIN).count(), 1);
        assert_eq!(fs::read(&target).unwrap(), b"another file's bytes");
        match kind {
            "file" => assert_eq!(fs::read(&occupied).unwrap(), b"someone else's bytes"),
            "symlink" => assert_eq!(fs::read_link(&occupied).unwrap(), target),
            "directory" => assert!(fs::symlink_metadata(&occupied).unwrap().is_dir()),
            _ => unreachable!(),
        }
        let _ = fs::remove_dir_all(&home);
    }
}

#[cfg(unix)]
#[test]
fn protected_missing_rc_targets_are_fenced_by_wiring_and_reports() {
    let home = tmp("rc-protected-missing");
    let guarded = home.join("Documents/not-created");
    std::os::unix::fs::symlink(guarded.join("bashrc"), home.join(".bashrc")).unwrap();
    let rcs = RcHome {
        home: home.to_path_buf(),
        zsh: ZshDir::At(guarded.clone()),
    };
    let outcomes = ensure_rc_sources_hooks(&rcs, RcWiring::HonorOptOut, Some(".zshrc"));
    let reports = rc_wiring(&rcs);
    for rc in [".bashrc", ".zshrc"] {
        assert!(
            outcomes.contains(&(rc, RcOutcome::ConsentFenced)),
            "{outcomes:?}"
        );
        assert!(
            reports.contains(&(rc, RcState::ConsentFenced)),
            "{reports:?}"
        );
    }
    assert_eq!(
        absent_login_rc(&rcs, ".zshrc"),
        Some(RcState::ConsentFenced)
    );
    assert!(fs::symlink_metadata(home.join("Documents")).is_err());
    assert_eq!(
        fs::read_link(home.join(".bashrc")).unwrap(),
        guarded.join("bashrc")
    );
    let _ = fs::remove_dir_all(&home);
}

/// A symlinked rc (stow, yadm/chezmoi, home-manager) gets the block written through the
/// link and stays a link with its mode; a dangling link is left alone.
#[cfg(unix)]
#[test]
fn rc_wiring_writes_through_a_symlinked_rc_and_keeps_it_a_link_and_its_mode() {
    let home = tmp("rcsymlink");
    let dot = home.join("dotfiles");
    fs::create_dir_all(&dot).unwrap();
    let target = dot.join("zshrc");
    fs::write(&target, "export FOO=1\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let link = home.join(".zshrc");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let gone = dot.join("bashrc-not-checked-out");
    std::os::unix::fs::symlink(&gone, home.join(".bashrc")).unwrap();
    let fish = home.join(".config/fish/config.fish");
    fs::create_dir_all(fish.parent().unwrap()).unwrap();
    fs::write(&fish, "set -x FOO 1\n").unwrap();
    fs::set_permissions(&fish, fs::Permissions::from_mode(0o600)).unwrap();

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the rc must stay a symlink"
    );
    assert_eq!(fs::read_link(&link).unwrap(), target);
    let through = fs::read_to_string(&target).unwrap();
    assert!(
        through.contains(RC_BEGIN),
        "the block lands in the dotfile: {through}"
    );
    assert!(through.starts_with("export FOO=1\n"), "{through}");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600,
        "a 0600 rc must not widen"
    );
    assert!(
        fs::symlink_metadata(home.join(".bashrc"))
            .unwrap()
            .file_type()
            .is_symlink()
            && !gone.exists(),
        "a dangling rc link is left exactly as it was"
    );
    assert!(fs::read_to_string(&fish).unwrap().contains(RC_BEGIN));
    assert_eq!(
        fs::metadata(&fish).unwrap().permissions().mode() & 0o777,
        0o600,
        "a regular 0600 rc must not widen either"
    );

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    assert_eq!(
        fs::read_to_string(&target)
            .unwrap()
            .matches(RC_BEGIN)
            .count(),
        1,
        "exactly one block through the link"
    );
    for dir in [&home, &dot, &fish.parent().unwrap().to_path_buf()] {
        let strays: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("atpkg-") && n.ends_with(".tmp"))
            .collect();
        assert!(
            strays.is_empty(),
            "temp files left in {}: {strays:?}",
            dir.display()
        );
    }
    let _ = fs::remove_dir_all(&home);
}

/// A bash login shell reads `~/.bash_profile`, not `~/.bashrc`: a home with
/// `.bash_profile` gets the block, sourcing the same `00-atpkg.bash`; a home with both
/// gets it in both; and `.bashrc` is never invented beside a lone `.bash_profile`.
#[cfg(unix)]
#[test]
fn rc_wiring_reaches_a_bash_login_shell_through_an_existing_bash_profile() {
    let home = tmp("bashprofile");
    let profile = home.join(".bash_profile");
    fs::write(&profile, "# login-only content\nexport FOO=1").unwrap();

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    let wired = fs::read_to_string(&profile).unwrap();
    assert!(
        wired.contains(RC_BEGIN) && wired.contains(RC_END),
        "block written into .bash_profile: {wired}"
    );
    assert!(
        wired.contains(".aterm/shell.d/00-atpkg.bash"),
        "it sources the bash hook, the same one .bashrc sources: {wired}"
    );
    assert!(
        wired.starts_with("# login-only content\nexport FOO=1\n\n"),
        "the user's content stays first and gains the newline it lacked: {wired}"
    );
    assert!(
        !home.join(".bashrc").exists(),
        ".bashrc must not be invented beside a lone .bash_profile"
    );

    let bashrc = home.join(".bashrc");
    fs::write(&bashrc, "export BAR=1\n").unwrap();
    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);
    for rc in [&profile, &bashrc] {
        let body = fs::read_to_string(rc).unwrap();
        assert_eq!(
            body.matches(RC_BEGIN).count(),
            1,
            "exactly one block in {}",
            rc.display()
        );
        assert!(body.contains("00-atpkg.bash"), "{}", rc.display());
    }
    let _ = fs::remove_dir_all(&home);
}

/// The consent fence: an rc that resolves under a protected root (through a symlinked rc,
/// or a symlinked parent of a regular rc) is left exactly as it was, while an rc beside
/// it that resolves somewhere ordinary is still wired.
#[cfg(unix)]
#[test]
fn rc_wiring_never_opens_an_rc_that_resolves_under_a_protected_folder() {
    let home = tmp("rcprotected");
    let docs = home.join("Documents").join("dotfiles");
    fs::create_dir_all(docs.join("config").join("fish")).unwrap();
    let zshrc = docs.join("zshrc");
    fs::write(&zshrc, "export FOO=1\n").unwrap();
    std::os::unix::fs::symlink(&zshrc, home.join(".zshrc")).unwrap();
    let fish = docs.join("config").join("fish").join("config.fish");
    fs::write(&fish, "set -x FOO 1\n").unwrap();
    std::os::unix::fs::symlink(docs.join("config"), home.join(".config")).unwrap();
    let plain = home.join("dotfiles");
    fs::create_dir_all(&plain).unwrap();
    let bashrc = plain.join("bashrc");
    fs::write(&bashrc, "export BAR=1\n").unwrap();
    std::os::unix::fs::symlink(&bashrc, home.join(".bashrc")).unwrap();

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);

    assert_eq!(
        fs::read_to_string(&zshrc).unwrap(),
        "export FOO=1\n",
        "an rc reached through a link into Documents is left exactly as it was"
    );
    assert_eq!(
        fs::read_to_string(&fish).unwrap(),
        "set -x FOO 1\n",
        "a regular rc under a symlinked ~/.config that resolves into Documents too"
    );
    assert!(
        fs::read_to_string(&bashrc).unwrap().contains(RC_BEGIN),
        "the control: a link into an ordinary directory is still wired"
    );
    let mut stack = vec![home.join("Documents")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(!name.contains("atpkg"), "wrote {name:?} under Documents");
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
            }
        }
    }
    let _ = fs::remove_dir_all(&home);
}

/// The same fence for the command links: a `~/.local` linked under a protected root is
/// refused.
#[cfg(unix)]
#[test]
fn command_links_stay_out_of_a_protected_local_bin() {
    let home = tmp("cmdlinks");
    assert_eq!(command_links_dir(&home), Some(home.join(".local/bin")));
    fs::create_dir_all(home.join(".local")).unwrap();
    assert_eq!(command_links_dir(&home), Some(home.join(".local/bin")));
    fs::write(home.join(".local/bin"), b"an existing file").unwrap();
    assert_eq!(command_links_dir(&home), None, "bin must be a directory");
    fs::remove_dir_all(home.join(".local")).unwrap();
    let dot = home.join("Documents").join("dotlocal");
    fs::create_dir_all(home.join("Documents")).unwrap();
    std::os::unix::fs::symlink(&dot, home.join(".local")).unwrap();
    assert_eq!(command_links_dir(&home), None, "a missing target is fenced");
    fs::create_dir_all(&dot).unwrap();
    assert_eq!(
        command_links_dir(&home),
        None,
        "refused: resolves under Documents"
    );
    fs::create_dir_all(dot.join("bin")).unwrap();
    assert_eq!(command_links_dir(&home), None);
    let _ = fs::remove_dir_all(&home);
}

/// The PATH names are the release bundle's to write: from the dev bundle nothing is
/// planted and a release link is left alone; from the release bundle an absent link is
/// planted, a link into an older release location is repointed, and a link
/// `ship dev-app` aimed at the dev bundle is left alone.
#[cfg(unix)]
#[test]
fn command_links_are_written_only_from_the_release_bundle() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = tmp("cmdlink-owner");
    let root = scratch.canonicalize().unwrap();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let bundle = |app: &str| -> PathBuf {
        let macos = root.join("Applications").join(app).join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        for name in ["aterm", "atpkg", "agh"] {
            let p = macos.join(name);
            fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        macos
    };
    let release = bundle("aterm.app");
    let dev = bundle("aterm (dev).app");
    let old_release = root.join("Old").join("aterm.app").join("Contents/MacOS");
    let bin = home.join(".local/bin");
    let target = |name: &str| fs::read_link(bin.join(name)).ok();
    let set = |name: &str, to: &Path| {
        let _ = fs::remove_file(bin.join(name));
        std::os::unix::fs::symlink(to, bin.join(name)).unwrap();
    };
    ensure_command_links_from(&home, &dev.join("aterm"));
    assert_eq!(target("aterm"), None, "a dev bundle plants no `aterm`");
    assert_eq!(target("atpkg"), None, "a dev bundle plants no `atpkg`");
    assert_eq!(target("agh"), None, "a dev bundle plants no `agh`");

    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(target("aterm"), Some(release.join("aterm")));
    assert_eq!(target("atpkg"), Some(release.join("atpkg")));
    assert_eq!(target("agh"), Some(release.join("agh")));

    ensure_command_links_from(&home, &dev.join("aterm"));
    assert_eq!(
        target("aterm"),
        Some(release.join("aterm")),
        "a dev bundle's launch must not take the release's `aterm`"
    );
    assert_eq!(
        target("atpkg"),
        Some(release.join("atpkg")),
        "a dev bundle's launch must not take the release's `atpkg`"
    );
    assert_eq!(
        target("agh"),
        Some(release.join("agh")),
        "a dev bundle's launch must not take the release's `agh`"
    );

    set("aterm", &old_release.join("aterm"));
    set("agh", &old_release.join("agh"));
    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(target("aterm"), Some(release.join("aterm")));
    assert_eq!(target("agh"), Some(release.join("agh")));

    set("aterm", &dev.join("aterm"));
    set("agh", &dev.join("agh"));
    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(target("aterm"), Some(dev.join("aterm")));
    assert_eq!(target("agh"), Some(dev.join("agh")));

    set("atpkg", &dev.join("atpkg"));
    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(target("atpkg"), Some(release.join("atpkg")));
    assert_eq!(target("aterm"), Some(dev.join("aterm")));
    assert_eq!(target("agh"), Some(dev.join("agh")));

    assert!(in_release_bundle(&release.join("aterm")));
    assert!(!in_release_bundle(&dev.join("aterm")));
    assert!(is_non_release_bundle_exe(&dev.join("aterm")));
    assert!(!is_non_release_bundle_exe(&release.join("aterm")));
    assert!(
        !is_non_release_bundle_exe(&root.join("target/release/aterm")),
        "a source build is not a bundle, and is not caught"
    );
    let _ = fs::remove_dir_all(&root);
}

/// `amacs` JOINS THE COMMAND LINKS (`docs/DESIGN-amacs-2026-10-19.md` §8.1), owned as
/// `aterm` is.
#[cfg(unix)]
#[test]
fn command_links_lay_amacs_on_macos() {
    use std::os::unix::fs::symlink;
    let scratch = tmp("cmdlink-amacs");
    let root = scratch.canonicalize().unwrap();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let bundle = |app: &str, aliases: &[&str]| -> PathBuf {
        let macos = root.join(app).join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        let aterm = macos.join("aterm");
        fs::write(&aterm, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&aterm, fs::Permissions::from_mode(0o755)).unwrap();
        for alias in aliases {
            symlink("aterm", macos.join(alias)).unwrap();
        }
        macos
    };
    let before = bundle("Old/aterm.app", &["atpkg"]);
    let release = bundle("Applications/aterm.app", &["atpkg", "amacs", "agh"]);
    let dev = bundle("Applications/aterm (dev).app", &["atpkg", "amacs", "agh"]);
    let bin = home.join(".local/bin");
    let target = |name: &str| fs::read_link(bin.join(name)).ok();

    ensure_command_links_from(&home, &before.join("aterm"));
    assert_eq!(target("atpkg"), Some(before.join("atpkg")));
    assert_eq!(
        target("amacs"),
        None,
        "a bundle without amacs lays no link to it"
    );

    ensure_command_links_from(&home, &release.join("aterm"));
    for name in COMMAND_LINKS {
        assert_eq!(target(name), Some(release.join(name)), "{name}");
    }
    assert_eq!(
        fs::canonicalize(bin.join("amacs")).unwrap(),
        release.join("aterm")
    );
    assert_eq!(
        target("agh"),
        Some(release.join("agh")),
        "GitHub's client is on PATH outside a tab"
    );

    ensure_command_links_from(&home, &dev.join("aterm"));
    assert_eq!(target("amacs"), Some(release.join("amacs")));

    let aim = |name: &str, at: &Path| {
        fs::remove_file(bin.join(name)).unwrap();
        symlink(at, bin.join(name)).unwrap();
    };
    aim("aterm", &dev.join("aterm"));
    aim("amacs", &dev.join("amacs"));
    aim("agh", &dev.join("agh"));
    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(target("aterm"), Some(dev.join("aterm")));
    assert_eq!(target("amacs"), Some(dev.join("amacs")));
    assert_eq!(
        target("agh"),
        Some(dev.join("agh")),
        "agh stays while aterm is aimed away"
    );
    aim("aterm", &release.join("aterm"));
    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(target("amacs"), Some(release.join("amacs")));
    assert_eq!(
        target("agh"),
        Some(release.join("agh")),
        "agh follows aterm back"
    );
    let own = root.join("src/amacs/target/debug/amacs");
    aim("amacs", &own);
    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(target("amacs"), Some(own));

    fs::remove_file(bin.join("amacs")).unwrap();
    fs::write(bin.join("amacs"), b"someone's own").unwrap();
    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(fs::read(bin.join("amacs")).unwrap(), b"someone's own");
    assert_eq!(
        copied_command_links(&home),
        if cfg!(target_os = "macos") {
            vec![bin.join("amacs")]
        } else {
            Vec::new()
        }
    );
    if cfg!(target_os = "macos") {
        assert_eq!(
            move_aside_copied_commands_at(&home, &release.join("aterm"), 5),
            Ok(AsideOutcome {
                moved: vec![MovedAside {
                    from: bin.join("amacs"),
                    to: bin.join("amacs.moved-aside-5"),
                    linked: true,
                }],
                failed: Vec::new(),
            })
        );
        assert_eq!(target("amacs"), Some(release.join("amacs")));
        assert_eq!(
            fs::read(bin.join("amacs.moved-aside-5")).unwrap(),
            b"someone's own",
            "moved, not deleted"
        );
    }
    let _ = fs::remove_dir_all(&root);
}

/// `agh` JOINS THE COMMAND LINKS (`docs/DESIGN-agh-2026-10-19.md` §20), owned as `aterm`
/// is: a release bundle from before agh lays the other three names and nothing at
/// `~/.local/bin/agh`, a release bundle with agh points it into itself, and a regular
/// file at the name is left byte-identical.
#[cfg(unix)]
#[test]
fn command_links_skip_agh_where_the_bundle_lacks_it() {
    use std::os::unix::fs::symlink;
    let scratch = tmp("cmdlink-agh");
    let root = scratch.canonicalize().unwrap();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let bundle = |app: &str, aliases: &[&str]| -> PathBuf {
        let macos = root.join(app).join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        let aterm = macos.join("aterm");
        fs::write(&aterm, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&aterm, fs::Permissions::from_mode(0o755)).unwrap();
        for alias in aliases {
            symlink("aterm", macos.join(alias)).unwrap();
        }
        macos
    };
    let before = bundle("Old/aterm.app", &["atpkg", "amacs"]);
    let release = bundle("Applications/aterm.app", &["atpkg", "amacs", "agh"]);
    let bin = home.join(".local/bin");
    let target = |name: &str| fs::read_link(bin.join(name)).ok();

    ensure_command_links_from(&home, &before.join("aterm"));
    for name in ["aterm", "atpkg", "amacs"] {
        assert_eq!(target(name), Some(before.join(name)), "{name}");
    }
    assert!(
        fs::symlink_metadata(bin.join("agh")).is_err(),
        "a bundle without agh lays nothing at its name"
    );

    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(target("agh"), Some(release.join("agh")));
    assert_eq!(
        fs::canonicalize(bin.join("agh")).unwrap(),
        release.join("aterm")
    );

    fs::remove_file(bin.join("agh")).unwrap();
    fs::write(bin.join("agh"), b"someone's own").unwrap();
    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(fs::read(bin.join("agh")).unwrap(), b"someone's own");
    assert_eq!(
        copied_command_links(&home),
        if cfg!(target_os = "macos") {
            vec![bin.join("agh")]
        } else {
            Vec::new()
        }
    );
    let _ = fs::remove_dir_all(&root);
}

/// THE STORE SELF-HEAL (`docs/DESIGN-amacs-2026-10-19.md` §8.1). A store install.sh laid
/// before a name existed gains it beside the binary, spelled as install.sh spells its
/// own (a link to `aterm`), and a second pass lays nothing.
#[cfg(unix)]
#[test]
fn store_aliases_self_heal_lays_missing_and_never_replaces_a_file() {
    use std::os::unix::fs::symlink;
    let scratch = tmp("store-aliases");
    let root = scratch.canonicalize().unwrap();
    let resolve = |path: &Path| fs::canonicalize(path).ok();
    let binary = |dir: &Path, name: &str| -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let exe = dir.join(name);
        fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        exe
    };
    let store = root.join("home/.local/lib/aterm/bin");
    let aterm = binary(&store, "aterm");
    symlink("aterm", store.join("aterm-ctl")).unwrap();
    fs::write(store.join("atpkg"), b"someone's own").unwrap();
    symlink("/opt/their/aterm-gui", store.join("aterm-gui")).unwrap();
    symlink("gone", store.join("abrew")).unwrap();
    let held = ["aterm-ctl", "atpkg", "aterm-gui", "abrew"];

    assert_eq!(store_dir_of(&aterm, resolve), Some(store.as_path()));
    let laid = heal_store_aliases(&aterm);
    let missing: Vec<&str> = aterm_types::argv0::ALIASES
        .into_iter()
        .filter(|name| !held.contains(name))
        .collect();
    assert_eq!(laid, missing);
    assert!(laid.contains(&"amacs"), "the editor's alias is laid");
    for name in &laid {
        assert_eq!(fs::read_link(store.join(name)).unwrap(), Path::new("aterm"));
        assert_eq!(fs::canonicalize(store.join(name)).unwrap(), aterm, "{name}");
    }
    assert_eq!(
        fs::read(store.join("atpkg")).unwrap(),
        b"someone's own",
        "a file is never replaced"
    );
    assert_eq!(
        fs::read_link(store.join("aterm-gui")).unwrap(),
        Path::new("/opt/their/aterm-gui")
    );
    assert_eq!(
        fs::read_link(store.join("abrew")).unwrap(),
        Path::new("gone")
    );
    assert!(
        heal_store_aliases(&aterm).is_empty(),
        "a second pass lays nothing"
    );

    let tree = root.join("checkout/target/debug");
    let dev = binary(&tree, "aterm");
    binary(&tree, "aterm-ctl");
    let renamed_dir = root.join("renamed");
    let renamed = binary(&renamed_dir, "aterm-next");
    symlink("aterm-next", renamed_dir.join("aterm-ctl")).unwrap();
    let bare_dir = root.join("bare");
    let bare = binary(&bare_dir, "aterm");
    for (exe, dir) in [(dev, tree), (renamed, renamed_dir), (bare, bare_dir)] {
        assert_eq!(store_dir_of(&exe, resolve), None, "{}", exe.display());
        assert!(heal_store_aliases(&exe).is_empty(), "{}", exe.display());
        assert!(
            fs::symlink_metadata(dir.join("amacs")).is_err(),
            "nothing laid beside {}",
            exe.display()
        );
    }
    let _ = fs::remove_dir_all(&root);
}

/// THE HOOKS PASS RUNS THE HEAL, AND FIRST: through the pass every caller takes
/// ([`refresh_as`], which `refresh` and `refresh_rewiring_rc` reach), a store's missing
/// alias is laid even with no $HOME, where the pass then hardens and writes nothing.
#[cfg(unix)]
#[test]
fn the_hooks_pass_heals_a_store_before_it_needs_a_home() {
    use std::os::unix::fs::symlink;
    let scratch = tmp("store-pass");
    let root = scratch.canonicalize().unwrap();
    let layout = Layout {
        prefix: root.join("pkg"),
    };
    let store = root.join("lib/aterm/bin");
    fs::create_dir_all(&store).unwrap();
    let aterm = store.join("aterm");
    fs::write(&aterm, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&aterm, fs::Permissions::from_mode(0o755)).unwrap();
    symlink("aterm", store.join("aterm-ctl")).unwrap();
    let amacs = store.join("amacs");

    for wiring in [RcWiring::HonorOptOut, RcWiring::Rewire] {
        let pass = refresh_as(&layout, wiring, Some(&aterm), None, false, None);
        assert_eq!(pass, HookPass::NotHardened);
        assert!(
            fs::symlink_metadata(&amacs).is_err(),
            "heal off: nothing laid"
        );
    }
    let pass = refresh_as(
        &layout,
        RcWiring::HonorOptOut,
        Some(&aterm),
        None,
        true,
        None,
    );
    assert_eq!(pass, HookPass::NotHardened, "no $HOME: no hook is written");
    assert_eq!(fs::read_link(&amacs).unwrap(), Path::new("aterm"));
    assert!(
        !layout.prefix.exists(),
        "the heal writes the store and nothing else"
    );
    let _ = fs::remove_dir_all(&root);
}

/// A copied binary at a link path stays until the owner asks: the unattended pass leaves
/// it byte-identical and [`copied_command_links`] names it for `doctor`.
#[cfg(unix)]
#[test]
fn a_copied_command_is_left_alone_unattended_and_moved_aside_only_by_repair() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = tmp("cmdlink-copied");
    let root = scratch.canonicalize().unwrap();
    let home = root.join("home");
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let bundle = |app: &str| -> PathBuf {
        let macos = root.join("Applications").join(app).join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        for name in ["aterm", "atpkg"] {
            let p = macos.join(name);
            fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        macos
    };
    let release = bundle("aterm.app");
    let dev = bundle("aterm (dev).app");
    let copied = bin.join("aterm");
    let bytes = b"#!/bin/sh\necho 'aterm 0.15.0'\n";
    fs::write(&copied, bytes).unwrap();
    let is_file = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file());

    ensure_command_links_from(&home, &release.join("aterm"));
    assert_eq!(
        fs::read(&copied).unwrap(),
        bytes,
        "the unattended pass never touches it"
    );
    assert_eq!(
        fs::read_link(bin.join("atpkg")).ok(),
        Some(release.join("atpkg")),
        "the name beside it is still linked"
    );
    assert_eq!(copied_command_links(&home), vec![copied.clone()]);

    assert!(
        move_aside_copied_commands_at(&home, &dev.join("aterm"), 7).is_err(),
        "a dev bundle does not lay the links"
    );
    assert!(
        is_file(&copied),
        "from a dev bundle nothing would take the name"
    );

    let earlier = bin.join("aterm.moved-aside-7");
    fs::write(&earlier, b"earlier").unwrap();
    let refused = move_aside_copied_commands_at(&home, &release.join("aterm"), 7).unwrap();
    assert!(refused.moved.is_empty());
    assert_eq!(refused.failed.len(), 1);
    assert_eq!(
        fs::read(&earlier).unwrap(),
        b"earlier",
        "an earlier move is never overwritten"
    );
    assert!(is_file(&copied));

    let aside = bin.join("aterm.moved-aside-9");
    let ctl = bin.join("aterm-ctl");
    fs::write(&ctl, b"old ctl").unwrap();
    assert_eq!(
        move_aside_copied_commands_at(&home, &release.join("aterm"), 9),
        Ok(AsideOutcome {
            moved: vec![
                MovedAside {
                    from: copied.clone(),
                    to: aside.clone(),
                    linked: true,
                },
                MovedAside {
                    from: ctl.clone(),
                    to: bin.join("aterm-ctl.moved-aside-9"),
                    linked: false,
                },
            ],
            failed: Vec::new(),
        })
    );
    assert!(
        fs::symlink_metadata(&ctl).is_err(),
        "a retired name is moved and nothing is linked in its place"
    );
    assert_eq!(fs::read(&aside).unwrap(), bytes, "renamed, not deleted");
    assert_eq!(fs::read_link(&copied).ok(), Some(release.join("aterm")));
    assert!(copied_command_links(&home).is_empty());
    assert!(
        move_aside_copied_commands_at(&home, &release.join("aterm"), 10)
            == Ok(AsideOutcome::default()),
        "a second repair has nothing to move"
    );
    let _ = fs::remove_dir_all(&root);
}

/// A move that cannot finish changes nothing: an occupied temporary name stays intact, the
/// source keeps its name, and no second name is left behind.
#[cfg(unix)]
#[test]
fn a_move_aside_that_cannot_finish_changes_nothing() {
    let scratch = tmp("aside-rollback");
    let dir = scratch.canonicalize().unwrap();
    let from = dir.join("aterm");
    let aside = dir.join("aterm.moved-aside-1");
    fs::write(&from, b"a build of someone's own").unwrap();
    let target = dir.join("target");
    fs::write(&target, b"").unwrap();
    let blocker = dir.join(format!("aterm.link-{}", std::process::id()));
    fs::create_dir_all(blocker.join("inside")).unwrap();

    assert!(rename_file_aside(&from, &aside, Some(&target)).is_err());
    assert_eq!(fs::read(&from).unwrap(), b"a build of someone's own");
    assert!(
        fs::symlink_metadata(&aside).is_err(),
        "no second name left behind"
    );

    fs::remove_dir_all(&blocker).unwrap();
    for symlink in [false, true] {
        if symlink {
            std::os::unix::fs::symlink(&target, &blocker).unwrap();
        } else {
            fs::write(&blocker, b"someone else's file").unwrap();
        }
        assert!(rename_file_aside(&from, &aside, Some(&target)).is_err());
        assert_eq!(fs::read(&from).unwrap(), b"a build of someone's own");
        assert!(fs::symlink_metadata(&aside).is_err());
        if symlink {
            assert_eq!(fs::read_link(&blocker).unwrap(), target);
        } else {
            assert_eq!(fs::read(&blocker).unwrap(), b"someone else's file");
        }
        fs::remove_file(&blocker).unwrap();
    }
    rename_file_aside(&from, &aside, Some(&target)).unwrap();
    assert_eq!(fs::read_link(&from).unwrap(), target);
    assert_eq!(fs::read(&aside).unwrap(), b"a build of someone's own");
    let _ = fs::remove_dir_all(&dir);
}

/// A retired name is moved with nothing linked in its place; when its old name cannot be
/// removed (a read-only folder), the second name is dropped and the file stays put.
#[cfg(unix)]
#[test]
fn a_retired_name_that_cannot_leave_its_folder_stays_put() {
    use std::os::unix::fs::PermissionsExt as _;
    aterm_uds::host::could_not_run_as_root("a folder whose mode lacks write");
    struct Unlock(PathBuf);
    impl Drop for Unlock {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }
    let scratch = tmp("aside-retired");
    let dir = scratch.canonicalize().unwrap();
    let locked = dir.join("locked");
    let open = dir.join("open");
    fs::create_dir_all(&locked).unwrap();
    fs::create_dir_all(&open).unwrap();
    let from = locked.join("aterm-ctl");
    let aside = open.join("aterm-ctl.moved-aside-1");
    fs::write(&from, b"an old aterm-ctl").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let unlock = Unlock(locked.clone());

    let moved = rename_file_aside(&from, &aside, None);
    drop(unlock);
    assert!(moved.is_err());
    assert_eq!(fs::read(&from).unwrap(), b"an old aterm-ctl");
    assert!(
        fs::symlink_metadata(&aside).is_err(),
        "no second name left behind"
    );

    rename_file_aside(&from, &aside, None).unwrap();
    assert!(
        fs::symlink_metadata(&from).is_err(),
        "moved, nothing in its place"
    );
    assert_eq!(fs::read(&aside).unwrap(), b"an old aterm-ctl");
    let _ = fs::remove_dir_all(&dir);
}

/// The remedy for a copied command on macOS names the installed app's own `atpkg`:
/// the alias, from the first app it would act from (never a translocated one), quoted
/// as one shell word.
#[cfg(target_os = "macos")]
#[test]
fn the_remedy_names_the_installed_app_it_would_act_from() {
    let scratch = tmp("hint-installed");
    let root = scratch.canonicalize().unwrap();
    let macos = root.join("Apps/aterm.app/Contents/MacOS");
    fs::create_dir_all(&macos).unwrap();
    fs::write(macos.join("aterm"), b"").unwrap();
    std::os::unix::fs::symlink("aterm", macos.join("atpkg")).unwrap();
    let mount = root.join("AppTranslocation/X/d/aterm.app/Contents/MacOS");
    fs::create_dir_all(&mount).unwrap();
    fs::write(mount.join("aterm"), b"").unwrap();
    std::os::unix::fs::symlink("aterm", mount.join("atpkg")).unwrap();
    fs::create_dir_all(root.join("Moved")).unwrap();
    std::os::unix::fs::symlink(
        root.join("AppTranslocation/X/d/aterm.app"),
        root.join("Moved/aterm.app"),
    )
    .unwrap();
    assert_eq!(installed_app_atpkg(&[Some(root.join("Moved"))]), None);
    assert_eq!(
        installed_app_atpkg(&[None, Some(root.join("Moved")), Some(root.join("Apps"))]),
        Some(macos.join("atpkg")),
        "the alias, not the binary it links to, from the app it would act from"
    );
    assert_eq!(installed_app_atpkg(&[Some(root.join("Nowhere"))]), None);
    let second = root.join("Second/aterm.app/Contents/MacOS");
    fs::create_dir_all(&second).unwrap();
    fs::write(second.join("aterm"), b"").unwrap();
    std::os::unix::fs::symlink("aterm", second.join("atpkg")).unwrap();
    assert_eq!(
        installed_app_atpkg(&[Some(root.join("Second")), Some(root.join("Apps"))]),
        Some(second.join("atpkg"))
    );
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        repair_hint_for(Some(Path::new(
            "/Bob's Apps/aterm.app/Contents/MacOS/atpkg"
        ))),
        "`'/Bob'\\''s Apps/aterm.app/Contents/MacOS/atpkg' repair`"
    );
    assert_eq!(
        installed_app_dirs(Some(PathBuf::from("/Users//u"))),
        [
            Some(PathBuf::from("/Applications")),
            Some(PathBuf::from("/Users//u/Applications"))
        ],
        "the system folder first"
    );
}

/// The links are laid only from a bundle that stays: not a translocated copy and
/// not one run from a mounted disk image or removable disk.
#[cfg(all(unix, target_os = "macos"))]
#[test]
fn links_come_only_from_a_bundle_that_stays_and_the_remedy_is_one_word() {
    for gone in [
        "/Volumes/aterm/aterm.app/Contents/MacOS/aterm",
        "/private/var/folders/x/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm",
        "/Apps/aterm (dev).app/Contents/MacOS/aterm",
        "/src/target/release/aterm",
    ] {
        assert_eq!(command_link_source(Path::new(gone)), None, "{gone}");
        assert!(command_link_check(Path::new(gone)).is_err(), "{gone}");
    }
    assert!(
        command_link_source(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")).is_some()
    );
}

/// The shell hooks are the release bundle's to lay unattended: from a dev bundle the
/// unattended pass writes nothing, from the release bundle it writes, and `repair` writes
/// from either.
#[cfg(unix)]
#[test]
fn an_unattended_hooks_pass_from_a_dev_bundle_writes_nothing() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = tmp("hooks-owner");
    let root = scratch.canonicalize().unwrap();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let layout = Layout {
        prefix: root.join("pkg"),
    };
    let dev = root.join("Applications/aterm (dev).app/Contents/MacOS/aterm");
    let release = root.join("Applications/aterm.app/Contents/MacOS/aterm");
    let hook = home.join(".aterm/shell.d/00-atpkg.zsh");

    let pass = pass_as(
        &layout,
        &RcHome::at(&home),
        RcWiring::HonorOptOut,
        Some(&dev),
        None,
    );
    assert_eq!(pass, HookPass::NotReleaseBundle);
    assert!(pass.rc().is_empty());
    assert!(
        !home.join(".aterm").exists(),
        "the unattended pass from a dev bundle touches nothing under $HOME"
    );

    let pass = pass_as(
        &layout,
        &RcHome::at(&home),
        RcWiring::HonorOptOut,
        Some(&release),
        None,
    );
    assert!(matches!(pass, HookPass::Rewritten(_)), "{pass:?}");
    assert!(hook.exists(), "the release bundle lays the hook");

    fs::remove_file(&hook).unwrap();
    let pass = pass_as(
        &layout,
        &RcHome::at(&home),
        RcWiring::Rewire,
        Some(&dev),
        None,
    );
    assert!(matches!(pass, HookPass::Rewritten(_)), "{pass:?}");
    assert!(
        hook.exists(),
        "repair is the asked-for pass and writes from anywhere"
    );

    let pass = pass_as(
        &layout,
        &RcHome::at(&home),
        RcWiring::HonorOptOut,
        None,
        None,
    );
    assert!(
        matches!(pass, HookPass::Rewritten(_)),
        "an unresolvable executable never stops a pass: {pass:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn powershell_dialect_appends_bin_and_moves_agents_to_the_front_via_platform_separator() {
    let files = hook_files(
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
    );
    let (_, ps) = files.iter().find(|(n, _)| n.ends_with(".ps1")).unwrap();
    assert!(
        ps.contains("[System.IO.Path]::PathSeparator"),
        "OS-correct separator"
    );
    assert!(
        ps.contains("$env:PATH = \"$env:PATH$__atpkg_sep$__atpkg_bin\""),
        "bin/ is appended, never prepended"
    );
    assert!(
        ps.contains("$__atpkg_in = [bool]($env:ATERM_AGENTS_DIR -or $env:ATERM_CHILD -or $env:ATERM_SESSION_ID)")
            && ps.contains(
                "Where-Object { $_ -ne $__atpkg_agents -and $_ -ne $__atpkg_reroute -and (-not $__atpkg_in -or $_ -ne $__atpkg_bin) }"
            )
            && ps.contains("if ($__atpkg_in) { $__atpkg_front = @($__atpkg_agents, $__atpkg_bin); $env:ATPKG_AGENTS")
            && ps.contains("$env:PATH = ($__atpkg_front + $__atpkg_rest) -join $__atpkg_sep\n"),
        "agents/ and bin/ are moved to the front inside aterm: removed, then prepended: {ps}"
    );
    assert!(
        ps.contains(&format!(
            "if (($__atpkg_in -or $env:{r}) -and (-not $env:{p} -or $env:{p} -eq '0')) {{ $__atpkg_front = @($__atpkg_reroute) + $__atpkg_front }}",
            r = crate::reroute::REROUTE_DIR_ENV,
            p = crate::reroute::PASSTHROUGH_ENV
        )),
        "reroute/ leads agents/ inside aterm unless --no-reroute's marker is engaged: {ps}"
    );
    assert!(
        !ps.contains(&format!("$env:{} =", crate::reroute::REROUTE_DIR_ENV)),
        "the handle is read, never set: {ps}"
    );
    assert!(
        ps.contains("-notcontains $__atpkg_bin") && !ps.contains("-notcontains $__atpkg_agents"),
        "bin/ keeps its presence guard; agents/ has none — an order, not a presence"
    );
    assert!(ps.contains("$env:ATPKG_AGENTS = $__atpkg_agents"));
    assert!(!ps.contains("export "), "no POSIX export in a .ps1");
}

#[test]
fn powershell_single_quotes_a_windows_path_so_backslashes_survive() {
    let files = hook_files(
        Path::new(r"C:\Users\x\AppData\Local\aterm\pkg\bin"),
        Path::new(r"C:\Users\x\AppData\Local\aterm\pkg\agents"),
        Path::new(r"C:\Users\x\AppData\Local\aterm\pkg\reroute"),
    );
    let (_, ps) = files.iter().find(|(n, _)| n.ends_with(".ps1")).unwrap();
    assert!(
        ps.contains(r"$__atpkg_bin = 'C:\Users\x\AppData\Local\aterm\pkg\bin'"),
        "a single-quoted PS literal keeps backslashes verbatim"
    );
    assert!(ps.contains(r"$__atpkg_agents = 'C:\Users\x\AppData\Local\aterm\pkg\agents'"));
    assert!(ps.contains(r"$__atpkg_reroute = 'C:\Users\x\AppData\Local\aterm\pkg\reroute'"));
}

#[test]
fn fish_dialect_appends_bin_moves_agents_to_the_front_and_avoids_posix_export() {
    let files = hook_files(
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
    );
    let (_, fish) = files.iter().find(|(n, _)| n.ends_with(".fish")).unwrap();
    assert!(
        fish.contains("set -gx PATH $PATH $__atpkg_bin"),
        "fish append"
    );
    assert!(
        fish.contains("set -gx PATH $__atpkg_front $__atpkg_rest\n")
            && fish.contains("set -l __atpkg_front\n")
            && fish.contains(&format!(
                "if test -n \"$ATERM_AGENTS_DIR$ATERM_CHILD$ATERM_SESSION_ID\"; set __atpkg_front \"$__atpkg_agents\" \"$__atpkg_bin\"; set -gx ATPKG_AGENTS $__atpkg_agents; else; set -e ATPKG_AGENTS; end\n\
                 if test -z \"${p}\"; or test \"${p}\" = 0; if test -n \"$ATERM_AGENTS_DIR$ATERM_CHILD$ATERM_SESSION_ID${r}\"; set __atpkg_front \"$__atpkg_reroute\" $__atpkg_front; end; end\n",
                p = crate::reroute::PASSTHROUGH_ENV,
                r = crate::reroute::REROUTE_DIR_ENV
            ))
            && !fish.contains(&format!("set -gx {}", crate::reroute::REROUTE_DIR_ENV))
            && fish.contains("if test \"$__atpkg_d\" != \"$__atpkg_agents\"; and test \"$__atpkg_d\" != \"$__atpkg_reroute\";"),
        "fish moves agents/ and bin/ (and the reroute dir ahead of them) to the front by equality: {fish}"
    );
    assert!(fish.contains("set -gx ATPKG_AGENTS $__atpkg_agents"));
    assert!(
        fish.contains("set __atpkg_rest $__atpkg_rest \"$__atpkg_d\"") && !fish.contains("set -a"),
        "no `set -a`: fish 2.x lacks it, and a failed append would set PATH to the agents dir alone"
    );
    assert_eq!(
        fish.matches("if not contains").count(),
        1,
        "one presence guard, bin/'s"
    );
    assert!(!fish.contains("export "), "no POSIX export in fish");
}

/// The four hooks move as a set, or not at all: a staging failure on the third publishes
/// none.
#[cfg(unix)]
#[test]
fn a_failed_hook_write_replaces_none_of_the_dialects() {
    let d = tmp("writeset");
    let out = HookPublication::prepare_with(
        &d,
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
        |path, content| {
            if path.extension().is_some_and(|e| e == "fish") {
                fs::create_dir(path).unwrap();
            }
            stage_hook(path, content)
        },
    );
    assert!(
        out.is_err(),
        "the blocked dialect must fail the whole write"
    );
    for name in [
        "00-atpkg.zsh",
        "00-atpkg.bash",
        "00-atpkg.fish",
        "00-atpkg.ps1",
    ] {
        assert!(
            !d.join(name).exists(),
            "{name} must not be published when a sibling dialect could not be written"
        );
    }

    let _ = fs::remove_dir_all(&d);
}

/// The publish phase moves as a set too: when the last dialect's rename fails, the three
/// already published over a previous pass's hooks are all put back, and neither a temp
/// nor a saved predecessor is left.
#[cfg(unix)]
#[test]
fn a_failed_publish_puts_back_every_dialect_it_had_already_replaced() {
    let d = tmp("publishset");
    let old = "# the previous pass's hook\n";
    for name in ["00-atpkg.zsh", "00-atpkg.bash", "00-atpkg.fish"] {
        fs::write(d.join(name), old).unwrap();
    }
    fs::create_dir_all(d.join("00-atpkg.ps1").join("occupied")).unwrap();

    let publication = HookPublication::prepare(
        &d,
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
    )
    .unwrap();
    assert!(
        matches!(
            crate::lock::Flock::try_lock(hook_lock_file(&d).unwrap()),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "another writer cannot enter the staged transaction"
    );
    let out = publication.publish();
    assert!(out.is_err(), "a rename onto a directory must fail the pass");
    drop(
        crate::lock::Flock::try_lock(hook_lock_file(&d).unwrap())
            .expect("rollback releases its lock"),
    );

    for name in ["00-atpkg.zsh", "00-atpkg.bash", "00-atpkg.fish"] {
        assert_eq!(
            fs::read_to_string(d.join(name)).unwrap(),
            old,
            "{name} must carry the PREVIOUS pass's hook again — the four files are only \
             ever correct as a set, and a partial publish is what suppressed the rc \
             wiring for every shell"
        );
    }
    let litter: Vec<String> = fs::read_dir(&d)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.starts_with("hook-publication-"))
        .collect();
    assert!(
        litter.is_empty(),
        "a failed publish must leave no temp and no saved predecessor, found {litter:?}"
    );

    let _ = fs::remove_dir_all(&d);
}

#[test]
fn fish_quoting_escapes_fishs_metacharacters_and_leaves_a_backtick_alone() {
    let hostile = PathBuf::from("/opt/a`b/c$d/e\"f/g\\h/i j");
    assert_eq!(
        fish_quote(&hostile),
        "/opt/a`b/c\\$d/e\\\"f/g\\\\h/i j",
        "fish: escape backslash, double quote and dollar -- and nothing else"
    );
    assert_eq!(
        sh_quote(&hostile),
        "/opt/a\\`b/c\\$d/e\\\"f/g\\\\h/i j",
        "sh: the backtick IS special in a POSIX double-quoted string, and stays escaped"
    );

    let files = hook_files(&hostile, &hostile, &hostile);
    let body = |ext: &str| -> String {
        files
            .iter()
            .find(|(n, _)| n.ends_with(ext))
            .map(|(_, b)| b.clone())
            .unwrap_or_else(|| panic!("hook_files writes no {ext} hook"))
    };
    let fish = body(".fish");
    assert!(
        fish.contains("set -l __atpkg_bin \"/opt/a`b/c\\$d/"),
        "the fish body must carry the backtick unescaped and the dollar escaped: {fish:?}"
    );
    assert!(
        !fish.contains("a\\`b"),
        "a backtick must never be backslash-escaped in a fish string: {fish:?}"
    );
    assert!(
        body(".zsh").contains("a\\`b"),
        "the POSIX body must still escape the backtick"
    );
}

/// The fish quoter judged by fish: the hook body, rendered for a bin, agents and
/// reroute dir under a path holding every character fish's double quotes treat
/// specially (`\\`, `"`, `$`) and the ones they do not (a backtick, a space), is
/// sourced by a real fish, which must name each directory byte for byte.
#[cfg(unix)]
#[test]
fn the_fish_hook_names_a_hostile_path_verbatim_in_a_real_fish() {
    let root = tmp("fishhostile");
    let hostile = root
        .join("a`b")
        .join("c$d")
        .join("e\"f")
        .join("g\\h")
        .join("i j");
    let (bin, agents, reroute) = (
        hostile.join("bin"),
        hostile.join("agents"),
        hostile.join("reroute"),
    );
    for dir in [&bin, &agents, &reroute] {
        fs::create_dir_all(dir).unwrap();
    }
    let files = hook_files(&bin, &agents, &reroute);
    let hook = root.join("hook.fish");
    let body = &files.iter().find(|(n, _)| n.ends_with(".fish")).unwrap().1;
    fs::write(&hook, body).unwrap();
    let mut absent = Vec::new();
    if let Some(fish) = real_shell("fish", &mut absent) {
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let out = aterm_spec::host::shell_command(&fish, &home)
            .args([
                "--no-config",
                "-c",
                "source \"$ATPKG_TEST_HOOK\"; printf 'BIN=%s\\n' \"$ATPKG_BIN\"; \
                 printf 'AGENTS=%s\\n' \"$ATPKG_AGENTS\"; printf 'PATH=%s\\n' $PATH",
            ])
            .env("PATH", "/usr/bin:/bin")
            .env("ATERM_SESSION_ID", "atpkg-hook-test")
            .env_remove(crate::reroute::PASSTHROUGH_ENV)
            .env("ATPKG_TEST_HOOK", &hook)
            .output()
            .expect("fish runs");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && stderr.is_empty(),
            "fish: exit {:?}, stderr={stderr:?}",
            out.status.code()
        );
        let lines: Vec<&str> = stdout.lines().collect();
        let path: Vec<&str> = lines
            .iter()
            .filter_map(|l| l.strip_prefix("PATH="))
            .collect();
        assert_eq!(
            lines.first().copied(),
            Some(format!("BIN={}", bin.display()).as_str()),
            "fish must name the bin dir verbatim: {stdout:?}"
        );
        assert!(
            lines.contains(&format!("AGENTS={}", agents.display()).as_str()),
            "fish must name the agents dir verbatim: {stdout:?}"
        );
        assert_eq!(
            path.get(..3),
            Some(
                &[
                    reroute.to_str().unwrap(),
                    agents.to_str().unwrap(),
                    bin.to_str().unwrap(),
                ][..]
            ),
            "reroute, agents, and bin lead inside aterm, verbatim: {stdout:?}"
        );

        let rc_home = hostile.join("home");
        let rc = rc_home.join(".config/fish/config.fish");
        fs::create_dir_all(rc.parent().unwrap()).unwrap();
        fs::write(&rc, "# pre-existing user content\n").unwrap();
        let shell_d = rc_home.join(".aterm").join("shell.d");
        fs::create_dir_all(&shell_d).unwrap();
        fs::write(
            shell_d.join(format!("{HOOK_BASENAME}.fish")),
            "set -g ATPKG_RC_TEST sourced\n",
        )
        .unwrap();
        ensure_rc_sources_hooks(&RcHome::at(&rc_home), RcWiring::HonorOptOut, None);
        let written = fs::read_to_string(&rc).unwrap();
        let line = written
            .lines()
            .find(|l| l.contains("00-atpkg.fish"))
            .unwrap_or_else(|| panic!("no fish source line in {written:?}"));
        let out = aterm_spec::host::shell_command(&fish, &home)
            .args(["--no-config", "-c"])
            .arg(format!("{line}\nprintf 'X=%s\\n' \"$ATPKG_RC_TEST\""))
            .output()
            .expect("fish runs");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim_end(),
            "X=sourced",
            "the fish rc line must source its hook from a hostile HOME: line={line:?}, \
             stderr={:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = fs::remove_dir_all(&root);
    aterm_spec::host::could_not_run_without(&absent);
}

/// The fish rc line names a `$HOME` with a backtick verbatim.
#[cfg(unix)]
#[test]
fn the_fish_rc_line_carries_a_backtick_home_unescaped() {
    let home = crate::testdir::dir("atpkg-hooks-fish`tick-");
    let rc = home.join(".config/fish/config.fish");
    fs::create_dir_all(rc.parent().unwrap()).unwrap();
    fs::write(&rc, "# pre-existing user content\n").unwrap();

    ensure_rc_sources_hooks(&RcHome::at(&home), RcWiring::HonorOptOut, None);

    let written = fs::read_to_string(&rc).unwrap();
    let line = written
        .lines()
        .find(|l| l.contains("00-atpkg.fish"))
        .unwrap_or_else(|| panic!("no fish source line in {written:?}"))
        .to_owned();
    let hook = home.join(".aterm").join("shell.d").join("00-atpkg.fish");
    assert!(
        line.contains(&format!("\"{}\"", hook.display())),
        "the fish rc line must name the hook path verbatim, got {line:?}"
    );
    assert!(
        !line.contains("\\`"),
        "a backtick must never be backslash-escaped in the fish rc line, got {line:?}"
    );

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn write_hooks_writes_all_dialects_and_removes_stray_sh() {
    let d = tmp("write");
    fs::write(d.join("00-atpkg.sh"), b"echo stray\n").unwrap();
    let written = write_hooks(
        &d,
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
    )
    .unwrap();
    assert_eq!(written.len(), 4);
    for name in [
        "00-atpkg.zsh",
        "00-atpkg.bash",
        "00-atpkg.fish",
        "00-atpkg.ps1",
    ] {
        assert!(d.join(name).is_file(), "{name} written");
    }
    assert!(
        !d.join("00-atpkg.sh").exists(),
        "stray .sh removed (fish-safety)"
    );
    let body = fs::read_to_string(d.join("00-atpkg.zsh")).unwrap();
    assert!(body.contains("/p/bin") && body.contains("/p/agents"));
    #[cfg(unix)]
    {
        let mode = fs::metadata(d.join("00-atpkg.zsh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn publication_preserves_another_writers_staging_and_predecessor() {
    let d = tmp("other-publication");
    let stage = d.join(format!(".{HOOK_BASENAME}.zsh.tmp-{}", std::process::id()));
    let saved = d.join(format!(".{HOOK_BASENAME}.zsh.prev-{}", std::process::id()));
    fs::write(&stage, b"another transaction's staging").unwrap();
    fs::write(&saved, b"another transaction's predecessor").unwrap();
    fs::write(d.join("00-atpkg.zsh"), b"existing hook").unwrap();

    write_hooks(
        &d,
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
    )
    .unwrap();

    assert_eq!(fs::read(stage).unwrap(), b"another transaction's staging");
    assert_eq!(
        fs::read(saved).unwrap(),
        b"another transaction's predecessor"
    );
}

#[test]
fn failed_rollback_retains_the_predecessor_and_releases_the_writer_lock() {
    let d = tmp("rollback-retained");
    let destination = d.join("00-atpkg.zsh");
    fs::write(&destination, b"original bytes").unwrap();
    let mut publication = HookPublication::prepare(
        &d,
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
    )
    .unwrap();
    let directory = publication.directory.as_ref().unwrap().path().to_path_buf();
    let saved = directory.join("previous-00-atpkg.zsh");
    let previous = move_aside(&destination, &saved).unwrap();
    // An outside edit prevents restoring this destination; its predecessor must survive.
    fs::create_dir(&destination).unwrap();
    let error = publication.rollback(
        vec![(destination.clone(), previous)],
        io::Error::other("publish refused"),
    );
    drop(publication);
    assert_eq!(fs::read(saved).unwrap(), b"original bytes");
    assert!(destination.is_dir());
    assert!(
        error.to_string().contains(&directory.display().to_string()),
        "{error}"
    );
    drop(
        crate::lock::Flock::try_lock(hook_lock_file(&d).unwrap())
            .expect("failed rollback releases its writer lock"),
    );
}

#[cfg(unix)]
#[test]
fn a_link_cannot_redirect_the_hook_writer_lock() {
    let d = tmp("publication-lock-link");
    let target = d.join("unrelated");
    fs::write(&target, b"unrelated bytes").unwrap();
    std::os::unix::fs::symlink(&target, d.join("hooks.lock")).unwrap();
    assert!(
        write_hooks(
            &d,
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute")
        )
        .is_err()
    );
    assert_eq!(fs::read(&target).unwrap(), b"unrelated bytes");
    assert!(!d.join("00-atpkg.zsh").exists());
}

#[cfg(unix)]
#[test]
fn rollback_restores_a_hook_link_without_modifying_its_target() {
    let d = tmp("rollback-link");
    let target = d.join("unrelated");
    fs::write(&target, b"unrelated contents").unwrap();
    std::os::unix::fs::symlink(&target, d.join("00-atpkg.zsh")).unwrap();
    fs::create_dir(d.join("00-atpkg.ps1")).unwrap();
    assert!(
        write_hooks(
            &d,
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute")
        )
        .is_err()
    );
    assert_eq!(fs::read_link(d.join("00-atpkg.zsh")).unwrap(), target);
    assert_eq!(
        fs::read(d.join("unrelated")).unwrap(),
        b"unrelated contents"
    );
}

/// A hook that is already current is never rewritten (same inode), while one whose bytes
/// or mode drifted is.
#[cfg(unix)]
#[test]
fn write_hooks_leaves_identical_hooks_untouched() {
    use std::os::unix::fs::MetadataExt as _;
    let d = tmp("identical");
    let (bin, agents, reroute) = (
        Path::new("/p/bin"),
        Path::new("/p/agents"),
        Path::new("/p/reroute"),
    );
    write_hooks(&d, bin, agents, reroute).unwrap();
    let inode = |name: &str| fs::metadata(d.join(name)).unwrap().ino();
    let names = [
        "00-atpkg.zsh",
        "00-atpkg.bash",
        "00-atpkg.fish",
        "00-atpkg.ps1",
    ];
    let before: Vec<u64> = names.iter().map(|n| inode(n)).collect();
    let again = write_hooks(&d, bin, agents, reroute).unwrap();
    assert_eq!(again.len(), 4, "every dialect is still in place: {again:?}");
    let after: Vec<u64> = names.iter().map(|n| inode(n)).collect();
    assert_eq!(before, after, "no hook was rewritten");
    fs::write(d.join("00-atpkg.zsh"), "# edited by hand\n").unwrap();
    fs::set_permissions(d.join("00-atpkg.fish"), fs::Permissions::from_mode(0o644)).unwrap();
    write_hooks(&d, bin, agents, reroute).unwrap();
    for (name, was) in names.iter().zip(&before) {
        let drifted = *name == "00-atpkg.zsh" || *name == "00-atpkg.fish";
        assert_eq!(inode(name) != *was, drifted, "{name}");
    }
    assert_eq!(
        fs::metadata(d.join("00-atpkg.fish"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let _ = fs::remove_dir_all(&d);
}

/// THE MEASURED FAILURE, REPLAYED IN REAL SHELLS.
#[cfg(unix)]
#[test]
fn every_hook_dialect_orders_path_in_its_real_shell() {
    let root = tmp("realsh");
    let agents = root.join("Application Support/pkg/agents");
    let reroute = root.join("Application Support/pkg/reroute");
    let bin = root.join("Application Support/pkg/bin");
    let files = hook_files(&bin, &agents, &reroute);
    let (a, r, b) = (
        agents.to_str().unwrap(),
        reroute.to_str().unwrap(),
        bin.to_str().unwrap(),
    );
    let inherited = format!("/Users//u/.local/bin:/usr/bin:/opt/homebrew/bin:{r}:{a}::/bin:{a}:{r}");
    let want = format!("{r}:{a}:{b}:/Users//u/.local/bin:/usr/bin:/opt/homebrew/bin::/bin");
    let posix_cmd =
        "PATH=\"$ATPKG_TEST_PATH\"; . \"$ATPKG_TEST_HOOK\"; printf 'PATH=%s\\n' \"$PATH\"";
    let fish_cmd = "set -gx PATH (string split -- : \"$ATPKG_TEST_PATH\"); \
                    source \"$ATPKG_TEST_HOOK\"; printf 'PATH=%s\\n' (string join : $PATH)";
    let pwsh_cmd = "$env0 = [Environment]::GetEnvironmentVariables(); \
                    foreach ($i in 0..([int]$env:ATPKG_TEST_CASES - 1)) { \
                    foreach ($k in @([Environment]::GetEnvironmentVariables().Keys)) { \
                    if (-not $env0.Contains($k)) { [Environment]::SetEnvironmentVariable($k, $null) } }; \
                    foreach ($k in $env0.Keys) { [Environment]::SetEnvironmentVariable($k, $env0[$k]) }; \
                    & { $env:PATH = [Environment]::GetEnvironmentVariable(\"ATPKG_TEST_PATH_$i\"); \
                    . $env:ATPKG_TEST_HOOK; \
                    [Console]::Out.Write('PATH=' + $env:PATH + [char]10) } }";
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let mut absent = Vec::new();
    for (name, args, ext, cmd) in [
        (
            "bash",
            &["--noprofile", "--norc", "-c"][..],
            "bash",
            posix_cmd,
        ),
        ("zsh", &["-f", "-c"][..], "zsh", posix_cmd),
        ("fish", &["--no-config", "-c"][..], "fish", fish_cmd),
        (
            "pwsh",
            &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"][..],
            "ps1",
            pwsh_cmd,
        ),
    ] {
        let hook = root.join(format!("hook.{ext}"));
        let body = &files.iter().find(|(n, _)| n.ends_with(ext)).unwrap().1;
        fs::write(&hook, body).unwrap();
        let Some(shell) = real_shell(name, &mut absent) else {
            continue;
        };
        let spawn = |paths: &[&str]| -> Vec<String> {
            let mut command = aterm_spec::host::shell_command(&shell, &home);
            command
                .args(args)
                .arg(cmd)
                .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
                .env("POWERSHELL_UPDATECHECK", "Off")
                .env("ATERM_SESSION_ID", "atpkg-hook-test")
                .env_remove(crate::reroute::PASSTHROUGH_ENV)
                .env("ATPKG_TEST_HOOK", &hook);
            if let [path] = paths {
                command.env("ATPKG_TEST_PATH", path);
            } else {
                command.env("ATPKG_TEST_CASES", paths.len().to_string());
                for (i, path) in paths.iter().enumerate() {
                    command.env(format!("ATPKG_TEST_PATH_{i}"), path);
                }
            }
            let out = command
                .output()
                .unwrap_or_else(|error| panic!("{name} ({shell:?}): spawn: {error}"));
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success(),
                "{name}: exit {:?}; stdout={stdout:?} stderr={:?}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
            let lines: Vec<String> = stdout
                .lines()
                .map(|line| {
                    line.strip_prefix("PATH=")
                        .unwrap_or_else(|| panic!("{name}: not a PATH line: {stdout:?}"))
                        .to_owned()
                })
                .collect();
            assert_eq!(lines.len(), paths.len(), "{name}: one PATH line per case");
            lines
        };
        let spelled = |path: String| -> String {
            if name != "fish" {
                return path;
            }
            path.split(':')
                .map(|entry| if entry.is_empty() { "." } else { entry })
                .collect::<Vec<_>>()
                .join(":")
        };
        let want = spelled(want.clone());
        let mut cases = vec![
            (
                inherited.clone(),
                want.clone(),
                "reroute, agents, and bin first and once, the rest in order",
            ),
            (want.clone(), want.clone(), "idempotent"),
        ];
        for (path, expect) in [
            (format!(":{a}"), format!("{r}:{a}:{b}:")),
            (format!("{a}:"), format!("{r}:{a}:{b}:")),
            (format!("{a}::{a}"), format!("{r}:{a}:{b}:")),
            (format!("{r}::{a}"), format!("{r}:{a}:{b}:")),
            (a.to_string(), format!("{r}:{a}:{b}")),
            (r.to_string(), format!("{r}:{a}:{b}")),
        ] {
            cases.push((path, spelled(expect), "an edge"));
        }
        let paths: Vec<&str> = cases.iter().map(|(path, _, _)| path.as_str()).collect();
        let got = if name == "pwsh" {
            spawn(&paths)
        } else {
            paths.iter().flat_map(|path| spawn(&[path])).collect()
        };
        for ((path, expect, what), got) in cases.iter().zip(got) {
            assert_eq!(&got, expect, "{name}: {what}: {path:?}");
        }
    }
    let hook = root.join("hook.bash");
    let bash = aterm_spec::host::test_shell("bash").expect("bash on a unix host");
    let outside = |handle: Option<&Path>| -> String {
        let mut command = aterm_spec::host::shell_command(&bash, &home);
        command
            .args(["--noprofile", "--norc", "-c", posix_cmd])
            .env_remove(crate::reroute::PASSTHROUGH_ENV)
            .env_remove(crate::reroute::REROUTE_DIR_ENV)
            .env("ATPKG_TEST_HOOK", &hook)
            .env("ATPKG_TEST_PATH", &inherited);
        for marker in AGENTS_MARKERS {
            command.env_remove(marker);
        }
        if let Some(handle) = handle {
            command.env(crate::reroute::REROUTE_DIR_ENV, handle);
        }
        let out = command.output().expect("bash runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .trim_end()
            .strip_prefix("PATH=")
            .expect("a PATH line")
            .to_owned()
    };
    let rest = "/Users//u/.local/bin:/usr/bin:/opt/homebrew/bin::/bin";
    assert_eq!(
        outside(None),
        format!("{rest}:{b}"),
        "both demoted outside aterm"
    );
    assert_eq!(
        outside(Some(&reroute)),
        format!("{r}:{rest}:{b}"),
        "the reroute handle alone fronts the reroute dir"
    );
    let _ = fs::remove_dir_all(&root);
    aterm_spec::host::could_not_run_without(&absent);
}
