// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_pin_maps_to_its_archive_and_cache_dir() {
    assert_eq!(platform_for("macos", "aarch64"), Some("mac-arm64"));
    assert_eq!(platform_for("macos", "x86_64"), None);
    assert_eq!(platform_for("linux", "aarch64"), Some("ubuntu-24.04-arm64"));
    assert_eq!(platform_for("linux", "x86_64"), None);
    assert_eq!(
        archive_url(&PINNED[1]),
        "https://cdn.playwright.dev/dbazure/download/playwright/builds/firefox/1543/\
         firefox-ubuntu-24.04-arm64.zip"
    );
    assert_eq!(
        archive_url(&PINNED[0]),
        "https://cdn.playwright.dev/dbazure/download/playwright/builds/firefox/1543/\
         firefox-mac-arm64.zip"
    );
    assert_eq!(
        pinned_dir(Path::new("/c"), &PINNED[0]),
        PathBuf::from("/c/firefox-1543-mac-arm64")
    );
}

/// Headless and on its own profile, opening the page; the profile holds
/// the prefs, and a second profile made over it starts empty again.
#[test]
fn firefox_runs_headless_on_a_fresh_profile() {
    let args = launch_args(Path::new("/p"), "http://127.0.0.1:1/cpu.html?post=1");
    assert_eq!(args[..4], ["-headless", "-no-remote", "-profile", "/p"]);
    assert_eq!(args[4], "http://127.0.0.1:1/cpu.html?post=1");
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let profile = dir.path().join("profile");
    make_profile(&profile).expect("profile");
    std::fs::write(profile.join("stale"), "x").expect("plant");
    make_profile(&profile).expect("again");
    assert!(!profile.join("stale").exists());
    assert!(home_of(&profile).is_dir());
    let prefs = std::fs::read_to_string(profile.join("user.js")).expect("prefs");
    assert!(prefs.contains("\"webgl.sanitize-unmasked-renderer\", false"));
}

/// Firefox's home, for POSIX and for Foundation, is the scratch one, set
/// once each; the rest of the environment passes through.
#[test]
fn firefox_runs_under_a_scratch_home() {
    let vars = [
        ("HOME", "/Users//o"),
        ("PATH", "/bin"),
        ("CFFIXED_USER_HOME", "/x"),
    ]
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    let env = launch_env(vars, Path::new("/p/home"));
    let get = |key: &str| {
        env.iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(get("HOME"), ["/p/home"]);
    assert_eq!(get("CFFIXED_USER_HOME"), ["/p/home"]);
    assert_eq!(get("PATH"), ["/bin"]);
}

/// A home whose folders Foundation locked against deletion is removed
/// all the same; plain removal cannot (the negative control).
#[cfg(target_os = "macos")]
#[test]
fn a_locked_scratch_home_is_removed() {
    use aterm_uds::output::CommandOutput as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let library = home.join("Library");
    std::fs::create_dir_all(&library).expect("library");
    let locked = aterm_uds::spawnfd::command("/bin/chmod")
        .args(["+a", "group:everyone deny delete"])
        .arg(&library)
        .output_owned()
        .expect("chmod +a");
    assert!(locked.status.success(), "chmod +a: {locked:?}");
    assert!(
        std::fs::remove_dir_all(&home).is_err(),
        "the lock held nothing"
    );
    remove_home(&home);
    assert!(!home.exists());
}

/// Linux needs an X display for WebGL2, set and not empty; macOS needs
/// none.
#[test]
fn firefox_on_linux_needs_a_display() {
    assert!(display_missing("linux", None).is_some());
    assert!(display_missing("linux", Some(OsStr::new(""))).is_some());
    assert!(display_missing("linux", Some(OsStr::new(":91"))).is_none());
    assert!(display_missing("macos", None).is_none());
}

/// Only a host with a pin is told to fetch one.
#[test]
fn the_fix_names_the_fetch_only_where_a_pin_exists() {
    assert!(fix_for(true).contains("verify provision"));
    assert!(!fix_for(false).contains("provision"));
    assert!(fix_for(false).contains(FIREFOX_ENV));
}

/// A Firefox user agent is Firefox's and names its product; Chromium's,
/// which also says "like Gecko", is not.
#[test]
fn only_firefox_s_user_agent_is_firefox() {
    let ff = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:155.0) Gecko/20100101 \
              Firefox/155.0";
    let chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                  (KHTML, like Gecko) HeadlessChrome/155.0.8059.84 Safari/537.36";
    assert!(is_firefox(ff) && !is_firefox(chrome) && !is_firefox(""));
    assert_eq!(product(ff), "Firefox/155.0");
}
