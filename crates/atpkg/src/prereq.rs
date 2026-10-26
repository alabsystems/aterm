// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The C toolchain: a prerequisite atpkg detects and never ships.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What the probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CcVerdict {
    /// The `cc` a build would run compiled and linked the probe program.
    Ready {
        /// The compiler that built the probe program.
        driver: PathBuf,
    },
    /// macOS: `cc` is the libxcselect shim and no developer directory holds a `clang`.
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
    /// The compiler answered, but the build probe could not run — unknown, not broken.
    Unproven {
        /// The compiler that answered.
        driver: PathBuf,
        /// Why the build probe could not run.
        why: String,
    },
    /// A target where a `cc` test is the wrong question (Windows: rustc owns MSVC discovery).
    NotProbed,
}

impl CcVerdict {
    /// Whether this verdict means a Trust build cannot compile here (an answer, not an unknown).
    #[must_use]
    pub const fn blocks_builds(&self) -> bool {
        matches!(
            self,
            Self::NoDeveloperDir | Self::NoDriver | Self::Broken { .. }
        )
    }

    /// The one command that fixes this verdict, when there is one.
    #[must_use]
    pub const fn fix(&self) -> Option<&'static str> {
        match self {
            Self::NoDeveloperDir | Self::NoDriver => Some(act()),
            Self::Broken { .. } if !cfg!(target_os = "macos") => Some(act()),
            _ => None,
        }
    }

    /// The sentence `doctor` and the default-set tail print for this verdict.
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

/// The act that installs a C toolchain on this platform.
#[must_use]
pub const fn act() -> &'static str {
    if cfg!(target_os = "macos") {
        "aterm init --only base.clt"
    } else if cfg!(windows) {
        "install the Visual Studio Build Tools (C++ workload)"
    } else {
        "install your distribution's C compiler (e.g. `sudo apt install build-essential` or \
         `sudo dnf install gcc`)"
    }
}

