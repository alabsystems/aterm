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
//! `cc` that resolves into the shim directory is never spawned until the machine has shown
//! it holds a compiler: the active developer directory is read off
//! `/var/db/xcode_select_link` (a symlink; no process), and only when it holds a `clang`
//! does the probe ask `xcrun --find cc`, bounded — and only when THAT names a `cc` is the
//! shim itself run. A `cc` anywhere else — Homebrew, nix, a distro gcc — is not a shim and
//! answers `--version`, bounded. `$CC` is not consulted: rustc does not consult it for
//! linking.
//!
//! **A compiler that answers is then made to BUILD** (the design's §Design 2): the `cc` a
//! build would run compiles and links a one-line `#include <stdio.h>` program in a private
//! (0700, freshly created) directory under the temp root, bounded, and the verdict is
//! `Ready` only when it exits 0 AND wrote a non-empty program. The program is never
//! executed (it would be the wrong question under Rosetta or a cross toolchain). That
//! catches what `--version` cannot: a gcc with no libc headers (Linux without
//! `libc6-dev`), a developer directory whose SDK is gone, a linker that refuses — each a
//! `Broken` naming the compiler's first stderr line. A temp root the probe cannot make
//! its directory in is `Unproven`, never `Broken`: a read-only temp says nothing about the
//! toolchain. On macOS the program goes through the shim `cc`, not the resolved clang:
//! the shim is what supplies the SDK (a bare CLT clang cannot find `stdio.h`, measured
//! 2026-09-27), and it is what rustc and `cc-rs` run.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What the probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CcVerdict {
    /// The `cc` a build would run compiled and linked the probe program. `driver` names
    /// that compiler: on macOS the one the shim forwards to (`xcrun --find cc`, e.g.
    /// `…/CommandLineTools/usr/bin/cc`), elsewhere the `cc` found on `PATH`.
    Ready {
        /// The compiler that built the probe program.
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
    /// The compiler answered, but the build probe could not run (no private directory
    /// under the temp root, or its source could not be written) — unknown, not broken.
    Unproven {
        /// The compiler that answered.
        driver: PathBuf,
        /// Why the build probe could not run.
        why: String,
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

    /// The one act that fixes this verdict, when a single command does: installing a
    /// toolchain fixes a MISSING one. A compiler that exists and cannot build names its
    /// own cause (the stderr line [`Self::line`] quotes). Off macOS that cause is most
    /// often libc's missing headers, which the install act supplies; on macOS the
    /// compiler that refused is already installed — an unaccepted Xcode license, a
    /// PATH-first `cc` with no SDK — and `xcode-select --install` only replies that the
    /// tools are there, so there is no act to name. `None` for every verdict that does
    /// not block builds.
    #[must_use]
    pub fn fix(&self) -> Option<&'static str> {
        match self {
            Self::NoDeveloperDir | Self::NoDriver => Some(act()),
            Self::Broken { .. } if !cfg!(target_os = "macos") => Some(act()),
            _ => None,
        }
    }

    /// The one sentence every surface prints for this verdict — `doctor` and the
    /// default-set tail spell it the same way.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Ready { driver } => {
                format!("C toolchain: {} builds a C program", driver.display())
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
                "the C compiler at {} cannot build a program ({why}), so Rust builds stop at \
                 the first build script that compiles C. {}",
                driver.display(),
                self.fix().map_or_else(
                    || "fix what it reports".to_string(),
                    |act| format!("fix: {act}")
                )
            ),
            Self::Unanswered { driver } => format!(
                "the C compiler at {} did not answer within {} s — whether Rust builds can \
                 compile C here is unknown",
                driver.display(),
                PROBE_TIMEOUT.as_secs()
            ),
            Self::Unproven { driver, why } => format!(
                "the C compiler at {} answers, but the build probe could not run ({why}) — \
                 whether Rust builds can compile C here is unknown",
                driver.display()
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
    /// How long each compiler probe may take: [`PROBE_TIMEOUT`] on this machine. A
    /// test that needs an ANSWER from a script it just wrote widens it — the first exec
    /// of a new file from a new test binary waits on the system's code assessment, which
    /// a loaded machine has been measured to stretch past the production bound — and
    /// the wedged-compiler test narrows it.
    pub bound: Duration,
    /// Where the build probe makes its private directory: the system temp directory on
    /// this machine.
    pub tmp_root: PathBuf,
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
            tmp_root: std::env::temp_dir(),
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
        return match ask(&cc, &cc, &["--version"], host.bound) {
            CcVerdict::Ready { driver } => build_probe(&cc, driver, path_var, host),
            other => other,
        };
    }
    // THE SHIM: never spawned until `xcrun` names a compiler behind it. Only a
    // developer directory that really holds a clang makes it safe to ask Apple's
    // own resolver which one it would run.
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
    // `xcrun --find cc` names the very tool the shim forwards to; only when it does is
    // the shim itself run, to build the probe.
    let shim_dir = cc.parent().unwrap_or(Path::new("/usr/bin"));
    match ask(&shim_dir.join("xcrun"), &cc, &["--find", "cc"], host.bound) {
        CcVerdict::Ready { driver } => build_probe(&cc, driver, path_var, host),
        other => other,
    }
}

