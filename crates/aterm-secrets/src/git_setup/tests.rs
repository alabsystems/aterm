// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn old(login: &str) -> String {
    format!(
        r#"!f() {{ [ "$1" = get ] || exit 0; u=; while IFS='=' read -r k v; do [ "$k" = username ] && u="$v"; done; case "$u" in alabsystems) t=gh_access_token_alabsystems ;; ''|{login}) u={login}; t=gh_access_token ;; *) exit 0 ;; esac; p=$(cat "$HOME/.secrets/$t" 2>/dev/null) && [ -n "$p" ] || {{ echo quit=1; exit 0; }}; printf 'username=%s\npassword=%s\n' "$u" "$p"; }}; f"#
    )
}

/// A scratch home under the temporary folder's real path: on macOS `$TMPDIR` is under `/var`, a
/// link to `/private/var`, and the helper refuses any linked component of its path.
fn fixture() -> aterm_tempfile::TempDir {
    let tmp = std::env::temp_dir().canonicalize().unwrap();
    let dir = aterm_tempfile::TempDir::new_in(tmp).unwrap();
    std::fs::create_dir_all(dir.path().join(".config/git")).unwrap();
    dir
}

fn config(path: &Path, helpers: &[&str]) {
    fsx::write_atomic(
        path,
        b"# preserved comment\n[user]\n\tname = Preserved\n[example]\n\tvalue = keep\n",
        0o600,
    )
    .unwrap();
    for helper in helpers {
        for key in KEYS {
            git(path, &["--add", key, helper]).unwrap();
        }
    }
}

#[test]
fn exact_legacy_mapping_is_transported_and_other_shell_programs_refuse() {
    let program = Path::new("/Users//alice/protected/aterm");
    for login in ["alice", "octo-cat", "user123"] {
        let value = old(login);
        assert_eq!(legacy(&value), Some(login));
        assert_eq!(
            replacement(&value, Path::new("/Users//alice"), program).unwrap(),
            Some(format!(
                "!/Users//alice/protected/aterm secrets git-credential --dev-user {login}"
            ))
        );
        for wrong in [
            value.replace("u=alice", "u=mallory"),
            value.replace("t=gh_access_token ;;", "t=gh_access_token_alabsystems ;;"),
            format!("{value}; unexpected"),
        ] {
            if wrong != value {
                assert!(replacement(&wrong, Path::new("/Users//alice"), program).is_err());
            }
        }
    }
}

#[test]
fn native_route_preserves_explicit_login_and_declines_custom_token_sources() {
    let home = Path::new("/Users//alice");
    let program = Path::new("/Users//alice/protected/aterm");
    assert_eq!(
        replacement(
            "!/Users//alice/.local/bin/aterm secrets git-credential --dev-user alice",
            home,
            program
        )
        .unwrap(),
        Some("!/Users//alice/protected/aterm secrets git-credential --dev-user alice".into())
    );
    for custom in [
        "store",
        "!aterm secrets git-credential",
        "!/Users//alice/.local/bin/aterm secrets git-credential --token-file /private/token",
        "!/Users//alice/.local/bin/aterm secrets git-credential --dev-user a; echo extra",
    ] {
        assert!(replacement(custom, home, program).is_err());
    }
}

#[test]
fn path_words_do_not_hide_shell_syntax_or_parent_components() {
    for path in [
        "relative/aterm",
        "/Users//alice/a b/aterm",
        "/Users//alice/a/../aterm",
        "/Users//alice/$(echo)/aterm",
        "/Users//alice/aterm;extra",
    ] {
        assert!(word(Path::new(path)).is_err(), "{path}");
    }
    assert!(word(Path::new("/Users//alice/.ubd/target.noindex/debug/aterm")).is_ok());
}

#[test]
fn actual_git_staging_preserves_other_keys_reset_order_and_original_bytes() {
    let home = fixture();
    let path = home.path().join(".config/git/config");
    config(&path, &["", &old("alice")]);
    let before = std::fs::read(&path).unwrap();
    let changes = plan(home.path(), Path::new("/Users//alice/protected/aterm")).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), before, "plan is read only");
    let stage = home.path().join("stage");
    fsx::write_atomic(&stage, changes[0].edited.as_slice(), 0o600).unwrap();
    assert_eq!(
        git(&stage, &["--get", "user.name"]).unwrap().stdout,
        b"Preserved\n"
    );
    assert_eq!(
        git(&stage, &["--get", "example.value"]).unwrap().stdout,
        b"keep\n"
    );
    assert!(
        std::str::from_utf8(changes[0].edited.as_slice())
            .unwrap()
            .contains("# preserved comment")
    );
    for key in KEYS {
        let values = git(&stage, &["--null", "--get-all", key]).unwrap().stdout;
        assert_eq!(
            values,
            b"\0!/Users//alice/protected/aterm secrets git-credential --dev-user alice\0"
        );
    }
}

