// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Chromium the browser lane drives: found, fetched to a cache at a
//! pinned version and sha256, and launched headless with its DevTools
//! protocol on two pipes.
//!
//! In order, the lane takes `ATERM_BROWSER` (any Chromium-family executable),
//! then the pinned chrome-headless-shell in this machine's cache
//! (`verify fetch-browser` puts it there), then an installed Chromium or
//! Chrome. None of them is a COULD NOT RUN naming the fetch.

use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::command;

/// The Chrome for Testing release the lane pins (the Stable channel's on
/// 2026-10-16, published 2026-10-14).
pub(crate) const PIN_VERSION: &str = "155.0.8059.84";

/// One platform's pinned `chrome-headless-shell-<platform>.zip`: its sha256
/// and size, measured on the download and cross-checked against the
/// bucket's own MD5 (`x-goog-hash`) when pinned.
pub(crate) struct Pinned {
    pub platform: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

/// The pinned archives, one per platform this lane can run on.
pub(crate) const PINNED: [Pinned; 3] = [
    Pinned {
        platform: "mac-arm64",
        sha256: "7f111980bcb41e908d0fa3e969a625e7777ff683c1fc43fd7759d05ec3ffe52b",
        bytes: 102_441_485,
    },
    Pinned {
        platform: "mac-x64",
        sha256: "3c5bf34eb8daa874e8b50edf900eab618b74c1b9596e596340278eeba4ff1ae1",
        bytes: 108_105_176,
    },
    Pinned {
        platform: "linux64",
        sha256: "a5d71b91aa996fef00517cd212b2dfb816b43055edefbe727e6fd562d4ae1f9c",
        bytes: 125_033_980,
    },
];

/// The environment variable naming a browser to drive instead.
pub(crate) const BROWSER_ENV: &str = "ATERM_BROWSER";

/// The fix a run without a browser names.
pub(crate) const FETCH_FIX: &str =
    "targo --unverified verify fetch-browser, or set ATERM_BROWSER to a Chromium executable";

/// Chrome for Testing's platform name for `(os, arch)`, where a pin exists.
pub(crate) fn platform_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("mac-arm64"),
        ("macos", "x86_64") => Some("mac-x64"),
        ("linux", "x86_64") => Some("linux64"),
        _ => None,
    }
}

/// This host's pinned archive, if it has one.
pub(crate) fn host_pin() -> Option<&'static Pinned> {
    let platform = platform_for(std::env::consts::OS, std::env::consts::ARCH)?;
    PINNED.iter().find(|p| p.platform == platform)
}

/// Where Chrome for Testing serves `platform`'s pinned archive.
pub(crate) fn archive_url(platform: &str) -> String {
    format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{PIN_VERSION}/{platform}/\
         chrome-headless-shell-{platform}.zip"
    )
}

/// The cache the pinned browser is unpacked into, aterm's `browser` in this
/// host's user cache ([`crate::user_cache`]): on macOS
/// `~/Library/Caches/aterm/browser.noindex`, elsewhere
/// `$XDG_CACHE_HOME/aterm/browser`, else `~/.cache/aterm/browser`. The lane's
/// Firefox is unpacked beside the Chromium.
pub(crate) fn host_cache_root() -> Option<PathBuf> {
    crate::host_user_cache("browser")
}

/// The unpacked pin's directory under `root`. It exists only complete: the
/// fetch unpacks beside it and renames.
fn pinned_dir(root: &Path, platform: &str) -> PathBuf {
    root.join(format!("chrome-headless-shell-{PIN_VERSION}-{platform}"))
}

/// The executable inside [`pinned_dir`].
fn pinned_binary(dir: &Path, platform: &str) -> PathBuf {
    dir.join(format!(
        "chrome-headless-shell-{platform}/chrome-headless-shell"
    ))
}

/// A browser the lane can drive.
#[derive(Debug, Clone)]
pub(crate) struct Browser {
    pub path: PathBuf,
    /// chrome-headless-shell is headless by construction; a full Chrome or
    /// Chromium is asked for `--headless`.
    pub shell: bool,
    /// Where it came from, for the report.
    pub source: String,
}

/// Browser executables installed where the platform puts them.
fn installed_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(target_os = "macos") {
        let apps = [
            "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            "Chromium.app/Contents/MacOS/Chromium",
            "Google Chrome.app/Contents/MacOS/Google Chrome",
        ];
        for app in apps {
            v.push(Path::new("/Applications").join(app));
            if let Some(h) = home {
                v.push(h.join("Applications").join(app));
            }
        }
    } else {
        let names = [
            "chrome-headless-shell",
            "chromium",
            "chromium-browser",
            "google-chrome",
            "google-chrome-stable",
        ];
        let path = std::env::var_os("PATH").unwrap_or_default();
        for dir in std::env::split_paths(&path) {
            for n in names {
                v.push(dir.join(n));
            }
        }
    }
    v
}

