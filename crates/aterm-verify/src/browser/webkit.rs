// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Playwright WebKit browser verification runner ([`no_windows_skip`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Pinned Playwright WebKit build revision.
pub(crate) const BUILD: &str = "2359";

/// Pinned WebKit browser version string.
pub(crate) const VERSION: &str = "26.6";

/// One platform's pinned archive.
pub(crate) struct Pinned {
    pub platform: &'static str,
    pub archive: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub binary: &'static str,
}

/// Pinned WebKit archives indexed by host platform.
pub(crate) const PINNED: [Pinned; 1] = [Pinned {
    platform: "mac-arm64",
    archive: "webkit-mac-26-arm64.zip",
    sha256: "f0c43ff8a566ef9cf57b5c0e349d985c60e6ffeb7416e8aac34a5c911bbb8ca7",
    bytes: 81_853_194,
    binary: "Playwright.app/Contents/MacOS/Playwright",
}];

/// The environment variable naming a WebKit to run instead.
pub(crate) const WEBKIT_ENV: &str = "ATERM_WEBKIT";

/// What `--no-windows` answers for the WebKit row, with its fix ([`find`]).
pub(crate) fn no_windows_skip(missing: Option<&str>) -> String {
    let free = "run without --no-windows when the desktop is free";
    let fix = missing.map_or_else(|| free.to_string(), |fix| format!("{fix}, then {free}"));
    format!(
        "WebKit (--no-windows: Playwright's WebKit keeps its headless pages in windows of its \
         own — fix: {fix})"
    )
}

/// Remediation command suggested when WebKit is not installed.
pub(crate) fn fix_for(pinned: bool) -> &'static str {
    if pinned {
        "targo --unverified verify provision, or set ATERM_WEBKIT to a Playwright WebKit \
         executable"
    } else {
        "set ATERM_WEBKIT to a Playwright WebKit executable; none is pinned for this host"
    }
}

/// [`fix_for`] this host.
pub(crate) fn fix() -> &'static str {
    fix_for(host_pin().is_some())
}

/// The pinned platform for `(os, arch)`, where a pin exists.
pub(crate) fn platform_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("mac-arm64"),
        _ => None,
    }
}

/// This host's pinned archive, if it has one.
pub(crate) fn host_pin() -> Option<&'static Pinned> {
    let platform = platform_for(std::env::consts::OS, std::env::consts::ARCH)?;
    PINNED.iter().find(|p| p.platform == platform)
}

/// Where Playwright's CDN serves `pin`.
pub(crate) fn archive_url(pin: &Pinned) -> String {
    format!(
        "https://cdn.playwright.dev/dbazure/download/playwright/builds/webkit/{BUILD}/{}",
        pin.archive
    )
}

/// The unpacked pin's directory under the browser cache `root`.
pub(crate) fn pinned_dir(root: &Path, pin: &Pinned) -> PathBuf {
    root.join(format!("webkit-{BUILD}-{}", pin.platform))
}

/// The WebKit the lane runs.
#[derive(Debug, Clone)]
pub(crate) struct WebKit {
    pub path: PathBuf,
    /// Where it came from, for the report.
    pub source: String,
}