#[test]
fn unknown_helper_refuses_whole_plan_without_touching_either_file() {
    let home = fixture();
    let first = home.path().join(".config/git/config");
    let second = home.path().join(".gitconfig");
    config(&first, &["", &old("alice")]);
    config(&second, &["!custom-helper"]);
    let original = [
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
    ];
    assert!(plan(home.path(), Path::new("/Users//alice/protected/aterm")).is_err());
    assert_eq!(std::fs::read(first).unwrap(), original[0]);
    assert_eq!(std::fs::read(second).unwrap(), original[1]);
}

#[test]
fn a_changed_or_replaced_input_declines_even_when_another_key_changed() {
    let home = fixture();
    let path = home.path().join(".config/git/config");
    config(&path, &[&old("alice")]);
    let changes = plan(home.path(), Path::new("/Users//alice/protected/aterm")).unwrap();
    git(&path, &["example.value", "peer edit"]).unwrap();
    assert!(unchanged(&changes[0]).is_err());
    fsx::write_atomic(&path, changes[0].original.as_slice(), 0o600).unwrap();
    assert!(
        unchanged(&changes[0]).is_err(),
        "same bytes with a different inode refuse"
    );
}

#[test]
fn lock_cleanup_does_not_remove_a_peer_replacement() {
    let home = fixture();
    let path = home.path().join("config.lock");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    let lock = Locked {
        path: path.clone(),
        file,
    };
    std::fs::remove_file(&path).unwrap();
    fsx::write_atomic(&path, b"peer", 0o600).unwrap();
    drop(lock);
    assert_eq!(std::fs::read(path).unwrap(), b"peer");
}

#[cfg(target_os = "macos")]
#[test]
fn linked_and_writable_executable_paths_refuse_without_permissions_changes() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = fixture();
    let path = home.path().join("aterm");
    fsx::write_atomic(&path, b"not executed", 0o777).unwrap();
    assert!(protected(&path).is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o777);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = home.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(protected(&link).is_err());
    assert!(
        std::fs::symlink_metadata(link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn developer_image_is_retained_by_content_and_a_different_existing_image_refuses() {
    let home = fixture();
    let program = home.path().join("source-program");
    fsx::write_atomic(&program, b"owned image bytes; never executed", 0o700).unwrap();
    let image = Image::prepare(home.path(), &program).unwrap();
    assert!(!image.path.exists(), "preparing creates nothing");
    image.install().unwrap();
    assert_eq!(std::fs::read(&image.path).unwrap(), image.bytes);
    assert!(
        !image
            .path
            .starts_with(program.parent().unwrap().join("target"))
    );
    image.install().unwrap();
    fsx::write_atomic(&image.path, b"changed own image", 0o700).unwrap();
    assert!(image.install().is_err());
    assert_eq!(std::fs::read(&image.path).unwrap(), b"changed own image");
}

#[cfg(target_os = "macos")]
#[test]
fn applied_config_and_exact_private_rollback_are_bound_to_the_owned_image() {
    let home = fixture();
    let program = home.path().join("source-program");
    fsx::write_atomic(&program, b"owned image bytes; never executed", 0o700).unwrap();
    let path = home.path().join(".config/git/config");
    config(&path, &["", &old("alice")]);
    let before = std::fs::read(&path).unwrap();
    let report = setup(home.path(), &program, true).unwrap();
    assert_eq!(report.files, std::slice::from_ref(&path));
    assert_eq!(
        git(&path, &["--get", "user.name"]).unwrap().stdout,
        b"Preserved\n"
    );
    let expected = format!(
        "\0!{} secrets git-credential --dev-user alice\0",
        report.program.display()
    );
    assert_eq!(
        git(&path, &["--null", "--get-all", KEYS[0]])
            .unwrap()
            .stdout,
        expected.as_bytes()
    );
    assert_eq!(
        std::fs::read(report.backup.unwrap().join("0.original")).unwrap(),
        before
    );
    assert!(report.program.is_file());
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    assert!(!Path::new(&lock).exists());
}