/// The one C program the build probe compiles: the include proves the headers (the SDK
/// on macOS, libc's on Linux) resolve.
const PROBE_SOURCE: &str = "#include <stdio.h>\nint main(void) { return 0; }\n";

/// Make `cc` compile and link [`PROBE_SOURCE`] in a private directory under
/// `host.tmp_root`, bounded by `host.bound`: `Ready { driver: resolved }` only when it
/// exits 0 AND wrote a non-empty program, which is never run. The directory is removed on
/// every path.
fn build_probe(cc: &Path, resolved: PathBuf, path_var: Option<&OsStr>, host: &CcHost) -> CcVerdict {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = host
        .tmp_root
        .join(format!("atpkg-cc-{}-{nanos}", std::process::id()));
    if let Err(error) = private_dir(&dir) {
        return CcVerdict::Unproven {
            driver: resolved,
            why: format!(
                "no private directory under {}: {error}",
                host.tmp_root.display()
            ),
        };
    }
    let _removed = RemoveOnDrop(&dir);
    let source = dir.join("probe.c");
    let program = dir.join("probe.out");
    if let Err(error) = std::fs::write(&source, PROBE_SOURCE) {
        return CcVerdict::Unproven {
            driver: resolved,
            why: format!("could not write {}: {error}", source.display()),
        };
    }
    let mut cmd = std::process::Command::new(cc);
    cmd.arg("-o").arg(&program).arg(&source).current_dir(&dir);
    if let Some(path) = path_var {
        cmd.env("PATH", path);
    }
    match crate::doctor::output_bounded_capturing(&mut cmd, host.bound) {
        None => CcVerdict::Unanswered {
            driver: cc.to_path_buf(),
        },
        Some(out) if out.status.success() => {
            let wrote = std::fs::metadata(&program).is_ok_and(|m| m.is_file() && m.len() > 0);
            if wrote {
                CcVerdict::Ready { driver: resolved }
            } else {
                CcVerdict::Broken {
                    driver: cc.to_path_buf(),
                    why: "it reported success but wrote no program".to_string(),
                }
            }
        }
        Some(out) => CcVerdict::Broken {
            driver: cc.to_path_buf(),
            why: first_stderr_line(&out),
        },
    }
}

/// Create `dir` — never an existing path (an existing name is a refusal, not a
/// hijack) — readable by this user only.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    // One builder per cfg arm: a shared `let mut` that only the unix arm mutates is an
    // `unused_mut` on every other target, which a `-D warnings` lint there refuses.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::DirBuilder::new().create(dir)
    }
}

/// Removes the probe's directory however the probe ends.
struct RemoveOnDrop<'a>(&'a Path);

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0);
    }
}

