// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The C toolchain: a PREREQUISITE atpkg DETECTS and never ships
//! (`docs/DESIGN-clt-prerequisite-2026-08-31.md`).
//!
//! A Trust build compiles C at build-script time (`zstd`, `ring`) and links through `cc`
//! (`trustc --print link-args` names `"cc"`), so a machine with the managed toolchain and
//! no C compiler dies at its first `targo build` while `install --default-set` said
//! "complete". A C toolchain is a machine capability — atpkg can neither version, pin, gc
//! nor uninstall it, and Apple's may not be redistributed — so it is probed, reported in
//! `doctor` and after the default set, and never installed here.
//!
//! **The probe never raises Apple's install dialog.** On macOS `/usr/bin/cc` is one of the
//! libxcselect shims (the same file as `/usr/bin/git`, `make`, `clang`): on a Mac with no
//! developer tools, RUNNING it requests an install and blocks until a human answers. So a
//! `cc` that resolves into the shim directory is never spawned: the active developer
//! directory is read off `/var/db/xcode_select_link` (a symlink; no process), and only
//! when it holds a `clang` does the probe ask `xcrun --find clang`, bounded. A `cc`
//! anywhere else — Homebrew, nix, a distro gcc — is not a shim and answers `--version`,
//! bounded. `$CC` is not consulted: rustc does not consult it for linking.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What the probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CcVerdict {
    /// A C toolchain answered: the driver that did (the resolved clang, or the `cc`).
    Ready {
        /// The compiler that answered.
        driver: PathBuf,
    },
    /// macOS: `cc` is the libxcselect shim and no developer directory holds a `clang`,
    /// so running it would only raise Apple's install dialog.
    NoDeveloperDir,
    /// No `cc` on `PATH` at all.
    NoDriver,
    /// The compiler ran and refused: its first stderr line (or its exit), verbatim.
    Broken {
        /// The compiler that was asked.
        driver: PathBuf,
        /// What it said.
        why: String,
    },
    /// The compiler did not answer inside the probe's bound — unknown, not broken.
    Unanswered {
        /// The compiler that was asked.
        driver: PathBuf,
    },
    /// A target where a `cc` test is the wrong question (Windows: rustc owns MSVC
    /// discovery).
    NotProbed,
}

impl CcVerdict {
    /// Whether this verdict means a Trust build cannot compile here: the three that are
    /// answers, not unknowns.
    #[must_use]
    pub fn blocks_builds(&self) -> bool {
        matches!(
            self,
            Self::NoDeveloperDir | Self::NoDriver | Self::Broken { .. }
        )
    }

    /// The one sentence every surface prints for this verdict — `doctor` and the
    /// default-set tail spell it the same way.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Ready { driver } => {
                format!("C toolchain: {} answers", driver.display())
            }
            Self::NoDeveloperDir => format!(
                "no C toolchain: `cc` is macOS's install shim and no developer directory holds \
                 a clang, so Rust builds stop at the first build script that compiles C. fix: \
                 {}",
                act()
            ),
            Self::NoDriver => format!(
                "no C compiler (`cc`) on PATH, so Rust builds stop at the first build script \
                 that compiles C. fix: {}",
                act()
            ),
            Self::Broken { driver, why } => format!(
                "the C compiler at {} does not work ({why}), so Rust builds stop at the first \
                 build script that compiles C. fix: {}",
                driver.display(),
                act()
            ),
            Self::Unanswered { driver } => format!(
                "the C compiler at {} did not answer within {} s — whether Rust builds can \
                 compile C here is unknown",
                driver.display(),
                PROBE_TIMEOUT.as_secs()
            ),
            Self::NotProbed => {
                "C toolchain not probed on this target (rustc finds MSVC itself)".to_string()
            }
        }
    }
}

/// The ONE act that installs a C toolchain on this platform.
#[must_use]
pub fn act() -> &'static str {
    if cfg!(target_os = "macos") {
        "xcode-select --install"
    } else if cfg!(windows) {
        "install the Visual Studio Build Tools (C++ workload)"
    } else {
        "install your distribution's C compiler (e.g. `sudo apt install build-essential` or \
         `sudo dnf install gcc`)"
    }
}

/// How long one compiler probe may take — `doctor`'s probe bound.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the probe looks, beyond `PATH`: injected so a test can stand a fake shim
/// directory and a fake developer-directory link in for the machine's.
#[derive(Clone, Debug)]
pub struct CcHost {
    /// The directory whose `cc` is the libxcselect install shim — `/usr/bin` on macOS,
    /// `None` elsewhere (no such shim).
    pub shim_dir: Option<PathBuf>,
    /// The symlink naming the active developer directory (`xcode-select -s`), read
    /// without spawning anything.
    pub xcode_select_link: PathBuf,
    /// The developer directory when the link is absent: where the Command Line Tools
    /// install.
    pub default_developer_dir: PathBuf,
    /// How long the one compiler probe may take: [`PROBE_TIMEOUT`] on this machine. A
    /// test that needs an ANSWER from a script it just wrote widens it — the first exec
    /// of a new file from a new test binary waits on the system's code assessment, which
    /// a loaded machine has been measured to stretch past the production bound — and
    /// the wedged-compiler test narrows it.
    pub bound: Duration,
}