/// How long one compiler probe may take.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the probe looks beyond `PATH`.
#[derive(Clone, Debug)]
pub struct CcHost {
    /// The directory whose `cc` is the libxcselect install shim.
    pub shim_dir: Option<PathBuf>,
    /// The symlink naming the active developer directory, read without spawning anything.
    pub xcode_select_link: PathBuf,
    /// The developer directory when the link is absent: where the Command Line Tools install.
    pub default_developer_dir: PathBuf,
    /// How long each compiler probe may take: [`PROBE_TIMEOUT`] on this machine.
    pub bound: Duration,
    /// Where the build probe makes its private directory.
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

/// The receipt the Command Line Tools install writes (macOS).
const CLT_RECEIPT: &str =
    "/Library/Apple/System/Library/Receipts/com.apple.pkg.CLTools_Executables.plist";

/// Where a `Ready` verdict is kept between runs ([`probe_cached`]).
#[derive(Clone, Debug)]
pub struct CcCache {
    /// The file that keeps it ([`crate::probe_cache`]).
    pub file: PathBuf,
    /// The receipt the Command Line Tools install writes: an install, an update or a removal
    /// changes its time and retires the verdict.
    pub receipt: PathBuf,
}

impl CcCache {
    /// This machine's, under aterm's state root; `None` without one.
    #[must_use]
    pub fn this_machine() -> Option<Self> {
        Some(Self {
            file: crate::probe_cache::file("cc")?,
            receipt: PathBuf::from(CLT_RECEIPT),
        })
    }
}

/// Probe this machine's C toolchain against `path_var` (the `PATH` a build would see), reading
/// a verdict kept from an earlier run when nothing it depends on changed ([`probe_cached`]).
#[must_use]
pub fn probe(path_var: Option<&OsStr>) -> CcVerdict {
    probe_cached(
        path_var,
        &CcHost::this_machine(),
        CcCache::this_machine().as_ref(),
    )
}

/// [`probe_with`], keeping a `Ready` verdict in `cache` under [`cc_key`]: a later probe whose
/// key matches answers `Ready` with nothing spawned (no `xcrun`, no compile and link); any other
/// verdict is probed every time.
#[must_use]
pub fn probe_cached(path_var: Option<&OsStr>, host: &CcHost, cache: Option<&CcCache>) -> CcVerdict {
    if cfg!(windows) {
        return CcVerdict::NotProbed;
    }
    let located = match locate(path_var, host) {
        Ok(located) => located,
        Err(verdict) => return verdict,
    };
    let kept = cache.and_then(|cache| Some((cache, cc_key(&located, path_var, host, cache)?)));
    if let Some((cache, key)) = &kept
        && let Some(driver) = crate::probe_cache::get(&cache.file, key)
    {
        return CcVerdict::Ready {
            driver: PathBuf::from(driver),
        };
    }
    let verdict = probe_located(located, path_var, host);
    if let (CcVerdict::Ready { driver }, Some((cache, key))) = (&verdict, &kept) {
        crate::probe_cache::put(&cache.file, key, &driver.to_string_lossy());
    }
    verdict
}

/// What a `Ready` verdict for `located` depends on, read without spawning: the compiler as it is
/// now ([`crate::probe_cache::identity`]), the active developer directory (`xcode-select`'s
/// link, else the default) and the identity of the `clang` it holds, the Command Line Tools
/// receipt's time, the SDK a build would fall back to, `SDKROOT` and the `PATH` the build sees.
/// `None` when the compiler cannot be read.
fn cc_key(
    located: &Located,
    path_var: Option<&OsStr>,
    host: &CcHost,
    cache: &CcCache,
) -> Option<String> {
    let (kind, cc) = match located {
        Located::Plain(cc) => ("plain", cc),
        Located::Shim(cc) => ("shim", cc),
    };
    let developer_dir = std::fs::read_link(&host.xcode_select_link)
        .unwrap_or_else(|_| host.default_developer_dir.clone());
    let receipt = std::fs::metadata(&cache.receipt)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(|| "no receipt".to_owned(), |at| at.as_nanos().to_string());
    let sdk = std::fs::canonicalize(developer_dir.join("SDKs/MacOSX.sdk"))
        .map_or_else(|_| "no sdk".to_owned(), |sdk| sdk.display().to_string());
    // The compiler the developer directory holds, where `locate` looks for it: an Xcode
    // install, update or replacement rewrites it while the Command Line Tools receipt stays,
    // and an update that leaves `cc` refusing (a license not yet agreed to) must probe again.
    let clang = [
        developer_dir.join("usr/bin/clang"),
        developer_dir.join("Toolchains/XcodeDefault.xctoolchain/usr/bin/clang"),
    ]
    .iter()
    .map(|clang| crate::probe_cache::identity(clang).unwrap_or_else(|| "absent".to_owned()))
    .collect::<Vec<_>>()
    .join(" ");
    fn lossy(value: Option<&OsStr>) -> String {
        value
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    crate::probe_cache::key([
        Some(kind.to_owned()),
        crate::probe_cache::identity(cc),
        Some(developer_dir.display().to_string()),
        Some(clang),
        Some(receipt),
        Some(sdk),
        Some(lossy(std::env::var_os("SDKROOT").as_deref())),
        Some(lossy(path_var)),
    ])
}

/// [`probe`] over an injected host.
#[must_use]
pub fn probe_with(path_var: Option<&OsStr>, host: &CcHost) -> CcVerdict {
    if cfg!(windows) {
        return CcVerdict::NotProbed;
    }
    match locate(path_var, host) {
        Ok(located) => probe_located(located, path_var, host),
        Err(verdict) => verdict,
    }
}

/// [`probe_with`] past [`locate`]: the compiler asked, then the build probe.
fn probe_located(located: Located, path_var: Option<&OsStr>, host: &CcHost) -> CcVerdict {
    let cc = match located {
        Located::Plain(cc) => {
            return match ask(&cc, &cc, &["--version"], host.bound) {
                CcVerdict::Ready { driver } => build_probe(&cc, driver, path_var, host),
                other => other,
            };
        }
        Located::Shim(cc) => cc,
    };
    let shim_dir = cc.parent().unwrap_or(Path::new("/usr/bin"));
    match ask(&shim_dir.join("xcrun"), &cc, &["--find", "cc"], host.bound) {
        CcVerdict::Ready { driver } => build_probe(&cc, driver, path_var, host),
        other => other,
    }
}

/// Whether a C toolchain is installed here, read without running anything.
#[must_use]
pub fn installed_with(path_var: Option<&OsStr>, host: &CcHost) -> Option<bool> {
    (!cfg!(windows)).then(|| locate(path_var, host).is_ok())
}

/// The `cc` a build would run, as [`locate`] finds it.
enum Located {
    /// A compiler that is not the install shim: asked its version first.
    Plain(PathBuf),
    /// The libxcselect shim, with a developer directory holding a `clang` behind it.
    Shim(PathBuf),
}

/// The read-only half of [`probe_with`]: the first `cc` on `path_var`, and for the install
/// shim whether the active developer directory (the link, else the default) holds a
/// `clang`; [`CcVerdict::NoDriver`] or [`CcVerdict::NoDeveloperDir`] when the probe stops
/// there. Nothing is spawned.
fn locate(path_var: Option<&OsStr>, host: &CcHost) -> Result<Located, CcVerdict> {
    let Some(cc) = path_var.and_then(|path| {
        std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("cc"))
            .find(|candidate| is_executable(candidate))
    }) else {
        return Err(CcVerdict::NoDriver);
    };
    let is_shim = host
        .shim_dir
        .as_deref()
        .is_some_and(|shim| cc.parent() == Some(shim));
    if !is_shim {
        return Ok(Located::Plain(cc));
    }
    let developer_dir = std::fs::read_link(&host.xcode_select_link)
        .unwrap_or_else(|_| host.default_developer_dir.clone());
    let holds_clang = [
        developer_dir.join("usr/bin/clang"),
        developer_dir.join("Toolchains/XcodeDefault.xctoolchain/usr/bin/clang"),
    ]
    .iter()
    .any(|clang| is_executable(clang));
    if holds_clang {
        Ok(Located::Shim(cc))
    } else {
        Err(CcVerdict::NoDeveloperDir)
    }
}

/// The build probe's program: the include proves the headers (the SDK on macOS, libc's on
/// Linux) resolve.
const PROBE_SOURCE: &str = "#include <stdio.h>\nint main(void) { return 0; }\n";

/// Make `cc` compile and link [`PROBE_SOURCE`] in a private directory under
/// `host.tmp_root`, within `host.bound`: `Ready { driver: resolved }` only when it exits 0
/// and wrote a non-empty program, which is never run. The directory is always removed.
fn build_probe(cc: &Path, resolved: PathBuf, path_var: Option<&OsStr>, host: &CcHost) -> CcVerdict {
    let nanos = aterm_time::since_epoch().map_or(0, |d| d.as_nanos());
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
    let run_cc = |sdkroot: Option<&Path>| {
        let mut cmd = aterm_uds::spawnfd::command(cc);
        cmd.arg("-o").arg(&program).arg(&source).current_dir(&dir);
        if let Some(path) = path_var {
            cmd.env("PATH", path);
        }
        if let Some(sdk) = sdkroot {
            cmd.env("SDKROOT", sdk);
        }
        crate::doctor::output_bounded_capturing(&mut cmd, host.bound)
    };
    let mut result = run_cc(None);
    if matches!(&result, Some(out) if !out.status.success())
        && std::env::var_os("SDKROOT").is_none()
        && host
            .shim_dir
            .as_deref()
            .is_some_and(|shim| cc.parent() == Some(shim))
    {
        let developer_dir = std::fs::read_link(&host.xcode_select_link)
            .unwrap_or_else(|_| host.default_developer_dir.clone());
        let sdk = developer_dir.join("SDKs/MacOSX.sdk");
        if sdk.is_dir() {
            let _ = std::fs::remove_file(&program);
            result = run_cc(Some(&sdk));
        }
    }
    match result {
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

/// Create `dir`, readable by this user only; an existing path is an error, never reused.
pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
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
pub(crate) struct RemoveOnDrop<'a>(pub(crate) &'a Path);

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

/// Run `bin args…` within `bound` as a verdict about `driver`: exit 0 is Ready (naming the
/// path `xcrun` printed, if any), any other exit is Broken with the first stderr line.
fn ask(bin: &Path, driver: &Path, args: &[&str], bound: Duration) -> CcVerdict {
    let mut cmd = aterm_uds::spawnfd::command(bin);
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

    fn scratch(name: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-prereq-{name}-"))
    }

    fn script(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The bound for a probe whose script must answer: a just-written file's first exec
    /// waits on the system's code assessment, which a loaded machine stretches past
    /// [`PROBE_TIMEOUT`]. These cases test reading the answer, not the bound.
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

    /// A fake `cc` that answers `--version` and, asked `-o <out> <src>`, builds: it writes
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

    /// No `cc` is NoDriver, one that answers is Ready, one that refuses is Broken with its
    /// first stderr line; NoDriver and Broken block builds.
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
        let odd = root.join("odd");
        std::fs::create_dir_all(odd.join("cc")).unwrap();
        assert_eq!(
            probe_with(Some(odd.as_os_str()), &host),
            CcVerdict::NoDriver
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The shim never runs without a compiler behind it: with no clang in the developer
    /// directory nothing is spawned; with one, `xcrun --find cc` answers first and the shim
    /// is only asked to build the probe. The verdict names what `xcrun` resolved.
    #[test]
    fn the_install_shim_runs_only_behind_a_resolved_compiler() {
        let root = scratch("shim");
        let shim = root.join("usr-bin");
        let ran = root.join("ran");
        let xcrun_ran = root.join("xcrun-ran");
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

    /// [`installed_with`] is the probe's read-only half: `Some(false)` exactly where the probe
    /// stops at NoDriver or NoDeveloperDir, `Some(true)` where it would ask a compiler, and
    /// it spawns nothing on either path.
    #[test]
    fn installed_is_false_exactly_where_the_probe_finds_nothing_to_ask() {
        let root = scratch("installed");
        let ran = root.join("ran");
        let plain = plain_host(&root);
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(installed_with(Some(empty.as_os_str()), &plain), Some(false));
        assert_eq!(installed_with(None, &plain), Some(false));
        let good = root.join("good");
        script(&good.join("cc"), &format!("touch '{}'", ran.display()));
        assert_eq!(installed_with(Some(good.as_os_str()), &plain), Some(true));

        let shim = root.join("usr-bin");
        script(&shim.join("cc"), &format!("touch '{}'", ran.display()));
        let developer = root.join("CommandLineTools");
        let host = CcHost {
            shim_dir: Some(shim.clone()),
            default_developer_dir: developer.clone(),
            ..plain_host(&root)
        };
        assert_eq!(installed_with(Some(shim.as_os_str()), &host), Some(false));
        assert_eq!(
            probe_with(Some(shim.as_os_str()), &host),
            CcVerdict::NoDeveloperDir
        );
        script(&developer.join("usr/bin/clang"), "exit 0");
        assert_eq!(installed_with(Some(shim.as_os_str()), &host), Some(true));
        assert!(!ran.exists(), "nothing was spawned");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A compiler that answers `--version` but cannot build is Broken, with its first stderr
    /// line.
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

        let liar = root.join("liar");
        script(
            &liar.join("cc"),
            "case \"$1\" in --version) echo 'cc 1';; *) exit 0;; esac",
        );
        assert!(matches!(
            probe_with(Some(liar.as_os_str()), &host),
            CcVerdict::Broken { driver, .. } if driver == liar.join("cc")
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// On macOS the install is `base.clt`, one password prompt in a terminal, never Apple's
    /// desktop dialog (design init-system A15).
    #[test]
    fn the_macos_install_is_base_clt() {
        if cfg!(target_os = "macos") {
            assert_eq!(act(), "aterm init --only base.clt");
        }
    }

    /// A missing toolchain is told to install one; a refusing compiler on macOS is told to fix
    /// what it reports, never the install.
    #[test]
    fn a_refusing_compiler_is_not_told_to_install_what_it_is() {
        for missing in [CcVerdict::NoDriver, CcVerdict::NoDeveloperDir] {
            assert_eq!(missing.fix(), Some(act()), "{missing:?}");
        }
        let broken = CcVerdict::Broken {
            driver: PathBuf::from("/Library/Developer/CommandLineTools/usr/bin/cc"),
            why: "Agreeing to the Xcode/iOS license requires admin privileges".into(),
        };
        let line = broken.line();
        assert!(line.contains("license requires admin privileges"), "{line}");
        if cfg!(target_os = "macos") {
            assert_eq!(broken.fix(), None);
            assert!(!line.contains(act()), "{line}");
        } else {
            assert_eq!(broken.fix(), Some(act()));
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

    /// A building compiler is Ready; its program never runs and the probe directory is gone.
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

    /// A temp root the probe cannot use is Unproven and blocks nothing.
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
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A compiler that hangs is unknown, not broken, and the probe returns inside its
    /// bound. The compiler is a
    /// driver that forks the real work, as `cc` forks `cc1`: the bound's kill ends the
    /// driver and leaves its ten-minute `sleep` holding the probe's pipes. The probe's
    /// return is held to a minute's hang detector, never a latency budget: a probe that
    /// ignores its bound, or drains those pipes after the kill, waits out all ten.
    #[test]
    fn a_wedged_compiler_is_unanswered_not_broken() {
        let root = scratch("wedged");
        let dir = root.join("slow");
        let pid_file = root.join("worker.pid");
        script(
            &dir.join("cc"),
            &format!("sleep 600 & echo $! > '{}'; wait", pid_file.display()),
        );
        let bound = Duration::from_millis(500);
        let host = CcHost {
            bound,
            ..plain_host(&root)
        };
        let started = std::time::Instant::now();
        let verdict = probe_with(Some(dir.as_os_str()), &host);
        let took = started.elapsed();
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|pid| pid.trim().parse::<libc::pid_t>().ok())
        {
            // SAFETY: kill(2) of the background `sleep` this test's driver started.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        assert!(
            took < aterm_spec::wait::MUST_HAPPEN,
            "not the ten-minute hang: {took:?}"
        );
        assert_eq!(
            verdict,
            CcVerdict::Unanswered {
                driver: dir.join("cc")
            }
        );
        assert!(!verdict.blocks_builds());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `Ready` verdict is kept under what it depends on: a probe with the same key spawns
    /// nothing (the compiler is never asked again), and a changed Command Line Tools receipt
    /// probes again. NEGATIVE CONTROL: the receipt's new time re-runs the compiler; with no
    /// cache every probe runs it.
    #[test]
    fn a_kept_cc_verdict_spawns_nothing_until_the_receipt_changes() {
        let root = scratch("kept");
        let bin = root.join("bin");
        let asked = root.join("asked");
        script(
            &bin.join("cc"),
            &format!(
                "echo \"$1\" >> '{}'; case \"$1\" in --version) echo 'cc version 1';; -o) printf x > \"$2\";; esac",
                asked.display()
            ),
        );
        let host = plain_host(&root);
        let receipt = root.join("receipt");
        std::fs::write(&receipt, "1").unwrap();
        let cache = CcCache {
            file: root.join("state").join("cc"),
            receipt: receipt.clone(),
        };
        let path = Some(bin.as_os_str());
        let runs = || std::fs::read_to_string(&asked).map_or(0, |text| text.lines().count());
        let ready = CcVerdict::Ready {
            driver: bin.join("cc"),
        };
        assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
        assert_eq!(runs(), 2, "asked its version, then built the probe");
        assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
        assert_eq!(runs(), 2, "the same key ran the compiler again");
        std::fs::File::options()
            .write(true)
            .open(&receipt)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(86_400))
            .unwrap();
        assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
        assert_eq!(runs(), 4, "a changed receipt left the old verdict standing");
        assert_eq!(probe_cached(path, &host, None), ready);
        assert_eq!(runs(), 6, "no cache: the compiler is asked");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An Xcode update replaces the compiler the selected developer directory holds while the
    /// Command Line Tools receipt stays: the kept verdict retires and the shim is asked again,
    /// so a `cc` the update left refusing (a license not yet agreed to) is seen. NEGATIVE
    /// CONTROL: with the same compiler in place the kept verdict answers and nothing spawns.
    #[test]
    fn an_xcode_update_retires_the_kept_cc_verdict() {
        let root = scratch("kept-xcode");
        let shim = root.join("usr-bin");
        let asked = root.join("asked");
        let xcode = root.join("Xcode.app/Contents/Developer");
        let clang = xcode.join("Toolchains/XcodeDefault.xctoolchain/usr/bin/clang");
        script(&clang, "exit 0");
        script(
            &shim.join("xcrun"),
            &format!("echo '{}/usr/bin/cc'", xcode.display()),
        );
        script(
            &shim.join("cc"),
            &format!(
                "echo \"$1\" >> '{}'; [ \"$1\" = -o ] && printf x > \"$2\"",
                asked.display()
            ),
        );
        let host = CcHost {
            shim_dir: Some(shim.clone()),
            xcode_select_link: root.join("xcode_select_link"),
            ..plain_host(&root)
        };
        std::os::unix::fs::symlink(&xcode, &host.xcode_select_link).unwrap();
        let receipt = root.join("receipt");
        std::fs::write(&receipt, "1").unwrap();
        let cache = CcCache {
            file: root.join("state").join("cc"),
            receipt,
        };
        let path = Some(shim.as_os_str());
        let runs = || std::fs::read_to_string(&asked).map_or(0, |text| text.lines().count());
        let ready = CcVerdict::Ready {
            driver: xcode.join("usr/bin/cc"),
        };
        assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
        assert_eq!(runs(), 1, "the shim built the probe");
        assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
        assert_eq!(runs(), 1, "the same Xcode asked the shim again");
        std::fs::remove_file(&clang).unwrap();
        script(&clang, "echo updated; exit 0");
        assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
        assert_eq!(runs(), 2, "an updated Xcode kept the old verdict");
        let _ = std::fs::remove_dir_all(&root);
    }
}