/// A refusing compiler's first non-empty stderr line, or its exit when it said nothing.
fn first_stderr_line(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(
            || match out.status.code() {
                Some(code) => format!("exit {code}"),
                None => "killed by a signal".to_string(),
            },
            str::to_string,
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
        Some(out) => CcVerdict::Broken {
            driver: driver.to_path_buf(),
            why: first_stderr_line(&out),
        },
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

    /// No shim directory: every `cc` is a plain compiler; the build probe works under
    /// `<root>/tmp`.
    fn plain_host(root: &Path) -> CcHost {
        let tmp_root = root.join("tmp");
        std::fs::create_dir_all(&tmp_root).unwrap();
        CcHost {
            shim_dir: None,
            xcode_select_link: root.join("no-link"),
            default_developer_dir: root.join("no-developer-dir"),
            bound: ANSWER,
            tmp_root,
        }
    }

    /// A fake `cc` that answers `--version` and, asked `-o <out> <src>`, BUILDS: it writes
    /// `<out>` as a runnable script that would leave `<ran>` behind if anything ran it.
    fn building_cc(path: &Path, ran: &Path) {
        script(
            path,
            &format!(
                "case \"$1\" in --version) echo 'cc version 1';; -o) printf '#!/bin/sh\\ntouch {}\\n' > \"$2\"; /bin/chmod +x \"$2\";; esac",
                ran.display()
            ),
        );
    }

    /// The build probe's leftovers under `tmp_root`: none, on every path.
    fn leftovers(tmp_root: &Path) -> Vec<String> {
        std::fs::read_dir(tmp_root)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with("atpkg-cc-"))
                    .collect()
            })
            .unwrap_or_default()
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
        building_cc(&good.join("cc"), &root.join("program-ran"));
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
        // A directory named `cc`, or a file nobody may run, is not a compiler.
        let odd = root.join("odd");
        std::fs::create_dir_all(odd.join("cc")).unwrap();
        assert_eq!(
            probe_with(Some(odd.as_os_str()), &host),
            CcVerdict::NoDriver
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE SHIM IS NEVER SPAWNED WITHOUT A COMPILER BEHIND IT. A `cc` in the shim
    /// directory with no developer directory holding a clang is NoDeveloperDir without
    /// running anything — the stub `cc` and `xcrun` here would leave a mark if they ran.
    /// With a developer directory that holds one, `xcrun --find cc` answers first, and
    /// only then is the shim run — and only ever to BUILD the probe (`-o <out> <src>`),
    /// the way rustc runs it; the verdict names what `xcrun` resolved.
    #[test]
    fn the_install_shim_runs_only_behind_a_resolved_compiler() {
        let root = scratch("shim");
        let shim = root.join("usr-bin");
        let ran = root.join("ran");
        let xcrun_ran = root.join("xcrun-ran");
        // The shim records its argv, then builds like a compiler.
        script(
            &shim.join("cc"),
            &format!(
                "echo \"$*\" > '{}'; [ \"$1\" = -o ] && printf x > \"$2\"",
                ran.display()
            ),
        );
        let clang_dir = root.join("CommandLineTools");
        script(
            &shim.join("xcrun"),
            &format!(
                "echo \"$*\" > '{}'; echo '{}/usr/bin/cc'",
                xcrun_ran.display(),
                clang_dir.display()
            ),
        );
        let tmp_root = root.join("tmp");
        std::fs::create_dir_all(&tmp_root).unwrap();
        let host = CcHost {
            shim_dir: Some(shim.clone()),
            xcode_select_link: root.join("xcode_select_link"),
            default_developer_dir: clang_dir.clone(),
            bound: ANSWER,
            tmp_root: tmp_root.clone(),
        };
        assert_eq!(
            probe_with(Some(shim.as_os_str()), &host),
            CcVerdict::NoDeveloperDir
        );
        assert!(!ran.exists(), "the shim was spawned");
        assert!(!xcrun_ran.exists(), "xcrun was spawned");

        // The Command Line Tools arrive: xcrun is asked which cc, then the shim builds.
        script(&clang_dir.join("usr/bin/clang"), "exit 0");
        assert_eq!(
            probe_with(Some(shim.as_os_str()), &host),
            CcVerdict::Ready {
                driver: clang_dir.join("usr/bin/cc")
            }
        );
        assert_eq!(
            std::fs::read_to_string(&xcrun_ran).unwrap().trim(),
            "--find cc"
        );
        let argv = std::fs::read_to_string(&ran).unwrap();
        let argv: Vec<&str> = argv.split_whitespace().collect();
        assert!(
            argv.len() == 3
                && argv[0] == "-o"
                && argv[1].ends_with("/probe.out")
                && argv[2].ends_with("/probe.c"),
            "the shim is only ever asked to build the probe: {argv:?}"
        );
        assert!(
            leftovers(&tmp_root).is_empty(),
            "{:?}",
            leftovers(&tmp_root)
        );
        std::fs::remove_file(&ran).unwrap();

        // `xcode-select -s` names another developer dir — an Xcode.app with no clang
        // in its toolchain: nothing to run again.
        let xcode = root.join("Xcode.app/Contents/Developer");
        std::fs::create_dir_all(&xcode).unwrap();
        std::os::unix::fs::symlink(&xcode, &host.xcode_select_link).unwrap();
        assert_eq!(
            probe_with(Some(shim.as_os_str()), &host),
            CcVerdict::NoDeveloperDir
        );
        assert!(!ran.exists(), "the shim was spawned");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A compiler that ANSWERS but cannot BUILD is broken, not ready — the gap
    /// `--version` alone left open (Linux gcc without libc headers; a developer directory
    /// whose SDK is gone), named by the compiler's own first stderr line. FAILED before
    /// 2026-09-27: `--version` answering was all `Ready` asked for.
    #[test]
    fn a_compiler_that_cannot_build_a_program_is_broken_not_ready() {
        let root = scratch("nolibc");
        let host = plain_host(&root);
        let dir = root.join("gcc");
        script(
            &dir.join("cc"),
            "case \"$1\" in --version) echo 'cc (GCC) 13';; *) echo \"probe.c:1:10: fatal error: stdio.h: No such file or directory\" >&2; exit 1;; esac",
        );
        let verdict = probe_with(Some(dir.as_os_str()), &host);
        assert_eq!(
            verdict,
            CcVerdict::Broken {
                driver: dir.join("cc"),
                why: "probe.c:1:10: fatal error: stdio.h: No such file or directory".into()
            }
        );
        assert!(verdict.blocks_builds());
        assert!(leftovers(&host.tmp_root).is_empty());

        // Success without an artifact is not proof.
        let liar = root.join("liar");
        script(
            &liar.join("cc"),
            "case \"$1\" in --version) echo 'cc 1';; *) exit 0;; esac",
        );
        assert_eq!(
            probe_with(Some(liar.as_os_str()), &host),
            CcVerdict::Broken {
                driver: liar.join("cc"),
                why: "it reported success but wrote no program".into()
            }
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A MISSING toolchain and a REFUSING one have different fixes. Installing one is
    /// the act for no compiler; a compiler that exists and cannot build names its own
    /// cause, and on macOS that cause is never "install the tools" — they answered (an
    /// unaccepted Xcode license, a PATH-first `cc` with no SDK), and `xcode-select
    /// --install` only replies that they are installed. Elsewhere a refusal is most
    /// often libc's missing headers, which the install act supplies. FAILED before
    /// 2026-09-28: every blocking verdict ended `fix: xcode-select --install` on macOS.
    #[test]
    fn a_refusing_compiler_is_not_told_to_install_what_it_is() {
        for missing in [CcVerdict::NoDriver, CcVerdict::NoDeveloperDir] {
            assert_eq!(missing.fix(), Some(act()), "{missing:?}");
            assert!(
                missing.line().ends_with(&format!("fix: {}", act())),
                "{}",
                missing.line()
            );
        }
        let broken = CcVerdict::Broken {
            driver: PathBuf::from("/Library/Developer/CommandLineTools/usr/bin/cc"),
            why: "Agreeing to the Xcode/iOS license requires admin privileges".into(),
        };
        let line = broken.line();
        assert!(line.contains("cannot build a program"), "{line}");
        assert!(line.contains("license requires admin privileges"), "{line}");
        if cfg!(target_os = "macos") {
            assert_eq!(broken.fix(), None);
            assert!(!line.contains(act()), "{line}");
            assert!(line.ends_with("fix what it reports"), "{line}");
        } else {
            assert_eq!(broken.fix(), Some(act()));
            assert!(line.ends_with(&format!("fix: {}", act())), "{line}");
        }
        for unknown in [
            CcVerdict::Unanswered {
                driver: PathBuf::from("/usr/bin/cc"),
            },
            CcVerdict::NotProbed,
        ] {
            assert_eq!(unknown.fix(), None, "{unknown:?}");
        }
    }

    /// A compiler that builds is ready; the program it built is never run, and the
    /// probe's private directory is gone afterwards.
    #[test]
    fn a_building_compiler_is_ready_and_its_program_never_runs() {
        let root = scratch("builds");
        let host = plain_host(&root);
        let dir = root.join("cc-dir");
        let program_ran = root.join("program-ran");
        building_cc(&dir.join("cc"), &program_ran);
        assert_eq!(
            probe_with(Some(dir.as_os_str()), &host),
            CcVerdict::Ready {
                driver: dir.join("cc")
            }
        );
        assert!(!program_ran.exists(), "the probe's program was executed");
        assert!(
            leftovers(&host.tmp_root).is_empty(),
            "{:?}",
            leftovers(&host.tmp_root)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A temp root the probe cannot make its directory in is UNPROVEN — unknown — never
    /// broken: a read-only temp says nothing about the toolchain, and it blocks nothing.
    #[test]
    fn an_unusable_temp_root_is_unproven_not_broken() {
        let root = scratch("notmp");
        let host = CcHost {
            tmp_root: root.join("no-such-dir"),
            ..plain_host(&root)
        };
        let dir = root.join("cc-dir");
        building_cc(&dir.join("cc"), &root.join("program-ran"));
        let verdict = probe_with(Some(dir.as_os_str()), &host);
        assert!(
            matches!(&verdict, CcVerdict::Unproven { driver, .. } if *driver == dir.join("cc")),
            "{verdict:?}"
        );
        assert!(!verdict.blocks_builds());
        assert!(verdict.line().contains("unknown"), "{}", verdict.line());
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