fn is_shell(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("headless-shell") || n.contains("headless_shell"))
}

/// The browser to drive, or why there is none (a COULD NOT RUN sentence,
/// naming [`FETCH_FIX`]).
pub(crate) fn find() -> Result<Browser, String> {
    if let Some(named) = std::env::var_os(BROWSER_ENV).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(named);
        if !path.is_file() {
            return Err(format!(
                "{BROWSER_ENV} names {}, which is not a file",
                path.display()
            ));
        }
        return Ok(Browser {
            shell: is_shell(&path),
            path,
            source: BROWSER_ENV.to_string(),
        });
    }
    if let (Some(pin), Some(root)) = (host_pin(), host_cache_root()) {
        let binary = pinned_binary(&pinned_dir(&root, pin.platform), pin.platform);
        if binary.is_file() {
            return Ok(Browser {
                path: binary,
                shell: true,
                source: format!("pinned chrome-headless-shell {PIN_VERSION}"),
            });
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(path) = installed_candidates(home.as_deref())
        .into_iter()
        .find(|p| p.is_file())
    {
        return Ok(Browser {
            shell: is_shell(&path),
            path,
            source: "installed".to_string(),
        });
    }
    Err(format!(
        "no Chromium: no {BROWSER_ENV}, no pinned chrome-headless-shell {PIN_VERSION} in {}, \
         none installed",
        host_cache_root().map_or_else(|| "the cache".to_string(), |p| p.display().to_string())
    ))
}

/// `verify fetch-browser`: the pinned chrome-headless-shell and Firefox for
/// this host, each downloaded, held to its sha256 and unpacked into the
/// cache. Prints each executable; exit 0, 1 on a failure, 3 where nothing
/// is pinned.
pub fn fetch_main(args: &[String]) -> i32 {
    if let Some(a) = args.first() {
        eprintln!(
            "verify fetch-browser: takes no arguments, not `{a}` (it fetches chrome-headless-shell \
             {PIN_VERSION} and Playwright's Firefox build {} for this host)",
            super::firefox::BUILD
        );
        return crate::exit::USAGE;
    }
    let Some(root) = host_cache_root() else {
        eprintln!("verify fetch-browser: COULD NOT RUN — no HOME to put the cache under");
        return crate::exit::COULD_NOT_RUN;
    };
    let chromium = host_pin().map(|pin| Archive {
        what: format!("chrome-headless-shell {PIN_VERSION}"),
        url: archive_url(pin.platform),
        sha256: pin.sha256,
        bytes: pin.bytes,
        dir: pinned_dir(&root, pin.platform),
        binary: pinned_binary(Path::new(""), pin.platform),
    });
    let firefox = super::firefox::host_pin().map(|pin| Archive {
        what: format!(
            "Playwright's Firefox build {} (Firefox {})",
            super::firefox::BUILD,
            super::firefox::VERSION
        ),
        url: super::firefox::archive_url(pin),
        sha256: pin.sha256,
        bytes: pin.bytes,
        dir: super::firefox::pinned_dir(&root, pin),
        binary: PathBuf::from(pin.binary),
    });
    let pinned: Vec<Archive> = chromium.into_iter().chain(firefox).collect();
    if pinned.is_empty() {
        eprintln!(
            "verify fetch-browser: COULD NOT RUN — no browser is pinned for {}-{}; set \
             {BROWSER_ENV} to a Chromium",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return crate::exit::COULD_NOT_RUN;
    }
    if super::firefox::host_pin().is_none() {
        eprintln!(
            "verify fetch-browser: no Firefox is pinned for {}-{}; the browser lane's Firefox \
             rows run only with {}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            super::firefox::FIREFOX_ENV
        );
    }
    let mut code = crate::exit::PASS;
    for archive in &pinned {
        match fetch(archive, &root) {
            Ok(binary) => println!("{}", binary.display()),
            Err(e) => {
                eprintln!("verify fetch-browser: FAIL — {}: {e}", archive.what);
                code = crate::exit::FAILED;
            }
        }
    }
    code
}

/// One archive `verify fetch-browser` unpacks into the cache: where it is
/// served, what it must hash to, the directory it is unpacked into, and its
/// executable inside that.
struct Archive {
    what: String,
    url: String,
    sha256: &'static str,
    bytes: u64,
    dir: PathBuf,
    binary: PathBuf,
}

/// Download, check and unpack `archive` under `root`; the executable.
fn fetch(archive: &Archive, root: &Path) -> Result<PathBuf, String> {
    let binary = archive.dir.join(&archive.binary);
    if binary.is_file() {
        eprintln!(
            "verify fetch-browser: {} already in the cache",
            archive.what
        );
        return Ok(binary);
    }
    std::fs::create_dir_all(root)
        .map_err(|e| format!("could not create {}: {e}", root.display()))?;
    // Each fetch owns its download and unpack directory. Concurrent fetches
    // can publish the same verified pin without removing each other's work.
    let scratch = aterm_tempfile::Builder::new()
        .prefix("browser-fetch-")
        .tempdir_in(root)
        .map_err(|e| format!("could not create scratch under {}: {e}", root.display()))?;
    let zip = scratch.path().join("browser.zip");
    let url = &archive.url;
    eprintln!(
        "verify fetch-browser: downloading {url} ({} MB)",
        archive.bytes / 1_000_000
    );
    run(
        command("curl")
            .args(["-sSfL", "--retry", "0", "-o"])
            .arg(&zip)
            .arg(url),
        "curl",
    )?;
    let got = crate::perf::arena::sha256_file(&zip)?;
    if got != archive.sha256 {
        return Err(format!(
            "{url} hashes to {got}, the pin is {} (crates/aterm-verify/src/browser/): the \
             archive changed under its version; nothing was unpacked",
            archive.sha256
        ));
    }
    // Only a completed, checked unpack is renamed into the shared cache.
    let staging = scratch.path().join("unpacked");
    if cfg!(target_os = "macos") {
        run(
            command("ditto").args(["-x", "-k"]).arg(&zip).arg(&staging),
            "ditto",
        )
    } else {
        run(
            command("unzip").arg("-q").arg(&zip).arg("-d").arg(&staging),
            "unzip",
        )
    }?;
    if !staging.join(&archive.binary).is_file() {
        return Err(format!("the archive holds no {}", archive.binary.display()));
    }
    if let Err(e) = std::fs::rename(&staging, &archive.dir) {
        // Another fetch may have published the same pin while this one unpacked.
        if !binary.is_file() {
            return Err(format!("could not place {}: {e}", archive.dir.display()));
        }
    }
    Ok(binary)
}

fn run(cmd: &mut aterm_uds::spawnfd::Command, what: &str) -> Result<(), String> {
    use aterm_uds::output::CommandOutput as _;
    let out = cmd
        .output_owned()
        .map_err(|e| format!("could not run {what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ))
    }
}

/// An ANGLE backend a session asks WebGL2 to run on (`--use-angle`). The
/// pinned shell offers two on macOS, Metal and SwiftShader; its log names
/// them when another is asked for (`allowed implementations:
/// [(gl=egl-angle,angle=metal),(gl=egl-angle,angle=swiftshader)]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Angle {
    /// The Mac's GPU, through ANGLE's Metal backend.
    Metal,
    /// SwiftShader, a CPU Vulkan under ANGLE: Chromium's software WebGL,
    /// no GPU needed.
    SwiftShader,
}

impl Angle {
    /// The `--use-angle` value, and the backend's name in the lane's rows.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Angle::Metal => "metal",
            Angle::SwiftShader => "swiftshader",
        }
    }

    /// Whether `adapter`, the unmasked renderer the GPU page reports, is
    /// this backend's. A browser that cannot give the backend asked for
    /// falls back to another (SwiftShader) without failing the page, so a
    /// session is held to the backend its adapter names.
    pub(crate) fn drew(self, adapter: &str) -> bool {
        adapter.contains(match self {
            Angle::Metal => "ANGLE Metal Renderer",
            Angle::SwiftShader => "SwiftShader",
        })
    }
}

