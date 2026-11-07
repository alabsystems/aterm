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
        /// How long the probe waited for it ([`CcHost::bound`]).
        bound: Duration,
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
            Self::Unanswered { driver, bound } => format!(
                "the C compiler at {} did not answer within {} s — whether Rust builds can \
                 compile C here is unknown",
                driver.display(),
                bound.as_secs()
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

/// How long the probe of a run a person waits on once waits for the compiler (the elevation
/// System State's Mac Setup tab and `aterm init` run, [`CcHost::waited_on`]): a hang
/// detector, never a latency budget. A new Mac's first compile waits on macOS's first check of
/// the compiler, which a slow network stretches past [`PROBE_TIMEOUT`] (E3, 2026-11-05, a
/// fresh macOS 27 guest: 3.6 s idle, 8.0 s at 1 Mbit/s with 150 ms delay and 1% loss), and a
/// probe that gives up then reads tools that are there as unknown.
pub const FIRST_COMPILE_BOUND: Duration = Duration::from_secs(60);

/// Where the probe looks beyond `PATH`.
#[derive(Clone, Debug)]
pub struct CcHost {
    /// The directory whose `cc` is the libxcselect install shim.
    pub shim_dir: Option<PathBuf>,
    /// The symlink naming the active developer directory, read without spawning anything.
    pub xcode_select_link: PathBuf,
    /// The developer directory when no override, selection or Xcode compiler is present:
    /// where the Command Line Tools install.
    pub default_developer_dir: PathBuf,
    /// Xcode's standard developer directory, which Apple discovers before standalone tools
    /// when no override or selection is set. `None` where Xcode is not a candidate.
    pub xcode_developer_dir: Option<PathBuf>,
    /// The inherited `DEVELOPER_DIR`, which Apple's tools read before the system selection.
    pub developer_dir_override: Option<PathBuf>,
    /// The inherited SDK override. An explicit SDK is mutable outside Apple's install receipts,
    /// so a successful probe using it is not cached.
    pub sdkroot_override: Option<PathBuf>,
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
            xcode_developer_dir: cfg!(target_os = "macos")
                .then(|| PathBuf::from(aterm_types::clt_shim::XCODE_DEVELOPER)),
            developer_dir_override: std::env::var_os("DEVELOPER_DIR").map(PathBuf::from),
            sdkroot_override: std::env::var_os("SDKROOT").map(PathBuf::from),
            bound: PROBE_TIMEOUT,
            tmp_root: std::env::temp_dir(),
        }
    }

    /// [`Self::this_machine`] for a run a person waits on once, which gives up only on a
    /// compiler that hangs ([`FIRST_COMPILE_BOUND`]).
    #[must_use]
    pub fn waited_on() -> Self {
        Self {
            bound: FIRST_COMPILE_BOUND,
            ..Self::this_machine()
        }
    }

    /// A caller's explicit selection, excluding aterm's temporary setup directory.
    fn developer_override(&self) -> Option<&Path> {
        self.developer_dir_override.as_deref().filter(|dir| {
            !dir.as_os_str().is_empty() && !aterm_types::clt_shim::is_shim_developer_dir(dir)
        })
    }

    /// The developer directory Apple's tools use, looking through aterm's temporary shim.
    fn developer_dir(&self) -> PathBuf {
        self.developer_override()
            .map(Path::to_path_buf)
            .or_else(|| std::fs::read_link(&self.xcode_select_link).ok())
            .or_else(|| {
                self.xcode_developer_dir
                    .clone()
                    .filter(|dir| holds_clang(dir))
            })
            .unwrap_or_else(|| self.default_developer_dir.clone())
    }

    fn sdkroot(&self) -> Option<&Path> {
        self.sdkroot_override
            .as_deref()
            .filter(|sdk| !sdk.as_os_str().is_empty())
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
/// now ([`crate::probe_cache::identity`]), the active developer directory (`DEVELOPER_DIR`,
/// else `xcode-select`'s link, else Xcode or the standalone tools), the identity of its `clang`,
/// the Command Line Tools receipt's time, the SDK a build would fall back to and the `PATH`
/// the build sees. `None` when the compiler cannot be read or an explicit SDK is used:
/// changes within that SDK have no Apple install receipt to retire a cached success.
/// The executable-output policy domain retires verdicts made by the weaker output check.
fn cc_key(
    located: &Located,
    path_var: Option<&OsStr>,
    host: &CcHost,
    cache: &CcCache,
) -> Option<String> {
    if host.sdkroot().is_some() {
        return None;
    }
    let (kind, cc) = match located {
        Located::Plain(cc) => ("plain", cc),
        Located::Shim(cc) => ("shim", cc),
    };
    let developer_dir = host.developer_dir();
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
    let clang = clang_paths(&developer_dir)
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
        Some("executable-link-v1".to_owned()),
        Some(kind.to_owned()),
        crate::probe_cache::identity(cc),
        Some(developer_dir.display().to_string()),
        Some(clang),
        Some(receipt),
        Some(sdk),
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
            return match ask(&cc, &cc, &["--version"], host) {
                CcVerdict::Ready { driver } => build_probe(&cc, driver, path_var, host),
                other => other,
            };
        }
        Located::Shim(cc) => cc,
    };
    let shim_dir = cc.parent().unwrap_or(Path::new("/usr/bin"));
    match ask(&shim_dir.join("xcrun"), &cc, &["--find", "cc"], host) {
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
/// shim whether the active developer directory (the override, else the link, else Xcode or
/// the tools) holds a `clang`; [`CcVerdict::NoDriver`] or [`CcVerdict::NoDeveloperDir`] when it stops
/// there. Nothing is spawned.
fn locate(path_var: Option<&OsStr>, host: &CcHost) -> Result<Located, CcVerdict> {
    use aterm_types::clt_shim;
    let own = clt_shim::shim_dir();
    // A shell can change HOME or its state root while retaining the launch's PATH and
    // developer directory. Those inherited setup tools are not the new account's compiler.
    let inherited = std::env::var_os(clt_shim::SHIM_ENV).map(PathBuf::from);
    let developer = std::env::var_os(clt_shim::DEVELOPER_DIR_ENV).map(PathBuf::from);
    let developer_shim = developer
        .as_deref()
        .filter(|dir| clt_shim::is_shim_developer_dir(dir))
        .and_then(Path::parent);
    let shims = [own.as_deref(), inherited.as_deref(), developer_shim];
    locate_without_setup_shim(path_var, host, &shims)
}

/// Setup's temporary tools report that installation is needed; they are not compilers.
/// Look past them before deciding whether the actual compiler is missing or broken.
fn locate_without_setup_shim(
    path_var: Option<&OsStr>,
    host: &CcHost,
    setup_shims: &[Option<&Path>],
) -> Result<Located, CcVerdict> {
    let Some(cc) = path_var.and_then(|path| {
        std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .filter(|dir| !setup_shims.contains(&Some(dir.as_path())))
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
    let developer_dir = host.developer_dir();
    if holds_clang(&developer_dir) {
        Ok(Located::Shim(cc))
    } else {
        Err(CcVerdict::NoDeveloperDir)
    }
}

/// The two places Apple keeps the compiler under a developer directory.
fn clang_paths(developer: &Path) -> [PathBuf; 2] {
    [
        developer.join("usr/bin/clang"),
        developer
            .join(aterm_types::clt_shim::DEFAULT_TOOLCHAIN_BIN)
            .join("clang"),
    ]
}

fn holds_clang(developer: &Path) -> bool {
    clang_paths(developer)
        .iter()
        .any(|clang| is_executable(clang))
}

/// The build probe's program: the include proves the headers (the SDK on macOS, libc's on
/// Linux) resolve.
const PROBE_SOURCE: &str = "#include <stdio.h>\nint main(void) { return 0; }\n";

/// Make `cc` compile and link [`PROBE_SOURCE`] in a private directory under
/// `host.tmp_root`, within `host.bound`: `Ready { driver: resolved }` only when it exits 0
/// and wrote a non-empty executable program, which is never run. The directory is always removed.
fn build_probe(cc: &Path, resolved: PathBuf, path_var: Option<&OsStr>, host: &CcHost) -> CcVerdict {
    let scratch = match aterm_tempfile::Builder::new()
        .prefix("atpkg-cc-")
        .suffix(".noindex")
        .tempdir_in(&host.tmp_root)
    {
        Ok(scratch) => scratch,
        Err(error) => {
            return CcVerdict::Unproven {
                driver: resolved,
                why: format!(
                    "no private directory under {}: {error}",
                    host.tmp_root.display()
                ),
            };
        }
    };
    let dir = scratch.path();
    let source = dir.join("probe.c");
    let program = dir.join("probe.out");
    let write_source = || -> std::io::Result<()> {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut f = opts.open(&source)?;
        f.write_all(PROBE_SOURCE.as_bytes())
    };
    if let Err(error) = write_source() {
        return CcVerdict::Unproven {
            driver: resolved,
            why: format!("could not write {}: {error}", source.display()),
        };
    }
    let run_cc = |sdkroot: Option<&Path>| {
        let mut cmd = aterm_uds::spawnfd::command(cc);
        cmd.arg("-o").arg(&program).arg(&source).current_dir(dir);
        cmd.env_remove("DEVELOPER_DIR").env_remove("SDKROOT");
        if let Some(developer) = host.developer_override() {
            cmd.env("DEVELOPER_DIR", developer);
        }
        if let Some(path) = path_var {
            cmd.env("PATH", path);
        }
        if let Some(sdk) = sdkroot.or(host.sdkroot()) {
            cmd.env("SDKROOT", sdk);
        }
        crate::doctor::output_bounded_capturing(&mut cmd, host.bound)
    };
    let mut result = run_cc(None);
    if matches!(&result, Some(out) if !out.status.success())
        && host.sdkroot().is_none()
        && host
            .shim_dir
            .as_deref()
            .is_some_and(|shim| cc.parent() == Some(shim))
    {
        let developer_dir = host.developer_dir();
        let sdk = developer_dir.join("SDKs/MacOSX.sdk");
        if sdk.is_dir() {
            let _ = std::fs::remove_file(&program);
            result = run_cc(Some(&sdk));
        }
    }
    match result {
        None => CcVerdict::Unanswered {
            driver: cc.to_path_buf(),
            bound: host.bound,
        },
        Some(out) if out.status.success() => {
            let wrote = std::fs::symlink_metadata(&program)
                .is_ok_and(|metadata| is_executable_file(&metadata));
            if wrote {
                CcVerdict::Ready { driver: resolved }
            } else {
                CcVerdict::Broken {
                    driver: cc.to_path_buf(),
                    why: "it reported success without a nonempty executable output".to_string(),
                }
            }
        }
        Some(out) => CcVerdict::Broken {
            driver: cc.to_path_buf(),
            why: first_stderr_line(&out),
        },
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
fn ask(bin: &Path, driver: &Path, args: &[&str], host: &CcHost) -> CcVerdict {
    let bound = host.bound;
    let mut cmd = aterm_uds::spawnfd::command(bin);
    cmd.args(args);
    cmd.env_remove("DEVELOPER_DIR").env_remove("SDKROOT");
    if let Some(developer) = host.developer_override() {
        cmd.env("DEVELOPER_DIR", developer);
    }
    if let Some(sdk) = host.sdkroot() {
        cmd.env("SDKROOT", sdk);
    }
    match crate::doctor::output_bounded_capturing(&mut cmd, bound) {
        None => CcVerdict::Unanswered {
            driver: driver.to_path_buf(),
            bound,
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

/// A nonempty regular file this user may execute (never a directory named `cc`).
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| is_executable_file(&metadata))
}

fn is_executable_file(meta: &std::fs::Metadata) -> bool {
    if !meta.is_file() || meta.len() == 0 {
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
            xcode_developer_dir: None,
            developer_dir_override: None,
            sdkroot_override: None,
            bound: ANSWER,
            tmp_root,
        }
    }

    #[test]
    fn empty_executable_files_are_missing_compilers_in_path_and_developer_discovery() {
        let root = scratch("empty-compiler");
        let empty = root.join("empty");
        let apple = root.join("usr-bin");
        let developer = root.join("developer");
        for file in [empty.join("cc"), developer.join("usr/bin/clang")] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, []).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let host = plain_host(&root);
        assert!(matches!(
            locate_without_setup_shim(Some(empty.as_os_str()), &host, &[]),
            Err(CcVerdict::NoDriver)
        ));
        script(&apple.join("cc"), "exit 1");
        let path = std::env::join_paths([&empty, &apple]).unwrap();
        assert!(matches!(
            locate_without_setup_shim(Some(&path), &host, &[]),
            Ok(Located::Plain(cc)) if cc == apple.join("cc")
        ));
        let host = CcHost {
            shim_dir: Some(apple.clone()),
            default_developer_dir: developer.clone(),
            ..host
        };
        assert!(matches!(
            locate_without_setup_shim(Some(&path), &host, &[]),
            Err(CcVerdict::NoDeveloperDir)
        ));
        script(&developer.join("usr/bin/clang"), "exit 0");
        assert!(matches!(
            locate_without_setup_shim(Some(&path), &host, &[]),
            Ok(Located::Shim(cc)) if cc == apple.join("cc")
        ));
    }

    #[test]
    fn setup_shim_does_not_turn_missing_tools_into_a_broken_compiler() {
        let root = scratch("setup-shim");
        let setup = root.join("clt-shim");
        let inherited = root.join("old-home/clt-shim");
        let apple = root.join("usr-bin");
        // These must never run: discovery alone can tell the tools are missing.
        script(&setup.join("cc"), "exit 69");
        script(&inherited.join("cc"), "exit 69");
        script(&apple.join("cc"), "exit 1");
        let host = CcHost {
            shim_dir: Some(apple.clone()),
            ..plain_host(&root)
        };
        let path = std::env::join_paths([&inherited, &setup, &apple]).unwrap();
        assert!(matches!(
            locate_without_setup_shim(Some(&path), &host, &[Some(&setup), Some(&inherited)]),
            Err(CcVerdict::NoDeveloperDir)
        ));
        // An unmarked PATH compiler retains priority, including its failures.
        assert!(matches!(
            locate_without_setup_shim(Some(&path), &host, &[Some(&setup)]),
            Ok(Located::Plain(cc)) if cc == inherited.join("cc")
        ));
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
                "echo \"$*\" > '{}'; [ \"$1\" = -o ] && {{ printf x > \"$2\"; /bin/chmod +x \"$2\"; }}",
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
            xcode_developer_dir: None,
            developer_dir_override: None,
            sdkroot_override: None,
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
        script(
            &liar.join("cc"),
            "case \"$1\" in --version) echo cc;; -o) printf x > \"$2\"; /bin/chmod 600 \"$2\";; esac",
        );
        assert!(matches!(
            probe_with(Some(liar.as_os_str()), &host),
            CcVerdict::Broken { driver, .. } if driver == liar.join("cc")
        ));
        building_cc(&liar.join("cc"), &root.join("program-ran"));
        assert!(matches!(
            probe_with(Some(liar.as_os_str()), &host),
            CcVerdict::Ready { .. }
        ));
        assert!(!root.join("program-ran").exists());
        assert!(leftovers(&host.tmp_root).is_empty());
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
                bound: PROBE_TIMEOUT,
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
                driver: dir.join("cc"),
                bound,
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
                "echo \"$1\" >> '{}'; case \"$1\" in --version) echo 'cc version 1';; -o) printf x > \"$2\"; /bin/chmod +x \"$2\";; esac",
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

    #[test]
    fn a_changed_explicit_sdk_cannot_reuse_a_cached_success() {
        let root = scratch("mutable-sdk");
        let bin = root.join("bin");
        let sdk = root.join("external SDK");
        std::fs::create_dir_all(&sdk).unwrap();
        let usable = sdk.join("usable");
        std::fs::write(&usable, "headers and libraries").unwrap();
        script(
            &bin.join("cc"),
            "case \"$1\" in --version) echo cc;; -o) if [ -f \"$SDKROOT/usable\" ]; then printf x > \"$2\"; /bin/chmod +x \"$2\"; else echo 'SDK no longer usable' >&2; exit 1; fi;; esac",
        );
        let host = CcHost {
            sdkroot_override: Some(sdk),
            ..plain_host(&root)
        };
        let cache = CcCache {
            file: root.join("state/cc"),
            receipt: root.join("no-receipt"),
        };
        let path = Some(bin.as_os_str());
        assert_eq!(
            probe_cached(path, &host, Some(&cache)),
            CcVerdict::Ready {
                driver: bin.join("cc")
            }
        );
        std::fs::remove_file(&usable).unwrap();
        assert!(matches!(
            probe_cached(path, &host, Some(&cache)),
            CcVerdict::Broken { why, .. } if why == "SDK no longer usable"
        ));
    }

    #[test]
    fn a_verdict_kept_before_executable_output_was_required_is_not_reused() {
        let root = scratch("old-output-policy");
        let bin = root.join("bin");
        let cc = bin.join("cc");
        let asked = root.join("asked");
        script(
            &cc,
            &format!(
                "echo \"$1\" >> '{}'; case \"$1\" in --version) echo cc;; -o) printf x > \"$2\"; /bin/chmod 600 \"$2\";; esac",
                asked.display()
            ),
        );
        let host = plain_host(&root);
        let cache = CcCache {
            file: root.join("state").join("cc"),
            receipt: root.join("absent-receipt"),
        };
        let developer = host.developer_dir();
        let clang = clang_paths(&developer)
            .iter()
            .map(|path| crate::probe_cache::identity(path).unwrap_or_else(|| "absent".to_owned()))
            .collect::<Vec<_>>()
            .join(" ");
        // The old cache format had these seven parts and no acceptance-policy domain.
        let old_key = crate::probe_cache::key([
            Some("plain".to_owned()),
            crate::probe_cache::identity(&cc),
            Some(developer.display().to_string()),
            Some(clang),
            Some("no receipt".to_owned()),
            Some("no sdk".to_owned()),
            Some(bin.display().to_string()),
        ])
        .unwrap();
        crate::probe_cache::put(&cache.file, &old_key, &cc.to_string_lossy());
        for calls in [2, 4] {
            assert!(matches!(
                probe_cached(Some(bin.as_os_str()), &host, Some(&cache)),
                CcVerdict::Broken { .. }
            ));
            assert_eq!(
                std::fs::read_to_string(&asked).unwrap().lines().count(),
                calls
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_developer_override_controls_discovery_and_retires_the_cached_compiler() {
        let root = scratch("developer-override");
        let shim = root.join("usr-bin");
        let first = root.join("first");
        let second = root.join("second");
        let asked = root.join("asked");
        for developer in [&first, &second] {
            script(&developer.join("usr/bin/clang"), "exit 0");
        }
        script(&shim.join("xcrun"), "echo \"$DEVELOPER_DIR/usr/bin/cc\"");
        script(
            &shim.join("cc"),
            &format!(
                "echo \"$DEVELOPER_DIR\" >> '{}'; [ \"$1\" = -o ] && {{ printf x > \"$2\"; /bin/chmod +x \"$2\"; }}",
                asked.display()
            ),
        );
        let mut host = CcHost {
            shim_dir: Some(shim.clone()),
            ..plain_host(&root)
        };
        let cache = CcCache {
            file: root.join("state/cc"),
            receipt: root.join("no-receipt"),
        };
        let path = Some(shim.as_os_str());
        assert_eq!(
            probe_cached(path, &host, Some(&cache)),
            CcVerdict::NoDeveloperDir
        );
        for (index, developer) in [&first, &second].into_iter().enumerate() {
            host.developer_dir_override = Some(developer.clone());
            let ready = CcVerdict::Ready {
                driver: developer.join("usr/bin/cc"),
            };
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            let calls = std::fs::read_to_string(&asked).unwrap();
            assert_eq!(calls.lines().count(), index + 1);
            assert_eq!(calls.lines().last(), developer.to_str());
        }
        host.developer_dir_override = Some(root.join("clt-shim/developer"));
        assert_eq!(
            probe_cached(path, &host, Some(&cache)),
            CcVerdict::NoDeveloperDir
        );
    }

    #[test]
    fn a_setup_developer_override_is_not_reintroduced_after_discovering_real_tools() {
        let root = scratch("installed-after-start");
        let shim = root.join("usr-bin");
        let developer = root.join("real-developer");
        script(&developer.join("usr/bin/clang"), "exit 0");
        script(
            &shim.join("xcrun"),
            &format!(
                "[ -n \"$DEVELOPER_DIR\" ] && {{ echo 'setup override reached xcrun' >&2; exit 1; }}\necho '{}/usr/bin/cc'",
                developer.display()
            ),
        );
        script(
            &shim.join("cc"),
            "[ -n \"$DEVELOPER_DIR\" ] && { echo 'setup override reached compiler' >&2; exit 1; }\nprintf x > \"$2\"\n/bin/chmod +x \"$2\"",
        );
        let host = CcHost {
            shim_dir: Some(shim.clone()),
            default_developer_dir: developer.clone(),
            developer_dir_override: Some(root.join("clt-shim/developer")),
            ..plain_host(&root)
        };
        let path = Some(shim.as_os_str());
        assert_eq!(installed_with(path, &host), Some(true));
        assert_eq!(
            probe_with(path, &host),
            CcVerdict::Ready {
                driver: developer.join("usr/bin/cc")
            }
        );
    }

    /// An Xcode update replaces the automatically discovered compiler while the
    /// Command Line Tools receipt stays: the kept verdict retires and the shim is asked again,
    /// so a `cc` the update left refusing (a license not yet agreed to) is seen. NEGATIVE
    /// CONTROL: with the same compiler in place the kept verdict answers and nothing spawns.
    #[test]
    fn an_automatic_xcode_is_discovered_and_its_update_retires_the_cache() {
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
                "echo \"$1\" >> '{}'; [ \"$1\" = -o ] && {{ printf x > \"$2\"; /bin/chmod +x \"$2\"; }}",
                asked.display()
            ),
        );
        let host = CcHost {
            shim_dir: Some(shim.clone()),
            xcode_select_link: root.join("xcode_select_link"),
            xcode_developer_dir: Some(xcode.clone()),
            ..plain_host(&root)
        };
        let receipt = root.join("receipt");
        std::fs::write(&receipt, "1").unwrap();
        let cache = CcCache {
            file: root.join("state").join("cc"),
            receipt,
        };
        let path = Some(shim.as_os_str());
        let without_xcode = CcHost {
            xcode_developer_dir: None,
            ..host.clone()
        };
        assert_eq!(probe_with(path, &without_xcode), CcVerdict::NoDeveloperDir);
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
