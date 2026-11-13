// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_pin_maps_to_its_archive_and_cache_dir() {
    assert_eq!(platform_for("macos", "aarch64"), Some("mac-arm64"));
    assert_eq!(platform_for("macos", "x86_64"), None);
    assert_eq!(platform_for("linux", "x86_64"), None);
    assert_eq!(platform_for("linux", "aarch64"), Some("ubuntu-24.04-arm64"));
    assert_eq!(
        archive_url(&PINNED[1]),
        "https://cdn.playwright.dev/dbazure/download/playwright/builds/webkit/2359/\
         webkit-ubuntu-24.04-arm64.zip"
    );
    assert!(keeps_windows("macos") && !keeps_windows("linux"));
    assert_eq!(
        archive_url(&PINNED[0]),
        "https://cdn.playwright.dev/dbazure/download/playwright/builds/webkit/2359/\
         webkit-mac-26-arm64.zip"
    );
    assert_eq!(
        pinned_dir(Path::new("/c"), &PINNED[0]),
        PathBuf::from("/c/webkit-2359-mac-arm64")
    );
}

/// WPE's worker that got no WebGL2 context cannot run its row, which names this host's render
/// nodes or their absence; macOS's WebKit lacking it is a finding, and any other stop stays
/// as it is, neither reading the host's nodes.
#[test]
fn wpe_says_why_its_worker_row_cannot_run() {
    use super::super::Stop;
    let lacks = || Stop::Lacks("the worker page: no WebGL2".to_string());
    let unread = || -> Vec<String> { panic!("only WPE's lack reads the render nodes") };
    let none = worker_stop("linux", &Vec::new, lacks());
    let one = worker_stop(
        "linux",
        &|| vec!["/dev/dri/renderD128".to_string()],
        lacks(),
    );
    match (none, one) {
        (Stop::CouldNotRun(none), Stop::CouldNotRun(one)) => {
            assert!(none.starts_with("the worker page: no WebGL2; "), "{none}");
            assert!(!none.contains("renderD128") && one.contains("/dev/dri/renderD128"));
        }
        _ => panic!("WPE's lack on Linux is could-not-run"),
    }
    assert!(matches!(
        worker_stop("macos", &unread, lacks()),
        Stop::Fail(why) if why == "the worker page: no WebGL2"
    ));
    assert!(matches!(
        worker_stop("linux", &unread, Stop::Fail("drew wrong".to_string())),
        Stop::Fail(why) if why == "drew wrong"
    ));
    assert!(matches!(
        worker_stop("linux", &unread, Stop::CouldNotRun("exited".to_string())),
        Stop::CouldNotRun(why) if why == "exited"
    ));
}

/// The render nodes are a DRM directory's `renderD*` entries, sorted, and a host with no such
/// directory has none.
#[test]
fn the_render_nodes_are_the_drm_directory_s_render_entries() {
    let dri = aterm_tempfile::tempdir().expect("tempdir");
    for name in ["renderD129", "card0", "renderD128", "by-path"] {
        std::fs::write(dri.path().join(name), b"").expect("write");
    }
    let at = |name: &str| dri.path().join(name).display().to_string();
    assert_eq!(
        render_nodes(dri.path()),
        [at("renderD128"), at("renderD129")]
    );
    assert!(render_nodes(&dri.path().join("absent")).is_empty());
}

/// The frameworks are the build's directory around its `.app`; a bare
/// executable's are beside it.
#[test]
fn the_frameworks_are_found_around_the_app() {
    assert_eq!(
        frameworks_dir(Path::new(
            "/c/webkit-2359-mac-arm64/Playwright.app/Contents/MacOS/Playwright"
        )),
        PathBuf::from("/c/webkit-2359-mac-arm64")
    );
    assert_eq!(
        frameworks_dir(Path::new("/b/Release/MiniBrowser")),
        PathBuf::from("/b/Release")
    );
}