/// The backends the lane runs, one browser session each: on macOS Metal
/// (the GPU the wgpu-WebGL recording was drawn on) and SwiftShader;
/// elsewhere SwiftShader, which needs no GPU.
pub(crate) fn angle_backends() -> &'static [Angle] {
    if cfg!(target_os = "macos") {
        &[Angle::Metal, Angle::SwiftShader]
    } else {
        &[Angle::SwiftShader]
    }
}

/// Whether the lane runs Chromium without its sandbox on `os`: on Linux.
/// Ubuntu 23.10 and later let no unconfined program make the user namespace
/// Chromium's sandbox needs (AppArmor's
/// `kernel.apparmor_restrict_unprivileged_userns`), and a chrome-headless-shell
/// unpacked into a user cache has neither an AppArmor profile nor a setuid
/// helper, so it aborts at start ("No usable sandbox!", measured with the
/// linux64 pin in the Linux lane's Ubuntu 24.04 guest). The lane's browser
/// opens only the pages its own loopback server serves, the same trust
/// `--enable-unsafe-swiftshader` already places in them. On macOS the
/// sandbox stays on.
pub(crate) fn sandbox_off(os: &str) -> bool {
    os == "linux"
}

/// The browser's arguments: headless, a private profile under `profile`,
/// DevTools on fds 3 and 4, WebGL2 on `angle`, without the sandbox where
/// [`sandbox_off`], and nothing that throttles a page the lane is waiting
/// on. Every session takes
/// `--enable-unsafe-swiftshader`, Chromium's opt-in to software WebGL for
/// trusted content. Under SwiftShader it silences the deprecation the
/// software path logs without it; under Metal it lets a GPU process that
/// cannot start fall back to SwiftShader (without it the page gets no
/// WebGL2 at all), and [`Angle::drew`] then names the session as not run
/// instead of the page failing for want of a GPU.
pub(crate) fn launch_args(
    browser: &Browser,
    profile: &Path,
    angle: Angle,
) -> Vec<std::ffi::OsString> {
    let mut v: Vec<std::ffi::OsString> = Vec::new();
    if !browser.shell {
        v.push("--headless".into());
    }
    let mut dir = std::ffi::OsString::from("--user-data-dir=");
    dir.push(profile);
    v.push(dir);
    for a in [
        "--remote-debugging-pipe",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        "--disable-background-timer-throttling",
        "--disable-renderer-backgrounding",
        "--disable-backgrounding-occluded-windows",
        "--ignore-gpu-blocklist",
        "--enable-unsafe-swiftshader",
        "--force-color-profile=srgb",
        "--force-device-scale-factor=1",
    ] {
        v.push(a.into());
    }
    if sandbox_off(std::env::consts::OS) {
        v.push("--no-sandbox".into());
    }
    v.push(format!("--use-angle={}", angle.name()).into());
    v.push("about:blank".into());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_pinned_host_maps_to_its_archive() {
        assert_eq!(platform_for("macos", "aarch64"), Some("mac-arm64"));
        assert_eq!(platform_for("macos", "x86_64"), Some("mac-x64"));
        assert_eq!(platform_for("linux", "x86_64"), Some("linux64"));
        assert_eq!(platform_for("linux", "aarch64"), None);
        assert_eq!(platform_for("windows", "x86_64"), None);
        assert_eq!(
            archive_url("mac-arm64"),
            format!(
                "https://storage.googleapis.com/chrome-for-testing-public/{PIN_VERSION}/\
                 mac-arm64/chrome-headless-shell-mac-arm64.zip"
            )
        );
    }

    #[test]
    fn a_full_browser_is_asked_for_headless_and_the_shell_is_not() {
        let profile = Path::new("/p");
        let shell = Browser {
            path: PathBuf::from("/c/chrome-headless-shell"),
            shell: true,
            source: String::new(),
        };
        let full = Browser {
            shell: false,
            ..shell.clone()
        };
        let has =
            |b: &Browser, a: &str| launch_args(b, profile, Angle::Metal).iter().any(|x| x == a);
        assert!(!has(&shell, "--headless") && has(&full, "--headless"));
        for b in [&shell, &full] {
            assert!(has(b, "--remote-debugging-pipe") && has(b, "--user-data-dir=/p"));
        }
        assert!(is_shell(Path::new("/x/chrome-headless-shell")));
        assert!(!is_shell(Path::new("/x/Google Chrome")));
        assert!(sandbox_off("linux") && !sandbox_off("macos"));
        assert_eq!(
            has(&shell, "--no-sandbox"),
            sandbox_off(std::env::consts::OS)
        );
    }

    /// Each backend asks for itself, and a session is held to the adapter
    /// the pinned shell reports for its backend: a Metal session that fell
    /// back to SwiftShader is not a Metal session.
    #[test]
    fn each_backend_asks_for_itself_and_knows_its_adapter() {
        let shell = Browser {
            path: PathBuf::from("/c/chrome-headless-shell"),
            shell: true,
            source: String::new(),
        };
        let args = |angle| launch_args(&shell, Path::new("/p"), angle);
        let has = |angle, a: &str| args(angle).iter().any(|x| x == a);
        assert!(has(Angle::Metal, "--use-angle=metal"));
        assert!(!has(Angle::Metal, "--use-angle=swiftshader"));
        assert!(has(Angle::SwiftShader, "--use-angle=swiftshader"));
        let metal = "ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Pro, Unspecified Version) (Gl)";
        let swiftshader = "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (LLVM 10.0.0) \
                           (0x0000C0DE)), SwiftShader driver) (Gl)";
        // The linux64 pin's SwiftShader JITs with Subzero, not LLVM.
        let subzero = "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero) \
                       (0x0000C0DE)), SwiftShader driver) (Gl)";
        assert!(Angle::Metal.drew(metal) && !Angle::Metal.drew(swiftshader));
        assert!(Angle::SwiftShader.drew(swiftshader) && !Angle::SwiftShader.drew(metal));
        assert!(Angle::SwiftShader.drew(subzero) && !Angle::Metal.drew(subzero));
        assert_eq!(angle_backends().last(), Some(&Angle::SwiftShader));
    }
}
