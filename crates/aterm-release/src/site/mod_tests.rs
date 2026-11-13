// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_the_canonical_public_release_clone_is_anonymous() {
    assert!(is_public_release_clone(
        "https://github.com/alabsystems/aterm.git",
        "alabsystems/aterm"
    ));
    assert!(!is_public_release_clone(
        "https://github.com/private/aterm.git",
        "alabsystems/aterm"
    ));
    assert!(!is_public_release_clone(
        "https://token@github.com/alabsystems/aterm.git",
        "alabsystems/aterm"
    ));
}

#[test]
fn an_installer_uses_the_site_file_bound() {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join(INSTALLER_SOURCE);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let mut text = "OFFICIAL_REPO_SLUG=\"alabsystems/aterm\"\n#".to_owned();
    text.extend(std::iter::repeat_n(
        ' ',
        MAX_SITE_FILE_BYTES as usize - text.len(),
    ));
    std::fs::write(&source, &text).unwrap();
    assert_eq!(installer(root.path(), "alabsystems/aterm").unwrap(), text);
    assert_eq!(read_text(&source).as_deref(), Some(text.as_str()));

    text.push('\n');
    std::fs::write(&source, text).unwrap();
    assert!(installer(root.path(), "alabsystems/aterm").is_err());
    assert!(read_text(&source).is_none());
}

/// A sync removes a scratch an earlier one kept only once its hold is spent (a restart
/// here); a kept one still held, one with no stamp (a sync's live scratch) and a
/// directory named otherwise stay.
#[test]
fn a_kept_scratch_goes_once_its_hold_is_spent() {
    let base = crate::test_scratch_dir("site");
    let dir = |name: &str, stamp: Option<String>| {
        let path = base.path().join(name);
        std::fs::create_dir_all(path.join("clone/target")).unwrap();
        if let Some(stamp) = stamp {
            std::fs::write(path.join(aterm_uds::deadline::EXEC_STAMP), stamp).unwrap();
        }
        path
    };
    let spent = dir("aterm-site-41-0", Some(u64::MAX.to_string()));
    let spent_noindex = dir("aterm-site-41-2.noindex", Some(u64::MAX.to_string()));
    let held = dir("aterm-site-41-1", None);
    aterm_uds::deadline::stamp_exec(&held);
    let held_noindex = dir("aterm-site-41-3.noindex", None);
    aterm_uds::deadline::stamp_exec(&held_noindex);
    let live = dir("aterm-site-42-0.noindex", None);
    let other = dir("aterm-site-sync-x", Some(u64::MAX.to_string()));
    reclaim_kept(base.path());
    assert!(!spent.exists());
    assert!(!spent_noindex.exists());
    for stays in [&held, &held_noindex, &live, &other] {
        assert!(stays.join("clone/target").is_dir(), "{}", stays.display());
    }
}

#[test]
fn the_command_line_is_the_hooks() {
    let o = parse_args(["--latest", "--site-dir", "/s", "--no-deploy"])
        .unwrap()
        .unwrap();
    assert!(o.latest && o.no_deploy && !o.dry_run);
    assert_eq!(o.site_dir.as_deref(), Some(Path::new("/s")));
    assert_eq!(parse_args(["--help"]).unwrap(), None);
    assert!(parse_args(["--site-dir"]).is_err());
    assert!(parse_args(["--bogus"]).is_err());
    assert!(
        parse_args(["--latest", "--redeploy"])
            .unwrap()
            .unwrap()
            .redeploy
    );
    let why = parse_args(["--redeploy", "--no-deploy"]).unwrap_err();
    assert!(why.contains("--redeploy"), "{why}");
}

#[test]
fn key_check_cannot_be_combined_with_site_changes() {
    let parsed = parse_args(["--check-key", "--site-dir", "/site"])
        .unwrap()
        .unwrap();
    assert!(parsed.check_key);
    assert_eq!(parsed.site_dir.as_deref(), Some(Path::new("/site")));
    for flag in [
        "--latest",
        "--dry-run",
        "--no-deploy",
        "--redeploy",
        "--skip-site",
    ] {
        assert!(parse_args(["--check-key", flag]).is_err(), "{flag}");
    }
    assert!(parse_args(["--check-key", "--releases", "/list"]).is_err());
    assert!(parse_args(["--check-key", "--check-access"]).is_err());
    assert!(parse_args(["--check-access", "--redeploy"]).is_err());
    assert!(parse_args(["--check-access", "--dry-run"]).is_err());
    assert!(
        parse_args(["--check-access"])
            .unwrap()
            .unwrap()
            .check_access
    );
}

#[test]
fn a_local_release_list_is_for_a_dry_run_only() {
    let why = parse_args(["--latest", "--releases", "/l.json"]).unwrap_err();
    assert!(why.contains("needs --dry-run"), "{why}");
    let o = parse_args(["--releases", "/l.json", "--dry-run"])
        .unwrap()
        .unwrap();
    assert_eq!(o.releases.as_deref(), Some(Path::new("/l.json")));
}

#[test]
fn a_site_toml_that_does_not_parse_is_named_never_ignored() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-site-settings-")
        .tempdir()
        .unwrap();
    assert!(settings(dir.path()).unwrap().dir.is_none());
    std::fs::create_dir_all(dir.path().join("publish")).unwrap();
    let file = dir.path().join("publish/site.toml");
    std::fs::write(&file, "dir = \"~/site\"\n").unwrap();
    assert_eq!(settings(dir.path()).unwrap().dir.as_deref(), Some("~/site"));
    std::fs::write(&file, "dirr = \"~/site\"\n").unwrap();
    let why = settings(dir.path()).unwrap_err();
    assert!(why.contains(&file.display().to_string()), "{why}");
    assert!(why.contains("dirr"), "{why}");
    assert!(Env::from_process(dir.path(), None).is_err());
}

#[test]
fn private_dirs_are_distinct_and_private() {
    let base = crate::test_scratch_dir("priv");
    let made: Vec<PathBuf> = (0..8)
        .map(|_| private_dir_in(base.path(), "aterm-site-t").unwrap())
        .collect();
    let unique: std::collections::BTreeSet<&PathBuf> = made.iter().collect();
    assert_eq!(unique.len(), made.len());
    for d in &made {
        assert_eq!(d.extension().and_then(|e| e.to_str()), Some("noindex"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(d.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        }
    }
}