/// Headless, its website data in the profile, opening the page; a second
/// profile made over the first starts empty again.
#[test]
fn webkit_runs_headless_on_a_fresh_profile() {
    let args = |os| launch_args(Path::new("/p"), "http://127.0.0.1:1/cpu.html?post=1", os);
    assert_eq!(
        args("macos"),
        [
            "--headless",
            "--user-data-dir=/p/data",
            "http://127.0.0.1:1/cpu.html?post=1"
        ]
    );
    assert_eq!(
        args("linux"),
        [
            "--headless",
            "--user-data-dir=/p/data",
            "--features=+AllowWebGLInWorkers",
            "http://127.0.0.1:1/cpu.html?post=1"
        ]
    );
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let profile = dir.path().join("profile");
    make_profile(&profile).expect("profile");
    std::fs::write(data_of(&profile).join("stale"), "x").expect("plant");
    make_profile(&profile).expect("again");
    assert!(!data_of(&profile).join("stale").exists());
    assert!(home_of(&profile).is_dir());
}

/// The home, for POSIX and for Foundation, is the scratch one and the
/// frameworks are the build's, each set once; the rest passes through.
#[test]
fn webkit_runs_under_a_scratch_home_with_its_frameworks() {
    let vars = [
        ("HOME", "/Users//o"),
        ("PATH", "/bin"),
        ("CFFIXED_USER_HOME", "/x"),
        ("DYLD_FRAMEWORK_PATH", "/elsewhere"),
    ]
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    let get = |env: &[(OsString, OsString)], key: &str| {
        env.iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect::<Vec<_>>()
    };
    let env = launch_env(vars.clone(), Path::new("/p/home"), Some(Path::new("/w")));
    assert_eq!(get(&env, "HOME"), ["/p/home"]);
    assert_eq!(get(&env, "CFFIXED_USER_HOME"), ["/p/home"]);
    assert_eq!(get(&env, "DYLD_FRAMEWORK_PATH"), ["/w"]);
    assert_eq!(get(&env, "DYLD_LIBRARY_PATH"), ["/w"]);
    assert_eq!(get(&env, "PATH"), ["/bin"]);
    let linux = launch_env(vars, Path::new("/p/home"), None);
    assert_eq!(get(&linux, "HOME"), ["/p/home"]);
    assert!(get(&linux, "CFFIXED_USER_HOME").is_empty());
    assert!(get(&linux, "DYLD_FRAMEWORK_PATH").is_empty());
    assert_eq!(get(&linux, "PATH"), ["/bin"]);
}

/// Only a host with a pin is told to fetch one.
#[test]
fn the_fix_names_the_fetch_only_where_a_pin_exists() {
    assert!(fix_for(true).contains("verify provision"));
    assert!(!fix_for(false).contains("provision"));
    assert!(fix_for(false).contains(WEBKIT_ENV));
}

/// The `--no-windows` skip's fix is dropping the flag where a WebKit is
/// there to run; where none is, the fix that gets one comes first, since
/// dropping the flag alone would only reach COULD NOT RUN.
#[test]
fn the_no_windows_skip_names_a_webkit_first_where_none_is() {
    let found = no_windows_skip(None);
    assert!(found.contains("fix: run without --no-windows"), "{found}");
    assert!(!found.contains(WEBKIT_ENV), "{found}");
    let unpinned = no_windows_skip(Some(fix_for(false)));
    assert!(
        unpinned.contains(&format!("fix: {}, then run without", fix_for(false))),
        "{unpinned}"
    );
    assert!(
        unpinned.contains("none is pinned for this host"),
        "{unpinned}"
    );
}

/// Playwright's build's bare user agent (as its pages answered it) and
/// Safari's are WebKit's; Chromium's and Firefox's are not, though
/// Chromium's says `AppleWebKit/` and `Safari/` too.
#[test]
fn only_webkit_s_user_agent_is_webkit() {
    let bare = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, \
                like Gecko)";
    let wk = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, \
              like Gecko) Version/26.6 Safari/605.1.15";
    let chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                  (KHTML, like Gecko) HeadlessChrome/155.0.8059.84 Safari/537.36";
    let ff = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:155.0) Gecko/20100101 \
              Firefox/155.0";
    assert!(is_webkit(bare) && is_webkit(wk));
    assert!(!is_webkit(chrome) && !is_webkit(ff) && !is_webkit(""));
    assert_eq!(product(wk), "WebKit Version/26.6");
    assert_eq!(product(bare), "WebKit");
    assert_eq!(product(chrome), chrome);
}