/// The WebKit to run, or why there is none (a COULD NOT RUN sentence).
pub(crate) fn find() -> Result<WebKit, String> {
    if let Some(named) = std::env::var_os(WEBKIT_ENV).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(named);
        if !path.is_file() {
            return Err(format!(
                "{WEBKIT_ENV} names {}, which is not a file",
                path.display()
            ));
        }
        return Ok(WebKit {
            path,
            source: WEBKIT_ENV.to_string(),
        });
    }
    let root = super::chromium::host_cache_root();
    let Some(pin) = host_pin() else {
        return Err(format!(
            "no WebKit: no {WEBKIT_ENV}, and none is pinned for {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    };
    let binary = root.as_deref().map(|r| pinned_dir(r, pin).join(pin.binary));
    match binary {
        Some(b) if b.is_file() => Ok(WebKit {
            path: b,
            source: format!("pinned Playwright WebKit build {BUILD} (WebKit {VERSION})"),
        }),
        _ => Err(format!(
            "no WebKit: no {WEBKIT_ENV}, no pinned Playwright WebKit build {BUILD} in {}",
            root.map_or_else(|| "the cache".to_string(), |p| p.display().to_string())
        )),
    }
}

/// The directory holding the WebKit frameworks `binary` loads.
pub(crate) fn frameworks_dir(binary: &Path) -> PathBuf {
    binary
        .ancestors()
        .find(|a| a.extension().is_some_and(|e| e == "app"))
        .and_then(Path::parent)
        .or_else(|| binary.parent())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// A fresh profile at `profile` ([`launch_env`], [`home_of`], [`data_of`]).
pub(crate) fn make_profile(profile: &Path) -> Result<(), String> {
    super::firefox::remove_home(&home_of(profile));
    let _ = std::fs::remove_dir_all(profile);
    for dir in [home_of(profile), data_of(profile)] {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// The scratch home inside `profile` that WebKit runs under.
pub(crate) fn home_of(profile: &Path) -> PathBuf {
    profile.join("home")
}

/// The website data directory inside `profile` (`--user-data-dir`).
pub(crate) fn data_of(profile: &Path) -> PathBuf {
    profile.join("data")
}

/// WebKit's environment ([`super::firefox::launch_env`]).
pub(crate) fn launch_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    home: &Path,
    frameworks: &Path,
) -> Vec<(OsString, OsString)> {
    const SET: [&str; 4] = [
        "HOME",
        "CFFIXED_USER_HOME",
        "DYLD_FRAMEWORK_PATH",
        "DYLD_LIBRARY_PATH",
    ];
    let mut env: Vec<(OsString, OsString)> = vars
        .into_iter()
        .filter(|(k, _)| !SET.iter().any(|s| k == s))
        .collect();
    for key in ["HOME", "CFFIXED_USER_HOME"] {
        env.push((key.into(), home.as_os_str().to_os_string()));
    }
    for key in ["DYLD_FRAMEWORK_PATH", "DYLD_LIBRARY_PATH"] {
        env.push((key.into(), frameworks.as_os_str().to_os_string()));
    }
    env
}

/// WebKit's arguments: headless, its website data in the private `profile`, opening `url`.
pub(crate) fn launch_args(profile: &Path, url: &str) -> Vec<OsString> {
    let mut data = OsString::from("--user-data-dir=");
    data.push(data_of(profile));
    vec!["--headless".into(), data, url.into()]
}

/// Whether `user_agent`, what the lane's pages report, is WebKit's.
pub(crate) fn is_webkit(user_agent: &str) -> bool {
    user_agent.contains("AppleWebKit/")
        && !user_agent.contains("Chrome/")
        && !user_agent.contains("Firefox/")
}

/// The session's product: `WebKit`, with Safari's `Version/<v>` where `user_agent` has one.
pub(crate) fn product(user_agent: &str) -> String {
    if !is_webkit(user_agent) {
        return user_agent.to_string();
    }
    user_agent
        .split_whitespace()
        .find(|w| w.starts_with("Version/"))
        .map_or_else(|| "WebKit".to_string(), |v| format!("WebKit {v}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_maps_to_its_archive_and_cache_dir() {
        assert_eq!(platform_for("macos", "aarch64"), Some("mac-arm64"));
        assert_eq!(platform_for("macos", "x86_64"), None);
        assert_eq!(platform_for("linux", "x86_64"), None);
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
        let args = launch_args(Path::new("/p"), "http://127.0.0.1:1/cpu.html?post=1");
        assert_eq!(
            args,
            [
                "--headless",
                "--user-data-dir=/p/data",
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
        let env = launch_env(vars, Path::new("/p/home"), Path::new("/w"));
        let get = |key: &str| {
            env.iter()
                .filter(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(get("HOME"), ["/p/home"]);
        assert_eq!(get("CFFIXED_USER_HOME"), ["/p/home"]);
        assert_eq!(get("DYLD_FRAMEWORK_PATH"), ["/w"]);
        assert_eq!(get("DYLD_LIBRARY_PATH"), ["/w"]);
        assert_eq!(get("PATH"), ["/bin"]);
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
}
