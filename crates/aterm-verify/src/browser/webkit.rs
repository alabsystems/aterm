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
mod tests {
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
}
