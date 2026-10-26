// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Chromium the browser lane drives: found, pinned at a version and sha256 that `verify
//! provision` fetches into a cache, and launched headless with its DevTools protocol on two pipes.

use std::path::{Path, PathBuf};

/// The pin itself, one source for this lane and the harness's headless sign-ins
/// ([`aterm_uds::host::chrome`]): the release, each platform's archive (its sha256 and size),
/// where it is served and where it unpacks.
pub(crate) use aterm_uds::host::chrome::{
    PIN_VERSION, PINNED, archive_url, host_pin, pinned_binary, pinned_dir, platform_for,
};

/// The environment variable naming a browser to drive instead.
pub(crate) const BROWSER_ENV: &str = "ATERM_BROWSER";

/// The fix a run without a browser names.
pub(crate) const FETCH_FIX: &str =
    "targo --unverified verify provision, or set ATERM_BROWSER to a Chromium executable";

/// The cache the pinned browser is unpacked into, aterm's `browser` in this host's user cache
/// ([`crate::user_cache`]): on macOS `~/Library/Caches/aterm/browser.noindex`, elsewhere
/// `$XDG_CACHE_HOME/aterm/browser`, else `~/.cache/aterm/browser`.
pub(crate) fn host_cache_root() -> Option<PathBuf> {
    crate::host_user_cache("browser")
}

/// A browser the lane can drive.
#[derive(Debug, Clone)]
pub(crate) struct Browser {
    pub path: PathBuf,
    /// chrome-headless-shell is headless by construction.
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

/// The browser `ATERM_BROWSER` names, when it is set: §8 rule 3's override, a
/// person's explicit choice.
fn named(value: Option<std::ffi::OsString>) -> Option<Result<Browser, String>> {
    let path = PathBuf::from(value.filter(|v| !v.is_empty())?);
    if !path.is_file() {
        return Some(Err(format!(
            "{BROWSER_ENV} names {}, which is not a file",
            path.display()
        )));
    }
    Some(Ok(Browser {
        shell: is_shell(&path),
        path,
        source: BROWSER_ENV.to_string(),
    }))
}

/// The browser lane's browser, or why there is none (a COULD NOT RUN sentence, naming
/// [`FETCH_FIX`]): `ATERM_BROWSER`, else the pinned chrome-headless-shell, else an
/// installed Chromium or Chrome (a host the pin does not cover, such as the Linux guest).
pub(crate) fn find() -> Result<Browser, String> {
    if let Some(named) = named(std::env::var_os(BROWSER_ENV)) {
        return named;
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

/// What the pinned shell answers to `Browser.getVersion` (`HeadlessChrome/<version>`):
/// the browser the wasm lane's floors were recorded in.
pub(crate) fn pinned_product() -> String {
    format!("HeadlessChrome/{PIN_VERSION}")
}

/// The browser the wasm drivers run in ([`mod@super::page`]), or why there is none:
/// `ATERM_BROWSER` when it is set, else the pinned chrome-headless-shell exactly as
/// `verify provision` placed it on this machine (its stamp admits the pin). Never an
/// installed Chrome or a copy laid by hand: what the checks and the bench run in is the
/// `chrome-headless-shell` row of `aterm_uds::host::TOOLS`.
pub(crate) fn pinned() -> Result<Browser, String> {
    pinned_from(
        std::env::var_os(BROWSER_ENV),
        crate::provision::this_machine(&crate::perf::workspace_root()).as_ref(),
    )
}

/// [`pinned`] for an `ATERM_BROWSER` value and a machine (`None`: no `$HOME` names its
/// caches).
pub(crate) fn pinned_from(
    env: Option<std::ffi::OsString>,
    machine: Option<&crate::provision::Machine>,
) -> Result<Browser, String> {
    if let Some(named) = named(env) {
        return named;
    }
    let Some(machine) = machine else {
        return Err("no $HOME names this user's cache, where the pinned browser lives".into());
    };
    match crate::provision::placed_browser(crate::provision::Engine::Chromium, machine) {
        Some(path) => Ok(Browser {
            path,
            shell: true,
            source: format!("pinned chrome-headless-shell {PIN_VERSION}"),
        }),
        None => Err(format!(
            "no pinned chrome-headless-shell {PIN_VERSION} placed in {} (an installed Chrome \
             is not the pin)",
            machine.browser.display()
        )),
    }
}

/// An ANGLE backend a session asks WebGL2 to run on (`--use-angle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Angle {
    /// The Mac's GPU, through ANGLE's Metal backend.
    Metal,
    /// SwiftShader, a CPU Vulkan under ANGLE: Chromium's software WebGL, no GPU needed.
    SwiftShader,
}

impl Angle {
    /// The `--use-angle` value, and the backend's name in the lane's rows.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Angle::Metal => "metal",
            Angle::SwiftShader => "swiftshader",
        }
    }

    /// Whether `adapter`, the unmasked renderer the GPU page reports, is this backend's.
    pub(crate) fn drew(self, adapter: &str) -> bool {
        adapter.contains(match self {
            Angle::Metal => "ANGLE Metal Renderer",
            Angle::SwiftShader => "SwiftShader",
        })
    }
}

/// The backends the lane runs, one browser session each: on macOS Metal (the GPU the wgpu-WebGL
/// recording was drawn on) and SwiftShader; elsewhere SwiftShader, which needs no GPU.
pub(crate) const fn angle_backends() -> &'static [Angle] {
    if cfg!(target_os = "macos") {
        &[Angle::Metal, Angle::SwiftShader]
    } else {
        &[Angle::SwiftShader]
    }
}

/// Whether the lane runs Chromium without its sandbox on `os`: on Linux.
pub(crate) fn sandbox_off(os: &str) -> bool {
    os == "linux"
}

/// The browser's arguments: headless, a private profile under `profile`, DevTools on fds 3 and 4,
/// WebGL2 on `angle`, without the sandbox where [`sandbox_off`], and nothing that throttles a page
/// the lane is waiting on. `extra` comes last before the first page (the wasm drivers'
/// `--js-flags=--expose-gc`).
pub(crate) fn launch_args(
    browser: &Browser,
    profile: &Path,
    angle: Angle,
    extra: &[&str],
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
    v.extend(extra.iter().map(Into::into));
    v.push("about:blank".into());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let has = |b: &Browser, a: &str| {
            launch_args(b, profile, Angle::Metal, &[])
                .iter()
                .any(|x| x == a)
        };
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
        let args = |angle| launch_args(&shell, Path::new("/p"), angle, &[]);
        let has = |angle, a: &str| args(angle).iter().any(|x| x == a);
        assert!(has(Angle::Metal, "--use-angle=metal"));
        assert!(!has(Angle::Metal, "--use-angle=swiftshader"));
        assert!(has(Angle::SwiftShader, "--use-angle=swiftshader"));
        let metal = "ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Pro, Unspecified Version) (Gl)";
        let swiftshader = "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (LLVM 10.0.0) \
                           (0x0000C0DE)), SwiftShader driver) (Gl)";
        let subzero = "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero) \
                       (0x0000C0DE)), SwiftShader driver) (Gl)";
        assert!(Angle::Metal.drew(metal) && !Angle::Metal.drew(swiftshader));
        assert!(Angle::SwiftShader.drew(swiftshader) && !Angle::SwiftShader.drew(metal));
        assert!(Angle::SwiftShader.drew(subzero) && !Angle::Metal.drew(subzero));
        assert_eq!(angle_backends().last(), Some(&Angle::SwiftShader));
    }
}
