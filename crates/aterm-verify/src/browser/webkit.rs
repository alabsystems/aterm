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

/// Pinned WebKit archives indexed by host platform: on macOS the Playwright.app, on Linux the
/// bundle's `pw_run.sh`, which runs its WPE MiniBrowser (headless, no display) under `--headless`.
pub(crate) const PINNED: [Pinned; 2] = [
    Pinned {
        platform: "mac-arm64",
        archive: "webkit-mac-26-arm64.zip",
        sha256: "f0c43ff8a566ef9cf57b5c0e349d985c60e6ffeb7416e8aac34a5c911bbb8ca7",
        bytes: 81_853_194,
        binary: "Playwright.app/Contents/MacOS/Playwright",
    },
    Pinned {
        platform: "ubuntu-24.04-arm64",
        archive: "webkit-ubuntu-24.04-arm64.zip",
        sha256: "01e84b5cc2b4e4a39ded958c0fe464bd0a156d9a0fe3af053cf4cf3fce3b6116",
        bytes: 100_936_204,
        binary: "pw_run.sh",
    },
];

/// The environment variable naming a WebKit to run instead.
pub(crate) const WEBKIT_ENV: &str = "ATERM_WEBKIT";

/// Whether this host's WebKit keeps its headless pages in windows of its own: macOS's
/// Playwright.app does; Linux's WPE MiniBrowser draws headless with no display.
pub(crate) const fn keeps_windows(os: &str) -> bool {
    matches!(os.as_bytes(), b"macos")
}

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
pub(crate) const fn fix_for(pinned: bool) -> &'static str {
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
        ("linux", "aarch64") => Some("ubuntu-24.04-arm64"),
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

/// WebKit's environment ([`super::firefox::launch_env`]): the scratch home, and on macOS
/// (`frameworks`: [`frameworks_dir`]) Foundation's home and the build's frameworks; Linux's
/// bundle sets its own library path.
pub(crate) fn launch_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    home: &Path,
    frameworks: Option<&Path>,
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
    env.push(("HOME".into(), home.as_os_str().to_os_string()));
    let Some(frameworks) = frameworks else {
        return env;
    };
    env.push(("CFFIXED_USER_HOME".into(), home.as_os_str().to_os_string()));
    for key in ["DYLD_FRAMEWORK_PATH", "DYLD_LIBRARY_PATH"] {
        env.push((key.into(), frameworks.as_os_str().to_os_string()));
    }
    env
}

/// The WebKit feature a worker's OffscreenCanvas needs for a WebGL context, on by default in
/// macOS's WebKit and off in WPE's. WebKit makes a worker no in-process context (ANGLE is not
/// multithreaded), only one in its GPU process, and WPE's GPU process draws WebGL only on a GBM
/// display (a DRM render node whose EGL imports dma-bufs). A worker's `getContext("webgl2")`
/// still answers null on the lane's VM, which has no render node, and on the GPU instance, whose
/// virtio-gpu render node gives such a display (through kms_swrast), so the worker row cannot run
/// (`worker.mjs`, [`worker_stop`]). Build 2359's MiniBrowser offers no
/// `UseGPUProcessForWebGLEnabled` feature ("is not available"), though its library names that
/// preference, so it keeps its default.
pub(crate) const WEBGL_IN_WORKERS: &str = "AllowWebGLInWorkers";

/// Where a Linux host keeps its DRM nodes.
pub(super) const DRI: &str = "/dev/dri";

/// The DRM render nodes (`renderD*`) in `dri`, sorted: where WPE's GPU process can make the GBM
/// display a worker's WebGL needs ([`WEBGL_IN_WORKERS`]). None where `dri` cannot be read.
pub(super) fn render_nodes(dri: &Path) -> Vec<String> {
    let mut nodes: Vec<String> = std::fs::read_dir(dri)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        .map(|e| e.path().display().to_string())
        .collect();
    nodes.sort();
    nodes
}

/// The worker row's stop on `os`. Where WPE's worker got no WebGL2 context at all
/// ([`super::Stop::Lacks`]) on Linux, the row cannot run, and it says why and what this host's
/// `render_nodes` ([`render_nodes`]) are; macOS's WebKit gives a worker WebGL2 (the lane has
/// measured it), so there the lack is a finding. Any other stop stays as it is.
pub(super) fn worker_stop(
    os: &str,
    render_nodes: &dyn Fn() -> Vec<String>,
    stop: super::Stop,
) -> super::Stop {
    match stop {
        super::Stop::Lacks(why) if keeps_windows(os) => super::Stop::Fail(why),
        super::Stop::Lacks(why) => {
            let nodes = render_nodes();
            let host = if nodes.is_empty() {
                format!("this host has no DRM render node (no renderD* in {DRI})")
            } else {
                format!("it made none on this host's {}", nodes.join(", "))
            };
            super::Stop::CouldNotRun(format!(
                "{why}; WPE makes a worker's WebGL only in its GPU process, on a GBM display \
                 whose EGL imports dma-bufs, and {host}"
            ))
        }
        other => other,
    }
}

/// WebKit's arguments on `os`: headless, its website data in the private `profile`, opening
/// `url`; on Linux, WPE's MiniBrowser with [`WEBGL_IN_WORKERS`] on, so the worker row runs the
/// WebGL path macOS's WebKit runs by default.
pub(crate) fn launch_args(profile: &Path, url: &str, os: &str) -> Vec<OsString> {
    let mut data = OsString::from("--user-data-dir=");
    data.push(data_of(profile));
    let mut args: Vec<OsString> = vec!["--headless".into(), data];
    if !keeps_windows(os) {
        args.push(format!("--features=+{WEBGL_IN_WORKERS}").into());
    }
    args.push(url.into());
    args
}

/// Whether `user_agent`, what the lane's pages report, is WebKit's.
pub(crate) fn is_webkit(user_agent: &str) -> bool {
    user_agent.contains("AppleWebKit/")
        && !user_agent.contains("Chrome/")
        && !user_agent.contains("Firefox/")
}

/// The session's product: `WebKit`, with Safari's `Version/<v>` where `user_agent` has one;
/// another browser's user agent as it is.
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
#[path = "webkit_tests.rs"]
mod tests;
