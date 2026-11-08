// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_one_exact_tag_response_is_evidence() {
    let name = "refs/tags/aterm-verification/0123456789abcdef0123456789abcdef";
    let oid = "ab".repeat(20);
    assert_eq!(
        parse_ref(format!("{oid}\t{name}\n").as_bytes(), name).unwrap(),
        Some(oid.clone())
    );
    assert_eq!(parse_ref(b"", name).unwrap(), None);
    for text in [
        format!("{oid}\trefs/heads/main\n"),
        format!("{oid}\t{name}\n{oid}\t{name}\n"),
        format!("bad\t{name}\n"),
    ] {
        assert!(parse_ref(text.as_bytes(), name).is_err());
    }
}

#[test]
fn writer_routes_reject_secret_userinfo_foreign_hosts_and_other_repositories() {
    for url in [
        "https://github.com/fixture/aterm.git",
        "git@github.com:fixture/aterm.git",
        "https://writer@github.com/fixture/aterm.git",
    ] {
        assert!(crate::github_writer_origin(
            url, "fixture", "aterm", "writer"
        ));
    }
    for url in [
        "https://writer:secret@github.com/fixture/aterm.git",
        "https://other@github.com/fixture/aterm.git",
        "https://github.com.attacker/fixture/aterm.git",
        "https://github.com/fixture/another.git",
        "https://github.com/fixture/aterm.git?other",
        "https://writer%40other@github.com/fixture/aterm.git",
    ] {
        assert!(!crate::github_writer_origin(
            url, "fixture", "aterm", "writer"
        ));
    }
}

fn fixture(config: &str) -> (aterm_tempfile::TempDir, Ordinary, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    crate::items::github::need_native_git();
    let root = aterm_tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.path().join("home");
    let scratch = root.path().join("repository-0.git");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&scratch).unwrap();
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(home.join(".gitconfig"), config).unwrap();
    let mut git = Ordinary::new(Arc::new(|| Err(Error::new("unused identity"))));
    git.program = Some(crate::items::github::git_program(&crate::port::Live::new()));
    git.isolated_home = Some(home);
    (root, git, scratch)
}

#[test]
fn existing_same_repository_ssh_rewrite_is_preserved_without_a_network_call() {
    let config = "[url \"git@github.com:fixture/\"]\n insteadOf = https://github.com/fixture/\n";
    let (_root, git, scratch) = fixture(config);
    let current = || Ok(());
    let invocation = Invocation {
        owner: "fixture",
        principal: "writer",
        scratch: &scratch,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        current: &current,
    };
    let command = git.network(&invocation).unwrap();
    assert_eq!(
        std::fs::read_to_string(git.isolated_home.as_ref().unwrap().join(".gitconfig")).unwrap(),
        config
    );
    assert!(
        command
            .get_envs()
            .any(|(name, value)| name == "GIT_SSH_COMMAND"
                && value
                    .is_some_and(|v| v.to_string_lossy().contains("StrictHostKeyChecking=yes")))
    );
}

#[test]
fn a_foreign_fetch_rewrite_and_custom_ssh_command_are_not_repaired_or_run() {
    for config in [
        "[url \"git@github.com:foreign/\"]\n insteadOf = https://github.com/fixture/\n",
        "[core]\n sshCommand = dangerous-custom-command\n",
    ] {
        let (_root, git, scratch) = fixture(config);
        let current = || Ok(());
        let invocation = Invocation {
            owner: "fixture",
            principal: "writer",
            scratch: &scratch,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
            current: &current,
        };
        assert!(git.network(&invocation).is_err());
        assert_eq!(
            std::fs::read_to_string(git.isolated_home.as_ref().unwrap().join(".gitconfig"))
                .unwrap(),
            config
        );
    }
}

#[test]
fn revocation_before_the_git_child_preserves_a_retryable_unstarted_result() {
    let (_root, git, scratch) = fixture("");
    let current = || Err(Error::new("revoked before dispatch"));
    let invocation = Invocation {
        owner: "fixture",
        principal: "writer",
        scratch: &scratch,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        current: &current,
    };
    assert_eq!(
        git.create_tag(
            &invocation,
            "aterm-verification/0123456789abcdef0123456789abcdef",
            &"ab".repeat(20)
        ),
        Err(EffectError::NotStarted)
    );
    assert!(std::fs::read_dir(&scratch).unwrap().next().is_none());
}

#[test]
fn scratch_symlinks_and_unrelated_files_are_left_unchanged() {
    use std::os::unix::fs::symlink;
    let (root, git, scratch) = fixture("");
    let unrelated = scratch.join("valuable");
    std::fs::write(&unrelated, b"keep").unwrap();
    let current = || Ok(());
    let invocation = Invocation {
        owner: "fixture",
        principal: "writer",
        scratch: &scratch,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        current: &current,
    };
    assert!(git.network(&invocation).is_err());
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep");
    std::fs::remove_file(unrelated).unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, scratch.join("objects")).unwrap();
    assert!(git.network(&invocation).is_err());
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
}
