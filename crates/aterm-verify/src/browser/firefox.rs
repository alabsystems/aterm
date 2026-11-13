// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless Firefox (Gecko) discovery, profile setup, and launch helpers for the browser lane.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Pinned Playwright Firefox build number.
pub(crate) const BUILD: &str = "1543";

/// The Firefox version that build is.
pub(crate) const VERSION: &str = "155.0";

/// Platform archive metadata for a pinned Firefox build.
pub(crate) struct Pinned {
    pub platform: &'static str,
    pub archive: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub binary: &'static str,
}

/// Pinned Firefox archives per supported platform.
pub(crate) const PINNED: [Pinned; 2] = [
    Pinned {
        platform: "mac-arm64",
        archive: "firefox-mac-arm64.zip",
        sha256: "12798eac57cad33a466d7315ddae1350a326dedd167ee4ac73307425f4fa8f28",
        bytes: 109_388_736,
        binary: "firefox/Nightly.app/Contents/MacOS/firefox",
    },
    Pinned {
        platform: "ubuntu-24.04-arm64",
        archive: "firefox-ubuntu-24.04-arm64.zip",
        sha256: "10a0c716eb93ee7e57aabf8d626bd35b5c4007517c90708f67634f640a47a569",
        bytes: 105_297_546,
        binary: "firefox/firefox",
    },
];

/// The environment variable naming a Firefox to run instead.
pub(crate) const FIREFOX_ENV: &str = "ATERM_FIREFOX";

/// Remediation hint when Firefox is unavailable.
pub(crate) const fn fix_for(pinned: bool) -> &'static str {
    if pinned {
        "targo --unverified verify provision, or set ATERM_FIREFOX to a Firefox executable"
    } else {
        "set ATERM_FIREFOX to a Firefox executable; none is pinned for this host"
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
        ("linux", "aarch64") => Some("ubuntu-24.04-arm64"),
        _ => None,
    }
}

/// Returns a skip reason if `os` requires an X11 `DISPLAY` that is unset or empty.
pub(crate) fn display_missing(os: &str, display: Option<&OsStr>) -> Option<String> {
    (os == "linux" && display.is_none_or(OsStr::is_empty)).then(|| {
        "Firefox on Linux makes its WebGL2 context on an X display, even headless, and \
         DISPLAY is not set (an Xvfb will do)"
            .to_string()
    })
}

/// This host's pinned archive, if it has one.
pub(crate) fn host_pin() -> Option<&'static Pinned> {
    let platform = platform_for(std::env::consts::OS, std::env::consts::ARCH)?;
    PINNED.iter().find(|p| p.platform == platform)
}

/// Where Playwright's CDN serves `pin`.
pub(crate) fn archive_url(pin: &Pinned) -> String {
    format!(
        "https://cdn.playwright.dev/dbazure/download/playwright/builds/firefox/{BUILD}/{}",
        pin.archive
    )
}

/// The unpacked pin's directory under the browser cache `root`.
pub(crate) fn pinned_dir(root: &Path, pin: &Pinned) -> PathBuf {
    root.join(format!("firefox-{BUILD}-{}", pin.platform))
}

/// The Firefox the lane runs.
#[derive(Debug, Clone)]
pub(crate) struct Firefox {
    pub path: PathBuf,
    /// Where it came from, for the report.
    pub source: String,
}