impl CcHost {
    /// This machine's.
    #[must_use]
    pub fn this_machine() -> Self {
        Self {
            shim_dir: cfg!(target_os = "macos").then(|| PathBuf::from("/usr/bin")),
            xcode_select_link: PathBuf::from("/var/db/xcode_select_link"),
            default_developer_dir: PathBuf::from("/Library/Developer/CommandLineTools"),
            bound: PROBE_TIMEOUT,
        }
    }
}

/// Probe this machine's C toolchain against `path_var` (the `PATH` a build would see).
#[must_use]
pub fn probe(path_var: Option<&OsStr>) -> CcVerdict {
    probe_with(path_var, &CcHost::this_machine())
}

/// [`probe`] over an injected host.
#[must_use]
pub fn probe_with(path_var: Option<&OsStr>, host: &CcHost) -> CcVerdict {
    if cfg!(windows) {
        return CcVerdict::NotProbed;
    }
    let Some(cc) = path_var.and_then(|path| {
        std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("cc"))
            .find(|candidate| is_executable(candidate))
    }) else {
        return CcVerdict::NoDriver;
    };
    let is_shim = host
        .shim_dir
        .as_deref()
        .is_some_and(|shim| cc.parent() == Some(shim));
    if !is_shim {
        return ask(&cc, &cc, &["--version"], host.bound);
    }
    // THE SHIM: never spawned. Only a developer directory that really holds a clang
    // makes it safe to ask Apple's own resolver which one it would run.
    let developer_dir = std::fs::read_link(&host.xcode_select_link)
        .unwrap_or_else(|_| host.default_developer_dir.clone());
    let holds_clang = [
        developer_dir.join("usr/bin/clang"),
        developer_dir.join("Toolchains/XcodeDefault.xctoolchain/usr/bin/clang"),
    ]
    .iter()
    .any(|clang| is_executable(clang));
    if !holds_clang {
        return CcVerdict::NoDeveloperDir;
    }
    let shim_dir = cc.parent().unwrap_or(Path::new("/usr/bin"));
    ask(
        &shim_dir.join("xcrun"),
        &cc,
        &["--find", "clang"],
        host.bound,
    )
}

/// Run `bin args…` bounded by `bound` ([`PROBE_TIMEOUT`] on this machine) and read its
/// answer as a verdict about `driver`: exit 0 is ready (naming the path `xcrun` printed,
/// when it printed one), any other exit is broken with the first non-empty stderr line.
fn ask(bin: &Path, driver: &Path, args: &[&str], bound: Duration) -> CcVerdict {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    match crate::doctor::output_bounded_capturing(&mut cmd, bound) {
        None => CcVerdict::Unanswered {
            driver: driver.to_path_buf(),
        },
        Some(out) if out.status.success() => {
            let printed = String::from_utf8_lossy(&out.stdout);
            let named = printed
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| line.starts_with('/'))
                .map(PathBuf::from);
            CcVerdict::Ready {
                driver: named.unwrap_or_else(|| driver.to_path_buf()),
            }
        }
        Some(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let why = stderr
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map_or_else(
                    || match out.status.code() {
                        Some(code) => format!("exit {code}"),
                        None => "killed by a signal".to_string(),
                    },
                    str::to_string,
                );
            CcVerdict::Broken {
                driver: driver.to_path_buf(),
                why,
            }
        }
    }
}

