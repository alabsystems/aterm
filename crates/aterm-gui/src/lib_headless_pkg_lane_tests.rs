// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn a_headless_lane_runs_only_over_a_scratch_home() {
    use super::home_is_scratch;
    let dir = aterm_tempfile::TempDir::new().expect("owned homes");
    let account = dir.path().join("account");
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(account.join("child")).expect("account");
    std::fs::create_dir(&scratch).expect("scratch");
    assert!(home_is_scratch(Some(&scratch), Some(&account)));
    assert!(!home_is_scratch(Some(&account), Some(&account)));
    assert!(!home_is_scratch(
        Some(&account.join("child/..")),
        Some(&account)
    ));
    #[cfg(unix)]
    {
        let alias = dir.path().join("account-alias");
        std::os::unix::fs::symlink(&account, &alias).expect("account alias");
        assert!(!home_is_scratch(Some(&alias), Some(&account)));
        assert!(!home_is_scratch(Some(&account), Some(&alias)));
    }
    let missing = dir.path().join("missing");
    assert!(!home_is_scratch(Some(&missing), Some(&account)));
    assert!(!home_is_scratch(Some(&scratch), Some(&missing)));
    assert!(!home_is_scratch(Some(&scratch), None));
    assert!(!home_is_scratch(None, Some(&account)));
}

#[test]
fn the_primers_headless_seam_needs_a_scratch_config_folder() {
    use super::xdg_config_is_scratch as admits;
    let dir = aterm_tempfile::TempDir::new().expect("owned homes");
    let root = dir.path().join("users");
    let account = root.join("account");
    let beside = dir.path().join("scratch");
    let inside = account.join("tmp/scratch");
    for folder in [
        account.join(".config"),
        beside.join("home"),
        beside.join("cfg"),
        inside.join("home/.config"),
    ] {
        std::fs::create_dir_all(folder).expect("folders");
    }
    let own = account.join(".config");
    let home = beside.join("home");
    assert!(admits(None, Some(&home), Some(&account)), "unset");
    assert!(admits(
        Some(&beside.join("cfg")),
        Some(&home),
        Some(&account)
    ));
    assert!(admits(
        Some(&inside.join("home/.config")),
        Some(&inside.join("home")),
        Some(&account)
    ));
    assert!(!admits(Some(&own), Some(&home), Some(&account)));
    assert!(!admits(
        Some(&own),
        Some(&inside.join("home")),
        Some(&account)
    ));
    assert!(
        !admits(Some(&own), Some(&root), Some(&account)),
        "a home above the account's holds its files"
    );
    #[cfg(unix)]
    {
        let alias = dir.path().join("config-alias");
        std::os::unix::fs::symlink(&own, &alias).expect("config alias");
        assert!(!admits(Some(&alias), Some(&home), Some(&account)));
    }
    assert!(!admits(
        Some(&beside.join("missing")),
        Some(&home),
        Some(&account)
    ));
    assert!(!admits(Some(&beside.join("cfg")), Some(&home), None));
    assert!(!admits(
        Some(&beside.join("cfg")),
        Some(&home),
        Some(&root.join("missing"))
    ));
}

#[test]
fn a_headless_lane_runs_only_over_a_store_inside_its_scratch_home() {
    use super::store_is_under;
    let dir = aterm_tempfile::TempDir::new().expect("owned homes");
    let scratch = dir.path().join("scratch");
    let real = dir.path().join("LocalAppData");
    std::fs::create_dir(&scratch).expect("scratch");
    std::fs::create_dir(&real).expect("the person's own local app data");
    let pkg = |base: &std::path::Path| base.join("aterm").join("pkg");
    assert!(store_is_under(
        &pkg(&scratch.join("AppData").join("Local")),
        &scratch
    ));
    assert!(store_is_under(&scratch, &scratch));
    assert!(!store_is_under(&pkg(&real), &scratch));
    assert!(!store_is_under(
        &pkg(&scratch.join("..").join("LocalAppData")),
        &scratch
    ));
    assert!(!store_is_under(
        &scratch.join("not-made").join("..").join("..").join("x"),
        &scratch
    ));
    let out = scratch.join("out");
    #[cfg(windows)]
    aterm_uds::host::could_not_run_under_redirection_guard(
        "the test makes a link leading out of the scratch home (a junction, atpkg's link on \
         Windows) and reads where it leads",
    );
    atpkg::platform::atomic_symlink(&real, &out).expect("a link leading out");
    assert!(!store_is_under(&pkg(&out), &scratch));
    assert!(!store_is_under(&pkg(&scratch), &dir.path().join("missing")));
}