/// The Firefox to run, or why there is none (a COULD NOT RUN sentence).
pub(crate) fn find() -> Result<Firefox, String> {
    if let Some(named) = std::env::var_os(FIREFOX_ENV).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(named);
        if !path.is_file() {
            return Err(format!(
                "{FIREFOX_ENV} names {}, which is not a file",
                path.display()
            ));
        }
        return Ok(Firefox {
            path,
            source: FIREFOX_ENV.to_string(),
        });
    }
    let root = super::chromium::host_cache_root();
    let Some(pin) = host_pin() else {
        return Err(format!(
            "no Firefox: no {FIREFOX_ENV}, and none is pinned for {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    };
    let binary = root.as_deref().map(|r| pinned_dir(r, pin).join(pin.binary));
    match binary {
        Some(b) if b.is_file() => Ok(Firefox {
            path: b,
            source: format!("pinned Playwright Firefox build {BUILD} (Firefox {VERSION})"),
        }),
        _ => Err(format!(
            "no Firefox: no {FIREFOX_ENV}, no pinned Playwright Firefox build {BUILD} in {}",
            root.map_or_else(|| "the cache".to_string(), |p| p.display().to_string())
        )),
    }
}

/// Profile `user.js` preferences for headless WebGL2 verification.
pub(crate) const PREFS: &str = "\
user_pref(\"webgl.sanitize-unmasked-renderer\", false);
user_pref(\"webgl.force-enabled\", true);
user_pref(\"privacy.reduceTimerPrecision\", false);
user_pref(\"privacy.resistFingerprinting\", false);
user_pref(\"privacy.fingerprintingProtection\", false);
user_pref(\"gfx.color_management.mode\", 0);
user_pref(\"layout.css.devPixelsPerPx\", \"1.0\");
user_pref(\"browser.shell.checkDefaultBrowser\", false);
user_pref(\"browser.startup.homepage_override.mstone\", \"ignore\");
user_pref(\"browser.aboutwelcome.enabled\", false);
user_pref(\"app.update.disabledForTesting\", true);
user_pref(\"datareporting.policy.dataSubmissionEnabled\", false);
user_pref(\"toolkit.telemetry.reportingpolicy.firstRun\", false);
";

/// Initializes a fresh Firefox profile directory at `profile` with [`PREFS`].
pub(crate) fn make_profile(profile: &Path) -> Result<(), String> {
    remove_home(&home_of(profile));
    let _ = std::fs::remove_dir_all(profile);
    let home = home_of(profile);
    std::fs::create_dir_all(&home)
        .map_err(|e| format!("could not create {}: {e}", home.display()))?;
    let prefs = profile.join("user.js");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
    }
    opts.open(&prefs)
        .and_then(|mut f| {
            use std::io::Write as _;
            f.write_all(PREFS.as_bytes())
        })
        .map_err(|e| format!("could not write {}: {e}", prefs.display()))
}

/// The scratch home inside `profile` that Firefox runs under.
pub(crate) fn home_of(profile: &Path) -> PathBuf {
    profile.join("home")
}

/// Removes the scratch `home` directory used by Firefox.
pub(crate) fn remove_home(home: &Path) {
    if !home.exists() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        use aterm_uds::output::CommandOutput as _;
        let _ = aterm_uds::spawnfd::command("/bin/chmod")
            .args(["-R", "-N"])
            .arg(home)
            .output_owned();
    }
    let _ = std::fs::remove_dir_all(home);
}

/// Builds Firefox's environment with `HOME` and `CFFIXED_USER_HOME` pointed at `home`.
pub(crate) fn launch_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    home: &Path,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = vars
        .into_iter()
        .filter(|(k, _)| k != "HOME" && k != "CFFIXED_USER_HOME")
        .collect();
    for key in ["HOME", "CFFIXED_USER_HOME"] {
        env.push((key.into(), home.as_os_str().to_os_string()));
    }
    env
}

/// Returns headless Firefox command-line arguments for `profile` and `url`.
pub(crate) fn launch_args(profile: &Path, url: &str) -> Vec<OsString> {
    vec![
        "-headless".into(),
        "-no-remote".into(),
        "-profile".into(),
        profile.as_os_str().to_os_string(),
        url.into(),
    ]
}

/// Returns whether `user_agent` identifies a Gecko/Firefox browser.
pub(crate) fn is_firefox(user_agent: &str) -> bool {
    user_agent.contains("Gecko/")
        && user_agent.contains("Firefox/")
        && !user_agent.contains("Chrome/")
}

/// `Firefox/<version>` out of `user_agent`, the session's product.
pub(crate) fn product(user_agent: &str) -> String {
    user_agent
        .split_whitespace()
        .find(|w| w.starts_with("Firefox/"))
        .unwrap_or(user_agent)
        .to_string()
}

#[cfg(test)]
#[path = "firefox_tests.rs"]
mod tests;