/// A regular file this user may execute (never a directory named `cc`).
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atpkg-prereq-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The bound for a probe whose script must ANSWER: the first exec of a file this test
    /// just wrote waits on the system's code assessment of it (and of this test binary),
    /// which a loaded machine stretched past [`PROBE_TIMEOUT`] (2026-09-26, load ~30 on 18
    /// cores: both answering cases read `Unanswered` at exactly 5 s, and passed on the
    /// re-run that found the assessment cached). The bound is the probe's job; what these
    /// cases test is the reading of the answer.
    const ANSWER: Duration = Duration::from_secs(120);

    /// No shim directory: every `cc` is a plain compiler.
    fn plain_host(root: &Path) -> CcHost {
        CcHost {
            shim_dir: None,
            xcode_select_link: root.join("no-link"),
            default_developer_dir: root.join("no-developer-dir"),
            bound: ANSWER,
        }
    }

    /// A stubbed PATH: no `cc` is NoDriver, a `cc` that answers is Ready, one that
    /// refuses is Broken with its own first stderr line — and the three that are answers
    /// are the ones that block builds.
    #[test]
    fn a_plain_cc_is_asked_its_version_on_the_path_it_is_handed() {
        let root = scratch("plain");
        let host = plain_host(&root);
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            probe_with(Some(empty.as_os_str()), &host),
            CcVerdict::NoDriver
        );
        assert_eq!(probe_with(None, &host), CcVerdict::NoDriver);
        assert!(CcVerdict::NoDriver.blocks_builds());

        let good = root.join("good");
        script(&good.join("cc"), "echo 'cc version 1'");
        let path = std::env::join_paths([empty.clone(), good.clone()]).unwrap();
        assert_eq!(
            probe_with(Some(&path), &host),
            CcVerdict::Ready {
                driver: good.join("cc")
            }
        );

        let bad = root.join("bad");
        script(
            &bad.join("cc"),
            "echo '' >&2; echo 'cc: no libc headers' >&2; exit 1",
        );
        let verdict = probe_with(Some(bad.as_os_str()), &host);
        assert_eq!(
            verdict,
            CcVerdict::Broken {
                driver: bad.join("cc"),
                why: "cc: no libc headers".into()
            }
        );
        assert!(verdict.blocks_builds());
        assert!(
            verdict.line().ends_with(&format!("fix: {}", act())),
            "{}",
            verdict.line()
        );
        // A directory named `cc`, or a file nobody may run, is not a compiler.
        let odd = root.join("odd");
        std::fs::create_dir_all(odd.join("cc")).unwrap();
        assert_eq!(
            probe_with(Some(odd.as_os_str()), &host),
            CcVerdict::NoDriver
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE SHIM IS NEVER SPAWNED. A `cc` in the shim directory with no developer
    /// directory holding a clang is NoDeveloperDir without running anything — the stub
    /// `cc` and `xcrun` here would leave a mark if they ran. With a developer directory
    /// that holds one, `xcrun --find clang` answers and names the compiler.
    #[test]
    fn the_install_shim_is_read_never_run() {
        let root = scratch("shim");
        let shim = root.join("usr-bin");
        let ran = root.join("ran");
        script(&shim.join("cc"), &format!("touch '{}'", ran.display()));
        let clang_dir = root.join("CommandLineTools");
        script(
            &shim.join("xcrun"),
            &format!("echo '{}/usr/bin/clang'", clang_dir.display()),
        );
        let host = CcHost {
            shim_dir: Some(shim.clone()),
            xcode_select_link: root.join("xcode_select_link"),
            default_developer_dir: clang_dir.clone(),
            bound: ANSWER,
        };
        assert_eq!(
            probe_with(Some(shim.as_os_str()), &host),
            CcVerdict::NoDeveloperDir
        );
        assert!(!ran.exists(), "the shim was spawned");

        // The Command Line Tools arrive: xcrun is asked, the shim still is not.
        script(&clang_dir.join("usr/bin/clang"), "exit 0");
        assert_eq!(
            probe_with(Some(shim.as_os_str()), &host),
            CcVerdict::Ready {
                driver: clang_dir.join("usr/bin/clang")
            }
        );
        assert!(!ran.exists(), "the shim was spawned");

        // `xcode-select -s` names another developer dir — an Xcode.app with no clang
        // in its toolchain: nothing to run again.
        let xcode = root.join("Xcode.app/Contents/Developer");
        std::fs::create_dir_all(&xcode).unwrap();
        std::os::unix::fs::symlink(&xcode, &host.xcode_select_link).unwrap();
        assert_eq!(
            probe_with(Some(shim.as_os_str()), &host),
            CcVerdict::NoDeveloperDir
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A compiler that hangs is unknown, not broken, and the probe returns inside its
    /// bound — and this machine's bound is the production one.
    #[test]
    fn a_wedged_compiler_is_unanswered_not_broken() {
        assert_eq!(CcHost::this_machine().bound, PROBE_TIMEOUT);
        let root = scratch("wedged");
        let dir = root.join("slow");
        script(&dir.join("cc"), "sleep 30");
        let bound = Duration::from_secs(2);
        let host = CcHost {
            bound,
            ..plain_host(&root)
        };
        let started = std::time::Instant::now();
        let verdict = probe_with(Some(dir.as_os_str()), &host);
        assert!(started.elapsed() < bound + Duration::from_secs(5));
        assert_eq!(
            verdict,
            CcVerdict::Unanswered {
                driver: dir.join("cc")
            }
        );
        assert!(!verdict.blocks_builds());
        let _ = std::fs::remove_dir_all(&root);
    }
}
