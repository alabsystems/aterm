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
    /// Discovery or the build probe could not establish compiler state — unknown, not broken.
    Unproven {
        /// The compiler or candidate whose state could not be established.
        driver: PathBuf,
        /// Why discovery or the build probe could not finish.
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
                "the C toolchain at {} could not be verified ({why}) — \
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
    /// Its SDK may change without an Apple install receipt, so successes are not cached.
    pub developer_dir_override: Option<PathBuf>,
    /// The inherited SDK override. An explicit SDK is mutable outside Apple's install receipts,
    /// so a successful probe using it is not cached.
    pub sdkroot_override: Option<PathBuf>,
    /// The inherited minimum macOS version for a build, independent of installed tool health.
    pub deployment_target_override: Option<std::ffi::OsString>,
    /// Inherit project compiler configuration, which cannot retain a cached success because
    /// it can name mutable inputs. Machine setup clears the environment instead, then applies
    /// the explicit probe inputs and temporary directory.
    pub inherit_environment: bool,
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
            deployment_target_override: std::env::var_os("MACOSX_DEPLOYMENT_TARGET"),
            inherit_environment: true,
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
    /// Unread selection or compiler state is not permission to choose a fallback. Relative
    /// link targets resolve beside the selection link, as the filesystem resolves them.
    fn developer_dir(&self) -> Result<PathBuf, String> {
        if let Some(dir) = self.developer_override() {
            return Ok(dir.to_path_buf());
        }
        match std::fs::read_link(&self.xcode_select_link) {
            Ok(dir) => {
                return Ok(if dir.is_absolute() {
                    dir
                } else {
                    self.xcode_select_link
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(dir)
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not read {}: {error}",
                    self.xcode_select_link.display()
                ));
            }
        }
        if let Some(dir) = &self.xcode_developer_dir
            && holds_clang(dir)?
        {
            return Ok(dir.clone());
        }
        Ok(self.default_developer_dir.clone())
    }

    fn sdkroot(&self) -> Option<&Path> {
        self.sdkroot_override
            .as_deref()
            .filter(|sdk| !sdk.as_os_str().is_empty())
    }

    fn deployment_target(&self) -> Option<&OsStr> {
        self.deployment_target_override
            .as_deref()
            .filter(|target| !target.is_empty())
    }

    fn apply_environment(&self, cmd: &mut aterm_uds::spawnfd::Command) {
        if !self.inherit_environment {
            cmd.env_clear().env("TMPDIR", &self.tmp_root);
        }
        cmd.env_remove("DEVELOPER_DIR")
            .env_remove("SDKROOT")
            .env_remove("MACOSX_DEPLOYMENT_TARGET");
        if let Some(developer) = self.developer_override() {
            cmd.env("DEVELOPER_DIR", developer);
        }
        if let Some(sdk) = self.sdkroot() {
            cmd.env("SDKROOT", sdk);
        }
        if let Some(target) = self.deployment_target() {
            cmd.env("MACOSX_DEPLOYMENT_TARGET", target);
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
    /// changes its identity or contents and retires the verdict. An unread receipt cannot
    /// supply a cache key.
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
/// verdict is probed every time. When a key is available, revalidate it before returning a
/// verdict or publishing a cached fact; an observed change leaves the current toolchain unproven.
/// A native driver the text cache cannot represent is returned without caching.
#[must_use]
pub fn probe_cached(path_var: Option<&OsStr>, host: &CcHost, cache: Option<&CcCache>) -> CcVerdict {
    const CHANGED: &str = "C toolchain inputs changed or became unreadable during the check";
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
        let driver = PathBuf::from(driver);
        if cc_key(&located, path_var, host, cache).as_ref() != Some(key) {
            return CcVerdict::Unproven {
                driver,
                why: CHANGED.into(),
            };
        }
        return CcVerdict::Ready { driver };
    }
    let verdict = probe_located(&located, path_var, host);
    if let Some((cache, key)) = &kept {
        if cc_key(&located, path_var, host, cache).as_ref() != Some(key) {
            return CcVerdict::Unproven {
                driver: match &located {
                    Located::Plain(driver) | Located::Shim(driver) => driver.clone(),
                },
                why: CHANGED.into(),
            };
        }
        if let CcVerdict::Ready { driver } = &verdict
            && let Some(driver) = driver.to_str()
        {
            crate::probe_cache::put(&cache.file, key, driver);
        }
    }
    verdict
}

/// What a `Ready` verdict for `located` depends on, read without spawning: the compiler as it is
/// now ([`crate::probe_cache::identity`]), the shim's discovery executable, the active developer
/// directory (`DEVELOPER_DIR`, else `xcode-select`'s link, else Xcode or the standalone tools),
/// the identity of its `clang`,
/// the Command Line Tools receipt's identity and digest, the SDK a build would fall back to and the `PATH`
/// the build sees and its minimum macOS deployment target. `None` when project configuration
/// is inherited, the compiler cannot be read, discovery is unreadable, or an explicit SDK or
/// developer directory is used. Project configuration can name arbitrary mutable inputs;
/// changes within an explicit SDK have no Apple install receipt to retire a cached success. The policy
/// domain retires verdicts made by weaker discovery, output, SDK custody, native configuration
/// identity or snapshot validation checks. Native PATH and deployment-target bytes are encoded
/// exactly; unrepresentable SDK/developer paths disable caching rather than supply a lossy identity.
fn cc_key(
    located: &Located,
    path_var: Option<&OsStr>,
    host: &CcHost,
    cache: &CcCache,
) -> Option<String> {
    if host.inherit_environment || host.sdkroot().is_some() || host.developer_override().is_some() {
        return None;
    }
    let (kind, cc) = match located {
        Located::Plain(cc) => ("plain", cc),
        Located::Shim(cc) => ("shim", cc),
    };
    let discovery = match located {
        Located::Plain(_) => "unused".to_owned(),
        Located::Shim(cc) => crate::probe_cache::identity(&discovery_program(cc))?,
    };
    let developer_dir = host.developer_dir().ok()?;
    let receipt = receipt_key(&cache.receipt)?;
    let sdk = match std::fs::canonicalize(developer_dir.join("SDKs/MacOSX.sdk")) {
        Ok(sdk) => sdk.to_str()?.to_owned(),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            "no sdk".to_owned()
        }
        Err(_) => return None,
    };
    // The compiler the developer directory holds, where `locate` looks for it: an Xcode
    // install, update or replacement rewrites it while the Command Line Tools receipt stays,
    // and an update that leaves `cc` refusing (a license not yet agreed to) must probe again.
    let clang = clang_paths(&developer_dir)
        .iter()
        .map(|clang| match std::fs::canonicalize(clang) {
            Ok(real) => {
                real.to_str()?;
                let metadata = std::fs::metadata(&real).ok()?;
                crate::probe_cache::metadata_identity(&real, &metadata)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Some("absent".to_owned())
            }
            Err(_) => None,
        })
        .collect::<Option<Vec<_>>>()?
        .join(" ");
    fn native(value: Option<&OsStr>) -> String {
        value
            .map(|v| aterm_codec::hex::encode_lower(v.as_encoded_bytes()))
            .unwrap_or_default()
    }
    crate::probe_cache::key([
        Some("fresh-executable-link-v13".to_owned()),
        Some(kind.to_owned()),
        crate::probe_cache::identity(cc),
        Some(discovery),
        Some(developer_dir.to_str()?.to_owned()),
        Some(clang),
        Some(receipt),
        Some(sdk),
        Some(native(path_var)),
        Some(native(host.deployment_target())),
    ])
}

/// Read a bounded regular receipt through one held file. Only absence supplies a missing
/// receipt fact; unread, linked, nonregular and oversized receipts disable caching.
fn receipt_key(path: &Path) -> Option<String> {
    use std::io::Read as _;
    const CAP: u64 = 64 * 1024;
    let (file, metadata) = match aterm_uds::regular_file::open(path) {
        Ok(opened) => opened,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some("receipt-v1 absent".to_owned());
        }
        Err(_) => return None,
    };
    if metadata.len() > CAP {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(CAP + 1).read_to_end(&mut bytes).ok()?;
    if u64::try_from(bytes.len()).ok()? > CAP {
        return None;
    }
    let mut digest = aterm_digest::Sha256::new();
    digest.update(&bytes);
    Some(format!(
        "receipt-v1 {} {}",
        crate::probe_cache::metadata_identity(path, &metadata)?,
        aterm_codec::hex::encode_lower(&digest.finalize())
    ))
}

/// [`probe`] over an injected host.
#[must_use]
pub fn probe_with(path_var: Option<&OsStr>, host: &CcHost) -> CcVerdict {
    probe_with_elapsed(path_var, host, Duration::ZERO)
}

pub(crate) fn probe_with_elapsed(
    path_var: Option<&OsStr>,
    host: &CcHost,
    elapsed: Duration,
) -> CcVerdict {
    if cfg!(windows) {
        return CcVerdict::NotProbed;
    }
    match locate(path_var, host) {
        Ok(located) => {
            let Some(left) = host
                .bound
                .checked_sub(elapsed)
                .filter(|left| !left.is_zero())
            else {
                let driver = match located {
                    Located::Plain(driver) | Located::Shim(driver) => driver,
                };
                return CcVerdict::Unanswered {
                    driver,
                    bound: host.bound,
                };
            };
            let mut remaining = host.clone();
            remaining.bound = left;
            probe_located(&located, path_var, &remaining)
        }
        Err(verdict) => verdict,
    }
}

/// [`probe_with`] past [`locate`]: the compiler asked, then the build probe.
fn probe_located(located: &Located, path_var: Option<&OsStr>, host: &CcHost) -> CcVerdict {
    let cc = match located {
        Located::Plain(cc) => {
            return match ask(cc, cc, CcQuery::Version, host) {
                CcVerdict::Ready { driver } => build_probe(cc, driver, path_var, host),
                other => other,
            };
        }
        Located::Shim(cc) => cc,
    };
    match ask(&discovery_program(cc), cc, CcQuery::Find, host) {
        CcVerdict::Ready { driver } => build_probe(cc, driver, path_var, host),
        other => other,
    }
}

/// Whether a C toolchain is installed here, read without running anything. `None` on Windows
/// or when discovery cannot establish presence or absence.
#[must_use]
pub fn installed_with(path_var: Option<&OsStr>, host: &CcHost) -> Option<bool> {
    if cfg!(windows) {
        return None;
    }
    match locate(path_var, host) {
        Ok(_) => Some(true),
        Err(CcVerdict::NoDriver | CcVerdict::NoDeveloperDir) => Some(false),
        Err(_) => None,
    }
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
/// there on absence, or [`CcVerdict::Unproven`] when discovery is unreadable. Nothing is spawned.
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

/// Literal matches remain meaningful when a directory is absent or unreadable; readable
/// aliases compare by resolved directory without rewriting the selected invocation path.
fn same_directory(selected: &Path, known: &Path) -> bool {
    selected == known
        || matches!(
            (std::fs::canonicalize(selected), std::fs::canonicalize(known)),
            (Ok(selected), Ok(known)) if selected == known
        )
}

/// Setup's temporary tools report that installation is needed; they are not compilers.
/// Look past them before deciding whether the actual compiler is missing or broken.
fn locate_without_setup_shim(
    path_var: Option<&OsStr>,
    host: &CcHost,
    setup_shims: &[Option<&Path>],
) -> Result<Located, CcVerdict> {
    let mut found = None;
    for dir in path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .filter(|dir| {
            !setup_shims
                .iter()
                .flatten()
                .any(|shim| same_directory(dir, shim))
        })
    {
        let candidate = dir.join("cc");
        match is_executable(&candidate) {
            Ok(true) => {
                found = Some(candidate);
                break;
            }
            Ok(false) => {}
            Err(why) => {
                return Err(CcVerdict::Unproven {
                    driver: candidate,
                    why,
                });
            }
        }
    }
    let cc = found.ok_or(CcVerdict::NoDriver)?;
    let is_shim = host
        .shim_dir
        .as_deref()
        .is_some_and(|shim| cc.parent().is_some_and(|dir| same_directory(dir, shim)));
    if !is_shim {
        return Ok(Located::Plain(cc));
    }
    let unread = |why| CcVerdict::Unproven {
        driver: cc.clone(),
        why,
    };
    let developer_dir = host.developer_dir().map_err(unread)?;
    if holds_clang(&developer_dir).map_err(unread)? {
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

fn holds_clang(developer: &Path) -> Result<bool, String> {
    for clang in clang_paths(developer) {
        if is_executable(&clang)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The build probe's program: the include proves the headers (the SDK on macOS, libc's on
/// Linux) resolve.
const PROBE_SOURCE: &str = "#include <stdio.h>\nint main(void) { return 0; }\n";

/// Make `cc` compile and link [`PROBE_SOURCE`] in a private directory under
/// `host.tmp_root`, each attempt within `host.bound`: `Ready { driver: resolved }` only when it exits 0
/// and wrote a non-empty executable program into this attempt's fresh output namespace.
/// An inspection error other than absence leaves that result unproven. The program is never
/// run. Its private directory's guard attempts cleanup on return.
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
    build_probe_in(cc, resolved, path_var, host, scratch.path())
}

/// Compile inside the caller's fresh private directory; a retry creates its own exclusive
/// output directory instead of depending on removal of a failed attempt's artifact.
fn build_probe_in(
    cc: &Path,
    resolved: PathBuf,
    path_var: Option<&OsStr>,
    host: &CcHost,
    dir: &Path,
) -> CcVerdict {
    let source = dir.join("probe.c");
    let mut program = dir.join("probe.out");
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
    let run_cc = |sdkroot: Option<&Path>, program: &Path| {
        let mut cmd = aterm_uds::spawnfd::command(cc);
        cmd.arg("-o").arg(program).arg(&source).current_dir(dir);
        host.apply_environment(&mut cmd);
        if let Some(path) = path_var {
            cmd.env("PATH", path);
        }
        if let Some(sdk) = sdkroot {
            cmd.env("SDKROOT", sdk);
        }
        compiler_output(&mut cmd, cc, host.bound)
    };
    let mut result = run_cc(None, &program);
    if matches!(&result, Ok(out) if !out.status.success())
        && host.sdkroot().is_none()
        && host
            .shim_dir
            .as_deref()
            .is_some_and(|shim| cc.parent().is_some_and(|dir| same_directory(dir, shim)))
    {
        let developer_dir = match host.developer_dir() {
            Ok(dir) => dir,
            Err(why) => {
                return CcVerdict::Unproven {
                    driver: resolved,
                    why,
                };
            }
        };
        let sdk = developer_dir.join("SDKs/MacOSX.sdk");
        let has_sdk = match std::fs::metadata(&sdk) {
            Ok(metadata) => metadata.is_dir(),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                false
            }
            Err(error) => {
                return CcVerdict::Unproven {
                    driver: resolved,
                    why: format!("could not inspect {}: {error}", sdk.display()),
                };
            }
        };
        if has_sdk {
            let fresh = dir.join("sdk-fallback");
            if let Err(error) = std::fs::create_dir(&fresh) {
                return CcVerdict::Unproven {
                    driver: resolved,
                    why: format!(
                        "could not create fresh probe output directory {}: {error}",
                        fresh.display()
                    ),
                };
            }
            program = fresh.join("probe.out");
            result = run_cc(Some(&sdk), &program);
        }
    }
    match result {
        Err(verdict) => verdict,
        Ok(out) if out.status.success() => {
            let wrote = match std::fs::symlink_metadata(&program) {
                Ok(metadata) => is_executable_file(&metadata),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => {
                    return CcVerdict::Unproven {
                        driver: resolved,
                        why: format!("could not inspect {}: {error}", program.display()),
                    };
                }
            };
            if wrote {
                CcVerdict::Ready { driver: resolved }
            } else {
                CcVerdict::Broken {
                    driver: cc.to_path_buf(),
                    why: "it reported success without a nonempty executable output".to_string(),
                }
            }
        }
        Ok(out) => CcVerdict::Broken {
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

#[derive(Clone, Copy)]
enum CcQuery {
    Version,
    Find,
}

fn discovery_program(cc: &Path) -> PathBuf {
    cc.parent().unwrap_or(Path::new("/usr/bin")).join("xcrun")
}

/// Preserve subprocess evidence: only deadline expiry means the compiler did not answer.
fn compiler_output(
    cmd: &mut aterm_uds::spawnfd::Command,
    driver: &Path,
    bound: Duration,
) -> Result<std::process::Output, CcVerdict> {
    aterm_uds::deadline::output_within(cmd, bound).map_err(|error| match error {
        aterm_uds::deadline::RunError::TimedOut => CcVerdict::Unanswered {
            driver: driver.to_path_buf(),
            bound,
        },
        error => CcVerdict::Unproven {
            driver: driver.to_path_buf(),
            why: format!("compiler process could not be observed: {error}"),
        },
    })
}

/// Run the query within `bound`. A successful version query retains `driver`; discovery
/// requires one absolute native path. A captured nonzero exit is Broken with the first stderr line.
fn ask(bin: &Path, driver: &Path, query: CcQuery, host: &CcHost) -> CcVerdict {
    let bound = host.bound;
    let mut cmd = aterm_uds::spawnfd::command(bin);
    let args: &[&str] = match query {
        CcQuery::Version => &["--version"],
        CcQuery::Find => &["--find", "cc"],
    };
    cmd.args(args);
    host.apply_environment(&mut cmd);
    let output = match query {
        CcQuery::Version => match compiler_output(&mut cmd, driver, bound) {
            Ok(out) => Some(out),
            Err(verdict) => return verdict,
        },
        CcQuery::Find => {
            use aterm_uds::deadline::{CompleteOutputError, RunError};
            match aterm_uds::deadline::output_until_complete_stdout(
                &mut cmd,
                std::time::Instant::now() + bound,
            ) {
                Ok(out) => Some(out),
                Err(CompleteOutputError::Run(RunError::TimedOut)) => None,
                Err(error) => {
                    return CcVerdict::Unproven {
                        driver: driver.to_path_buf(),
                        why: format!("compiler discovery output is unavailable: {error}"),
                    };
                }
            }
        }
    };
    match output {
        None => CcVerdict::Unanswered {
            driver: driver.to_path_buf(),
            bound,
        },
        Some(out) if out.status.success() => {
            let named = match query {
                CcQuery::Version => Some(driver.to_path_buf()),
                CcQuery::Find => discovered_compiler(&out.stdout),
            };
            match named {
                Some(driver) => CcVerdict::Ready { driver },
                None => CcVerdict::Unproven {
                    driver: driver.to_path_buf(),
                    why: "compiler discovery did not name one absolute path".to_owned(),
                },
            }
        }
        Some(out) => CcVerdict::Broken {
            driver: driver.to_path_buf(),
            why: first_stderr_line(&out),
        },
    }
}

/// Remove one protocol line ending without trimming path bytes or accepting multiple records.
fn discovered_compiler(bytes: &[u8]) -> Option<PathBuf> {
    let bytes = bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(bytes);
    if bytes
        .iter()
        .any(|byte| matches!(byte, b'\0' | b'\r' | b'\n'))
    {
        return None;
    }
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStrExt as _;
        PathBuf::from(OsStr::from_bytes(bytes))
    };
    #[cfg(not(unix))]
    let path = PathBuf::from(std::str::from_utf8(bytes).ok()?);
    path.is_absolute().then_some(path)
}

/// A nonempty regular file this user may execute (never a directory named `cc`).
fn is_executable(path: &Path) -> Result<bool, String> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(is_executable_file(&metadata)),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(format!("could not inspect {}: {error}", path.display())),
    }
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
            deployment_target_override: None,
            inherit_environment: false,
            bound: ANSWER,
            tmp_root,
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_sdk_retry_cannot_accept_an_unremovable_failed_attempts_output() {
        struct Unflag(PathBuf);
        impl Drop for Unflag {
            fn drop(&mut self) {
                let _ = crate::platform::set_file_flags(&self.0, 0);
            }
        }
        let root = scratch("sdk-fresh-output");
        let bin = root.join("bin");
        let cc = bin.join("cc");
        let developer = root.join("developer");
        std::fs::create_dir_all(developer.join("SDKs/MacOSX.sdk")).unwrap();
        script(
            &cc,
            "if [ -n \"$SDKROOT\" ]; then\n  if [ -f \"$0.write-fallback\" ]; then printf fresh > \"$2\"; /bin/chmod +x \"$2\"; fi\n  exit 0\nfi\nprintf stale > \"$2\"\n/bin/chmod +x \"$2\"\n/usr/bin/chflags uchg \"$2\" || exit 2\nexit 1",
        );
        let alias = root.join("bin-alias");
        std::os::unix::fs::symlink(&bin, &alias).unwrap();
        let cc = alias.join("cc");
        let host = CcHost {
            shim_dir: Some(bin),
            default_developer_dir: developer,
            ..plain_host(&root)
        };
        for emits in [false, true] {
            let dir = root.join(if emits { "fresh" } else { "stale" });
            std::fs::create_dir_all(&dir).unwrap();
            let failed_output = dir.join("probe.out");
            let _unflag = Unflag(failed_output.clone());
            if emits {
                std::fs::write(cc.with_file_name("cc.write-fallback"), b"yes").unwrap();
            }
            let verdict = build_probe_in(&cc, cc.clone(), None, &host, &dir);
            assert!(
                std::fs::remove_file(&failed_output)
                    .is_err_and(|error| { error.kind() == std::io::ErrorKind::PermissionDenied }),
                "the immutable fixture blocks unlink"
            );
            if emits {
                assert_eq!(verdict, CcVerdict::Ready { driver: cc.clone() });
            } else {
                assert!(matches!(verdict, CcVerdict::Broken { .. }), "{verdict:?}");
            }
        }
    }

    #[test]
    fn an_sdk_retry_refuses_an_existing_output_namespace() {
        let root = scratch("occupied-sdk-output");
        let bin = root.join("bin");
        let cc = bin.join("cc");
        script(&cc, "if [ -n \"$SDKROOT\" ]; then exit 0; fi; exit 1");
        let developer = root.join("developer");
        std::fs::create_dir_all(developer.join("SDKs/MacOSX.sdk")).unwrap();
        let host = CcHost {
            shim_dir: Some(bin),
            default_developer_dir: developer,
            ..plain_host(&root)
        };
        let dir = root.join("probe");
        let occupied = dir.join("sdk-fallback/probe.out");
        std::fs::create_dir_all(occupied.parent().unwrap()).unwrap();
        std::fs::write(&occupied, b"old executable").unwrap();
        std::fs::set_permissions(&occupied, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            build_probe_in(&cc, cc.clone(), None, &host, &dir),
            CcVerdict::Unproven { .. }
        ));
        assert_eq!(std::fs::read(&occupied).unwrap(), b"old executable");
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
        let setup_alias = root.join("setup-alias");
        let inherited_alias = root.join("inherited-alias");
        let apple = root.join("usr-bin");
        let apple_alias = root.join("apple-alias");
        // These must never run: discovery alone can tell the tools are missing.
        script(&setup.join("cc"), "exit 69");
        script(&inherited.join("cc"), "exit 69");
        script(&apple.join("cc"), "exit 1");
        std::os::unix::fs::symlink(&setup, &setup_alias).unwrap();
        std::os::unix::fs::symlink("old-home/clt-shim", &inherited_alias).unwrap();
        std::os::unix::fs::symlink(&apple, &apple_alias).unwrap();
        let host = CcHost {
            shim_dir: Some(apple.clone()),
            ..plain_host(&root)
        };
        for (setup_entry, inherited_entry, apple_entry) in [
            (&setup, &inherited, &apple),
            (&setup_alias, &inherited_alias, &apple_alias),
        ] {
            let path = std::env::join_paths([inherited_entry, setup_entry, apple_entry]).unwrap();
            assert!(matches!(
                locate_without_setup_shim(Some(&path), &host, &[Some(&setup), Some(&inherited)]),
                Err(CcVerdict::NoDeveloperDir)
            ));
            // An unmarked PATH compiler retains priority, including its failures and aliases.
            assert!(matches!(
                locate_without_setup_shim(Some(&path), &host, &[Some(&setup)]),
                Ok(Located::Plain(cc)) if cc == inherited_entry.join("cc")
            ));
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

    #[test]
    fn a_version_banner_cannot_rename_the_compiler() {
        let root = scratch("version-banner");
        let bin = root.join("bin");
        let cc = bin.join("cc");
        script(
            &cc,
            "[ \"$1\" = --version ] && { echo /not-the-compiler; exit 0; }\nprintf x > \"$2\"\n/bin/chmod +x \"$2\"",
        );
        assert_eq!(
            probe_with(Some(bin.as_os_str()), &plain_host(&root)),
            CcVerdict::Ready { driver: cc }
        );
    }

    #[test]
    fn compiler_discovery_requires_one_absolute_native_path() {
        use std::os::unix::ffi::OsStringExt as _;
        let root = scratch("discovery-output");
        let bin = root.join("bin");
        let answer = root.join("answer");
        let compiled = root.join("compiled");
        script(
            &bin.join("cc"),
            &format!(
                "echo compiled >> '{}'\nprintf x > \"$2\"\n/bin/chmod +x \"$2\"",
                compiled.display()
            ),
        );
        script(
            &bin.join("xcrun"),
            &format!("/bin/cat '{}'", answer.display()),
        );
        let developer = root.join("developer");
        script(&developer.join("usr/bin/clang"), "exit 0");
        let host = CcHost {
            shim_dir: Some(bin.clone()),
            default_developer_dir: developer,
            ..plain_host(&root)
        };
        let oversized = vec![b'/'; aterm_uds::deadline::OUTPUT_KEEP + 1];
        for invalid in [
            b"".as_slice(),
            b"relative/cc\n",
            b"/one\n/two\n",
            b"/cc\0\n",
            oversized.as_slice(),
        ] {
            std::fs::write(&answer, invalid).unwrap();
            assert!(
                matches!(
                    probe_with(Some(bin.as_os_str()), &host),
                    CcVerdict::Unproven { .. }
                ),
                "invalid discovery output length: {}",
                invalid.len()
            );
            assert!(!compiled.exists(), "unproven discovery starts no build");
        }
        for (index, name) in [b"compiler \xc2\xa0".as_slice(), b"compiler-\xff "]
            .into_iter()
            .enumerate()
        {
            let path = root.join(std::ffi::OsString::from_vec(name.to_vec()));
            let mut bytes = path.as_os_str().as_encoded_bytes().to_vec();
            bytes.extend_from_slice(b"\r\n");
            std::fs::write(&answer, bytes).unwrap();
            let cache = CcCache {
                file: root.join(format!("state/cc-{index}")),
                receipt: root.join("no-receipt"),
            };
            for _ in 0..2 {
                assert_eq!(
                    probe_cached(Some(bin.as_os_str()), &host, Some(&cache)),
                    CcVerdict::Ready {
                        driver: path.clone()
                    }
                );
            }
        }
        assert_eq!(
            std::fs::read_to_string(compiled).unwrap().lines().count(),
            3
        );
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
            deployment_target_override: None,
            inherit_environment: true,
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
        std::os::unix::fs::symlink(xcode.strip_prefix(&root).unwrap(), &host.xcode_select_link)
            .unwrap();
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

    #[test]
    fn changing_developer_compiler_execute_permissions_retires_a_cached_success() {
        let mut observations = Vec::new();
        for index in 0..2 {
            let root = scratch("clang-mode");
            let bin = root.join("bin");
            let cc = bin.join("cc");
            let developer = root.join("developer");
            let paths = clang_paths(&developer);
            for clang in &paths {
                script(clang, "exit 0");
            }
            let changed = &paths[index];
            script(
                &cc,
                &format!(
                    "case \"$1\" in -o) if [ ! -x '{}' ]; then echo 'compiler is not executable' >&2; exit 1; fi; printf x > \"$2\"; /bin/chmod +x \"$2\";; esac",
                    changed.display()
                ),
            );
            script(&bin.join("xcrun"), &format!("echo '{}'", cc.display()));
            let host = CcHost {
                shim_dir: Some(bin.clone()),
                default_developer_dir: developer,
                inherit_environment: false,
                ..plain_host(&root)
            };
            let cache = CcCache {
                file: root.join("state/cc"),
                receipt: root.join("no-receipt"),
            };
            let path = Some(bin.as_os_str());
            assert!(matches!(
                probe_cached(path, &host, Some(&cache)),
                CcVerdict::Ready { .. }
            ));
            let before = std::fs::metadata(changed).unwrap();
            std::fs::set_permissions(changed, std::fs::Permissions::from_mode(0o644)).unwrap();
            let after = std::fs::metadata(changed).unwrap();
            assert_eq!(before.modified().unwrap(), after.modified().unwrap());
            assert_eq!(before.len(), after.len());
            let cached = probe_cached(path, &host, Some(&cache));
            let direct = probe_with(path, &host);
            observations.push((index, direct, cached));
            std::fs::set_permissions(changed, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(matches!(probe_with(path, &host), CcVerdict::Ready { .. }));
            assert!(matches!(
                probe_cached(path, &host, Some(&cache)),
                CcVerdict::Ready { .. }
            ));
        }
        assert!(
            observations.iter().all(|(_, direct, cached)| matches!(
                direct,
                CcVerdict::Broken { .. }
            ) && direct == cached),
            "{observations:#?}"
        );
    }

    #[test]
    fn unreadable_optional_clang_identity_cannot_reuse_an_absence_fact() {
        let mut observations = Vec::new();
        for shim in [false, true] {
            let root = scratch("optional-clang");
            let bin = root.join("bin");
            let cc = bin.join("cc");
            let developer = root.join("developer");
            let paths = clang_paths(&developer);
            let fault = &paths[usize::from(shim)];
            if shim {
                script(&paths[0], "exit 0");
            }
            script(
                &cc,
                &format!(
                    "case \"$1\" in --version) echo cc;; -o) if [ -L '{}' ]; then echo 'compiler state unread' >&2; exit 1; fi; printf x > \"$2\"; /bin/chmod +x \"$2\";; esac",
                    fault.display()
                ),
            );
            script(&bin.join("xcrun"), &format!("echo '{}'", cc.display()));
            let host = CcHost {
                shim_dir: shim.then_some(bin.clone()),
                default_developer_dir: developer,
                inherit_environment: false,
                ..plain_host(&root)
            };
            let cache = CcCache {
                file: root.join("state/cc"),
                receipt: root.join("no-receipt"),
            };
            let path = Some(bin.as_os_str());
            assert!(matches!(
                probe_cached(path, &host, Some(&cache)),
                CcVerdict::Ready { .. }
            ));
            std::fs::create_dir_all(fault.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(fault, fault).unwrap();
            let cached = probe_cached(path, &host, Some(&cache));
            let direct = probe_with(path, &host);
            observations.push((shim, direct, cached));
            std::fs::remove_file(fault).unwrap();
            assert!(matches!(probe_with(path, &host), CcVerdict::Ready { .. }));
            assert!(matches!(
                probe_cached(path, &host, Some(&cache)),
                CcVerdict::Ready { .. }
            ));
        }
        assert!(
            observations.iter().all(|(_, direct, cached)| matches!(
                direct,
                CcVerdict::Broken { .. }
            ) && cached == direct),
            "{observations:#?}"
        );
    }

    #[test]
    fn an_unreadable_fallback_sdk_is_unproven_and_cannot_reuse_absence() {
        let root = scratch("unread-sdk");
        let bin = root.join("bin");
        let cc = bin.join("cc");
        let developer = root.join("developer");
        let sdk = developer.join("SDKs/MacOSX.sdk");
        script(&developer.join("usr/bin/clang"), "exit 0");
        script(&bin.join("xcrun"), &format!("echo '{}'", cc.display()));
        script(
            &cc,
            "case \"$1\" in --version) echo cc;; -o) if [ -f \"$0.refuse\" ] && [ -z \"$SDKROOT\" ]; then echo 'SDK needed' >&2; exit 1; fi; printf x > \"$2\"; /bin/chmod +x \"$2\";; esac",
        );
        let host = CcHost {
            shim_dir: Some(bin.clone()),
            default_developer_dir: developer,
            inherit_environment: false,
            ..plain_host(&root)
        };
        let cache = CcCache {
            file: root.join("state/cc"),
            receipt: root.join("no-receipt"),
        };
        let path = Some(bin.as_os_str());
        assert!(matches!(
            probe_cached(path, &host, Some(&cache)),
            CcVerdict::Ready { .. }
        ));
        std::fs::write(cc.with_file_name("cc.refuse"), b"yes").unwrap();
        assert!(matches!(probe_with(path, &host), CcVerdict::Broken { .. }));
        std::fs::create_dir_all(sdk.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&sdk, &sdk).unwrap();
        assert_ne!(
            std::fs::metadata(&sdk).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        let direct = probe_with(path, &host);
        let cached = probe_cached(path, &host, Some(&cache));
        assert!(
            matches!(direct, CcVerdict::Unproven { .. })
                && matches!(cached, CcVerdict::Unproven { .. }),
            "direct={direct:?}, cached={cached:?}"
        );
        std::fs::remove_file(&sdk).unwrap();
        std::fs::create_dir(&sdk).unwrap();
        assert!(matches!(
            probe_cached(path, &host, Some(&cache)),
            CcVerdict::Ready { .. }
        ));
    }

    #[test]
    fn unreadable_discovery_is_unknown_and_can_recover_without_installing() {
        let mut observations = Vec::new();
        for kind in ["path", "clang", "xcode", "selection"] {
            let root = scratch("unread-discovery");
            let bin = root.join("bin");
            let cc = bin.join("cc");
            let ran = root.join("program-ran");
            building_cc(&cc, &ran);
            script(&bin.join("xcrun"), &format!("echo '{}'", cc.display()));
            let developer = root.join("developer");
            script(&developer.join("usr/bin/clang"), "exit 0");
            let fault = root.join("unread");
            std::os::unix::fs::symlink(&fault, &fault).unwrap();
            let mut host = CcHost {
                default_developer_dir: developer.clone(),
                ..plain_host(&root)
            };
            let path = if kind == "path" {
                fault.clone()
            } else {
                host.shim_dir = Some(bin.clone());
                match kind {
                    "clang" => host.default_developer_dir = fault.clone(),
                    "xcode" => host.xcode_developer_dir = Some(fault.clone()),
                    "selection" => host.xcode_select_link = fault.join("selection"),
                    _ => unreachable!(),
                }
                bin.clone()
            };
            observations.push((
                kind,
                probe_with(Some(path.as_os_str()), &host),
                installed_with(Some(path.as_os_str()), &host),
            ));

            std::fs::remove_file(&fault).unwrap();
            if kind == "selection" {
                std::fs::create_dir(&fault).unwrap();
                std::os::unix::fs::symlink(&developer, fault.join("selection")).unwrap();
            } else {
                std::os::unix::fs::symlink(if kind == "path" { &bin } else { &developer }, &fault)
                    .unwrap();
            }
            assert_eq!(
                installed_with(Some(path.as_os_str()), &host),
                Some(true),
                "{kind}"
            );
            assert!(
                matches!(
                    probe_with(Some(path.as_os_str()), &host),
                    CcVerdict::Ready { .. }
                ),
                "{kind}"
            );
            let cache = CcCache {
                file: root.join("state/cc"),
                receipt: root.join("no-receipt"),
            };
            assert!(matches!(
                probe_cached(Some(path.as_os_str()), &host, Some(&cache)),
                CcVerdict::Ready { .. }
            ));
            assert!(cache.file.is_file(), "{kind}: the success was kept");
            if kind == "selection" {
                std::fs::remove_dir_all(&fault).unwrap();
            } else {
                std::fs::remove_file(&fault).unwrap();
            }
            std::os::unix::fs::symlink(&fault, &fault).unwrap();
            assert!(
                matches!(
                    probe_cached(Some(path.as_os_str()), &host, Some(&cache)),
                    CcVerdict::Unproven { .. }
                ),
                "{kind}: a warm cache cannot hide unread discovery"
            );
            assert!(!ran.exists(), "the probe's program is never executed");
        }
        assert!(
            observations.iter().all(|(_, verdict, installed)| {
                matches!(verdict, CcVerdict::Unproven { .. })
                    && !verdict.blocks_builds()
                    && installed.is_none()
            }),
            "{observations:#?}"
        );
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

    #[test]
    fn unreadable_output_metadata_is_unproven_and_a_fresh_attempt_can_recover() {
        let root = scratch("unread-output");
        let host = plain_host(&root);
        let cc = root.join("bin/cc");
        let disrupt = cc.with_file_name("cc.disrupt");
        script(
            &cc,
            "printf x > \"$2\"; /bin/chmod +x \"$2\"\nif [ -f \"$0.disrupt\" ]; then\nprobe_dir=$(/bin/pwd)\n/bin/mv \"$probe_dir\" \"$probe_dir.saved\" || exit 2\n/bin/ln -s \"$probe_dir\" \"$probe_dir\"\nfi",
        );
        std::fs::write(&disrupt, b"disrupt").unwrap();
        let dir = root.join("first");
        std::fs::create_dir(&dir).unwrap();
        let verdict = build_probe_in(&cc, cc.clone(), None, &host, &dir);
        assert!(root.join("first.saved/probe.out").is_file());
        let error = std::fs::symlink_metadata(dir.join("probe.out")).unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(
            matches!(&verdict, CcVerdict::Unproven { .. }),
            "{verdict:?}"
        );
        assert!(!verdict.blocks_builds());

        std::fs::remove_file(disrupt).unwrap();
        let next = root.join("next");
        std::fs::create_dir(&next).unwrap();
        assert_eq!(
            build_probe_in(&cc, cc.clone(), None, &host, &next),
            CcVerdict::Ready { driver: cc }
        );
    }

    #[test]
    fn a_compiler_launch_refusal_is_unproven_and_repair_can_be_kept() {
        for phase in ["version", "compile"] {
            let root = scratch("launch-refused");
            let bin = root.join("bin");
            let cc = bin.join("cc");
            std::fs::create_dir_all(&bin).unwrap();
            let broken = format!("#!{}\n", root.join("missing-interpreter").display());
            if phase == "compile" {
                script(
                    &cc,
                    "if [ \"$1\" = --version ]; then /bin/mv \"$0.next\" \"$0\"; echo cc; exit 0; fi\nexit 9",
                );
                std::fs::write(bin.join("cc.next"), &broken).unwrap();
                std::fs::set_permissions(
                    bin.join("cc.next"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            } else {
                std::fs::write(&cc, &broken).unwrap();
                std::fs::set_permissions(&cc, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let host = plain_host(&root);
            let path = Some(bin.as_os_str());
            let refused = probe_with(path, &host);
            assert!(
                matches!(&refused, CcVerdict::Unproven { why, .. } if why.contains("could not start")),
                "{phase}: {refused:?}"
            );
            script(
                &cc,
                "echo asked >> \"${0%/bin/cc}/asked\"\nif [ \"$1\" = --version ]; then echo cc; else printf x > \"$2\"; /bin/chmod +x \"$2\"; fi",
            );
            let cache = CcCache {
                file: root.join("state/cc"),
                receipt: root.join("no-receipt"),
            };
            let ready = CcVerdict::Ready { driver: cc };
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(
                std::fs::read_to_string(root.join("asked"))
                    .unwrap()
                    .lines()
                    .count(),
                2
            );
        }
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

    /// A receipt changed during a probe invalidates both its success and its failure. Restoring
    /// the exact old receipt must not retrieve a success made against the new toolchain.
    #[test]
    fn a_receipt_changed_during_the_probe_leaves_no_stale_verdict() {
        use std::os::unix::fs::MetadataExt as _;

        for phase in ["version", "compile"] {
            let root = scratch("changing-receipt");
            let bin = root.join("bin");
            let cc = bin.join("cc");
            script(
                &cc,
                "set -e\nroot=${0%/bin/cc}\necho asked >> \"$root/asked\"\nchange_receipt() { /bin/mv \"$root/receipt\" \"$root/receipt.saved\"; /bin/mv \"$root/receipt.next\" \"$root/receipt\"; /bin/rm \"$root/change\"; }\ncase \"$1\" in\n--version) if [ -f \"$root/change\" ] && [ \"$(/bin/cat \"$root/change\")\" = version ]; then change_receipt; fi; echo cc;;\n-o) if [ \"$(/bin/cat \"$root/receipt\")\" != new ]; then if [ -f \"$root/change\" ]; then change_receipt; fi; echo 'old tools cannot link' >&2; exit 1; fi; printf x > \"$2\"; /bin/chmod +x \"$2\";;\nesac",
            );
            let host = CcHost {
                inherit_environment: false,
                ..plain_host(&root)
            };
            let receipt = root.join("receipt");
            let next = root.join("receipt.next");
            let saved = root.join("receipt.saved");
            std::fs::write(&receipt, b"old").unwrap();
            std::fs::write(&next, b"new").unwrap();
            std::fs::write(root.join("change"), phase.as_bytes()).unwrap();
            let before = std::fs::metadata(&receipt).unwrap();
            let cache = CcCache {
                file: root.join("state/cc"),
                receipt: receipt.clone(),
            };
            let path = Some(bin.as_os_str());
            let changed = probe_cached(path, &host, Some(&cache));
            assert_eq!(std::fs::read(&receipt).unwrap(), b"new");
            let current = probe_with(path, &host);
            assert!(matches!(current, CcVerdict::Ready { .. }), "{current:?}");
            std::fs::rename(&receipt, &next).unwrap();
            std::fs::rename(&saved, &receipt).unwrap();
            let restored = std::fs::metadata(&receipt).unwrap();
            assert_eq!(
                (
                    restored.dev(),
                    restored.ino(),
                    restored.len(),
                    restored.modified().unwrap()
                ),
                (
                    before.dev(),
                    before.ino(),
                    before.len(),
                    before.modified().unwrap()
                ),
            );
            assert_eq!(std::fs::read(&receipt).unwrap(), b"old");
            let direct = probe_with(path, &host);
            let cached = probe_cached(path, &host, Some(&cache));
            assert!(
                matches!(changed, CcVerdict::Unproven { .. })
                    && matches!(&direct, CcVerdict::Broken { why, .. } if why == "old tools cannot link")
                    && matches!(&cached, CcVerdict::Broken { why, .. } if why == "old tools cannot link"),
                "{phase}: changed: {changed:?}; current: {current:?}; direct: {direct:?}; cached: {cached:?}"
            );
            assert!(!cache.file.exists(), "no failed or unstable fact was kept");
            let runs = || {
                std::fs::read_to_string(root.join("asked"))
                    .unwrap()
                    .lines()
                    .count()
            };
            assert_eq!(runs(), 8);

            std::fs::rename(&receipt, &saved).unwrap();
            std::fs::rename(&next, &receipt).unwrap();
            let ready = CcVerdict::Ready { driver: cc };
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(runs(), 10);
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(runs(), 10, "the stable success remains warm");
        }
    }

    /// A `Ready` verdict is kept under what it depends on: a probe with the same key spawns
    /// nothing (the compiler is never asked again), and a changed Command Line Tools receipt
    /// probes again, including replacement and in-place edits preserving the old timestamp.
    /// An unread or oversized receipt cannot supply a kept success.
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
        let fresh = |why: &str| {
            let before = runs();
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(runs(), before + 2, "{why}");
        };
        let modified = std::fs::metadata(&receipt).unwrap().modified().unwrap();
        let replacement = root.join("replacement-receipt");
        std::fs::write(&replacement, "2").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        std::fs::rename(&replacement, &receipt).unwrap();
        assert_eq!(
            std::fs::metadata(&receipt).unwrap().modified().unwrap(),
            modified
        );
        fresh("a replaced receipt kept its old timestamp and verdict");
        std::fs::write(&receipt, "3").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&receipt)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        fresh("an in-place receipt edit kept its old timestamp and verdict");
        std::fs::File::options()
            .write(true)
            .open(&receipt)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(86_400))
            .unwrap();
        fresh("a changed receipt left the old verdict standing");
        let before = runs();
        assert_eq!(probe_cached(path, &host, None), ready);
        assert_eq!(runs(), before + 2, "no cache: the compiler is asked");
        std::fs::remove_file(&receipt).unwrap();
        std::os::unix::fs::symlink("missing-receipt-target", &receipt).unwrap();
        for _ in 0..2 {
            fresh("an unread receipt cannot retain a success");
        }
        std::fs::remove_file(&receipt).unwrap();
        std::fs::write(&receipt, vec![0; 64 * 1024 + 1]).unwrap();
        for _ in 0..2 {
            fresh("an oversized receipt cannot retain a success");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inherited_compiler_configuration_cannot_retain_a_cached_fact() {
        const CHILD: &str = "ATERM_CC_CACHE_ENVIRONMENT_CHILD";
        if let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) {
            let bin = root.join("bin");
            let cache = CcCache {
                file: root.join("state/cc"),
                receipt: root.join("no-receipt"),
            };
            let mut host = plain_host(&root);
            host.inherit_environment = true;
            let verdict = probe_cached(Some(bin.as_os_str()), &host, Some(&cache));
            let result = match verdict {
                CcVerdict::Ready { .. } => "ready",
                CcVerdict::Broken { why, .. } if why == "project headers refused" => "refused",
                other => panic!("unexpected compiler verdict: {other:?}"),
            };
            std::fs::write(root.join("result"), result).unwrap();
            return;
        }
        let root = scratch("changed-compiler-environment");
        let bin = root.join("bin");
        let asked = root.join("asked");
        let headers = root.join("headers");
        std::fs::create_dir(&headers).unwrap();
        script(
            &bin.join("cc"),
            &format!(
                "echo asked >> '{}'\n[ \"$1\" = --version ] && {{ echo cc; exit 0; }}\nif [ \"$(/bin/cat \"$CPATH/fixture.h\")\" = reject ]; then echo 'project headers refused' >&2; exit 1; fi\nprintf x > \"$2\"\n/bin/chmod +x \"$2\"",
                asked.display()
            ),
        );
        for (header, expected, calls) in [
            ("accept", "ready", 2),
            ("reject", "refused", 4),
            ("accept", "ready", 6),
        ] {
            std::fs::write(headers.join("fixture.h"), header).unwrap();
            let result = root.join("result");
            let _ = std::fs::remove_file(&result);
            let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "prereq::tests::inherited_compiler_configuration_cannot_retain_a_cached_fact",
                "--nocapture",
            ])
            .env_clear()
            .env(CHILD, root.as_path())
            .env("CPATH", &headers);
            let output = aterm_uds::deadline::output_until(
                &mut cmd,
                std::time::Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(std::fs::read_to_string(&result).unwrap(), expected);
            assert_eq!(
                std::fs::read_to_string(&asked).unwrap().lines().count(),
                calls
            );
        }
        assert!(
            !root.join("state/cc").exists(),
            "inherited configuration retains no fact"
        );
    }

    #[test]
    fn a_system_probe_does_not_inherit_a_projects_compiler_environment() {
        const CHILD: &str = "ATERM_CC_ENVIRONMENT_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let witness = aterm_tempfile::tempdir().unwrap();
            let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "prereq::tests::a_system_probe_does_not_inherit_a_projects_compiler_environment",
                "--nocapture",
            ])
            .env(CHILD, witness.path())
            .env("CPATH", "project header configuration");
            let output = aterm_uds::deadline::output_until(
                &mut cmd,
                std::time::Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
            assert!(
                output.status.success() && witness.path().join("checked").is_file(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let root = scratch("compiler-environment");
        let bin = root.join("bin");
        script(
            &bin.join("cc"),
            "[ \"$1\" = --version ] && { echo cc; exit 0; }\nif [ -n \"$CPATH\" ]; then echo 'project headers refused' >&2; exit 1; fi\nprintf x > \"$2\"\n/bin/chmod +x \"$2\"",
        );
        let mut host = plain_host(&root);
        host.inherit_environment = true;
        let cache = CcCache {
            file: root.join("state/cc"),
            receipt: root.join("no-receipt"),
        };
        let path = Some(bin.as_os_str());
        let refused = |verdict| matches!(verdict, CcVerdict::Broken { why, .. } if why == "project headers refused");
        assert!(refused(probe_cached(path, &host, Some(&cache))));
        host.inherit_environment = false;
        assert_eq!(
            probe_cached(path, &host, Some(&cache)),
            CcVerdict::Ready {
                driver: bin.join("cc")
            }
        );
        host.inherit_environment = true;
        assert!(
            refused(probe_cached(path, &host, Some(&cache))),
            "a system-only fact must not stand for the project's build"
        );
        std::fs::write(
            PathBuf::from(std::env::var_os(CHILD).unwrap()).join("checked"),
            b"yes",
        )
        .unwrap();
    }

    #[test]
    fn distinct_native_paths_cannot_reuse_a_cached_compiler_success() {
        use std::os::unix::ffi::OsStringExt as _;

        let root = scratch("native-path-cache");
        let bin = root.join("bin");
        let asked = root.join("asked");
        script(
            &bin.join("cc"),
            &format!(
                "echo asked >> '{}'\ncase \"$1\" in --version) echo cc;; -o) case \"$PATH\" in *$(printf '\\201')) echo 'PATH configuration refused' >&2; exit 1;; esac; printf x > \"$2\"; /bin/chmod +x \"$2\";; esac",
                asked.display()
            ),
        );
        let host = CcHost {
            inherit_environment: false,
            ..plain_host(&root)
        };
        let cache = CcCache {
            file: root.join("state/cc"),
            receipt: root.join("no-receipt"),
        };
        let path = |byte| {
            let mut bytes = bin.as_os_str().as_encoded_bytes().to_vec();
            bytes.extend_from_slice(b":/not-a-directory-");
            bytes.push(byte);
            std::ffi::OsString::from_vec(bytes)
        };
        let first = path(0x80);
        let changed = path(0x81);
        assert_eq!(first.to_string_lossy(), changed.to_string_lossy());
        let ready = CcVerdict::Ready {
            driver: bin.join("cc"),
        };
        for _ in 0..2 {
            assert_eq!(probe_cached(Some(&first), &host, Some(&cache)), ready);
        }
        assert_eq!(std::fs::read_to_string(&asked).unwrap().lines().count(), 2);
        assert!(matches!(
            probe_with(Some(&changed), &host),
            CcVerdict::Broken { why, .. } if why == "PATH configuration refused"
        ));
        assert!(matches!(
            probe_cached(Some(&changed), &host, Some(&cache)),
            CcVerdict::Broken { why, .. } if why == "PATH configuration refused"
        ));
        assert_eq!(probe_cached(Some(&first), &host, Some(&cache)), ready);
        assert_eq!(std::fs::read_to_string(&asked).unwrap().lines().count(), 6);
    }

    #[test]
    fn a_changed_deployment_target_cannot_reuse_a_cached_success() {
        let root = scratch("deployment-target");
        let bin = root.join("bin");
        let asked = root.join("asked");
        script(
            &bin.join("cc"),
            &format!(
                "echo asked >> '{}'\ncase \"$1\" in\n--version) echo cc;;\n-o) if [ \"$MACOSX_DEPLOYMENT_TARGET\" = unavailable ]; then echo 'build target unavailable' >&2; exit 1; fi; printf x > \"$2\"; /bin/chmod +x \"$2\";;\nesac",
                asked.display()
            ),
        );
        let mut host = plain_host(&root);
        let cache = CcCache {
            file: root.join("state/cc"),
            receipt: root.join("no-receipt"),
        };
        let path = Some(bin.as_os_str());
        let runs = || std::fs::read_to_string(&asked).unwrap().lines().count();
        let ready = CcVerdict::Ready {
            driver: bin.join("cc"),
        };
        assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
        assert_eq!(runs(), 2);
        host.deployment_target_override = Some("unavailable".into());
        assert!(matches!(
            probe_cached(path, &host, Some(&cache)),
            CcVerdict::Broken { why, .. } if why == "build target unavailable"
        ));
        assert_eq!(
            runs(),
            4,
            "a changed build target must ask the compiler again"
        );
        host.deployment_target_override = Some("11.0".into());
        assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
        assert_eq!(runs(), 6);
        assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
        assert_eq!(runs(), 6, "an unchanged target retains its successful fact");
    }

    #[test]
    fn a_changed_explicit_sdk_or_developer_directory_cannot_reuse_a_cached_success() {
        for select_developer in [false, true] {
            let root = scratch("mutable-sdk");
            let bin = root.join("bin");
            let developer = root.join("external developer");
            let sdk = developer.join("SDKs/MacOSX.sdk");
            std::fs::create_dir_all(&sdk).unwrap();
            let usable = sdk.join("usable");
            std::fs::write(&usable, "headers and libraries").unwrap();
            script(
                &bin.join("cc"),
                "sdk=${SDKROOT:-$DEVELOPER_DIR/SDKs/MacOSX.sdk}\ncase \"$1\" in --version) echo cc;; -o) if [ -f \"$sdk/usable\" ]; then printf x > \"$2\"; /bin/chmod +x \"$2\"; else echo 'SDK no longer usable' >&2; exit 1; fi;; esac",
            );
            let host = CcHost {
                sdkroot_override: (!select_developer).then_some(sdk),
                developer_dir_override: select_developer.then_some(developer),
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
    }

    #[test]
    fn verdicts_kept_before_the_current_output_policy_are_not_reused() {
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
        let developer = host.developer_dir().unwrap();
        let clang = clang_paths(&developer)
            .iter()
            .map(|path| crate::probe_cache::identity(path).unwrap_or_else(|| "absent".to_owned()))
            .collect::<Vec<_>>()
            .join(" ");
        let old_parts = [
            Some("plain".to_owned()),
            crate::probe_cache::identity(&cc),
            Some(developer.display().to_string()),
            Some(clang),
            Some("no receipt".to_owned()),
            Some("no sdk".to_owned()),
            Some(bin.display().to_string()),
        ];
        let mut old_keys: Vec<_> = [None, Some("executable-link-v1")]
            .into_iter()
            .map(|domain| {
                crate::probe_cache::key(
                    domain
                        .into_iter()
                        .map(|domain| Some(domain.to_owned()))
                        .chain(old_parts.iter().cloned()),
                )
                .unwrap()
            })
            .collect();
        // No fact from the preceding query-output policy may retain an old driver spelling.
        let current = cc_key(
            &Located::Plain(cc.clone()),
            Some(bin.as_os_str()),
            &host,
            &cache,
        )
        .unwrap();
        old_keys.push(current.replacen(
            "fresh-executable-link-v13",
            "fresh-executable-link-v12",
            1,
        ));
        // The old receipt reader also called an unread receipt absent. Retire that fact
        // even if the receipt really is absent now and all other inputs still match.
        old_keys.push(current.replacen("receipt-v1 absent", "no receipt", 1));
        for (epoch, old_key) in old_keys.into_iter().enumerate() {
            crate::probe_cache::put(&cache.file, &old_key, &cc.to_string_lossy());
            for calls in [2, 4] {
                assert!(matches!(
                    probe_cached(Some(bin.as_os_str()), &host, Some(&cache)),
                    CcVerdict::Broken { .. }
                ));
                assert_eq!(
                    std::fs::read_to_string(&asked).unwrap().lines().count(),
                    epoch * 4 + calls
                );
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_developer_override_controls_discovery_and_always_probes_the_compiler() {
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
            assert_eq!(calls.lines().count(), (index + 1) * 2);
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

    #[test]
    fn a_changed_discovery_program_retires_a_cached_compiler_success() {
        for fault in ["permissions", "missing", "loop", "replacement"] {
            let root = scratch("discovery-program-cache");
            let bin = root.join("bin");
            let xcrun = bin.join("xcrun");
            let developer = root.join("developer");
            let driver = developer.join("usr/bin/cc");
            let compiled = root.join("compiled");
            script(&developer.join("usr/bin/clang"), "exit 0");
            script(&xcrun, &format!("echo '{}'", driver.display()));
            script(
                &bin.join("cc"),
                &format!(
                    "echo build >> '{}'\nprintf x > \"$2\"\n/bin/chmod +x \"$2\"",
                    compiled.display()
                ),
            );
            let host = CcHost {
                shim_dir: Some(bin.clone()),
                default_developer_dir: developer,
                ..plain_host(&root)
            };
            let cache = CcCache {
                file: root.join("state/cc"),
                receipt: root.join("no-receipt"),
            };
            let path = Some(bin.as_os_str());
            let ready = CcVerdict::Ready {
                driver: driver.clone(),
            };
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(
                std::fs::read_to_string(&compiled).unwrap().lines().count(),
                1
            );
            match fault {
                "permissions" => {
                    std::fs::set_permissions(&xcrun, std::fs::Permissions::from_mode(0o600))
                        .unwrap()
                }
                "missing" => std::fs::remove_file(&xcrun).unwrap(),
                "loop" => {
                    std::fs::remove_file(&xcrun).unwrap();
                    std::os::unix::fs::symlink("xcrun", &xcrun).unwrap();
                }
                "replacement" => script(&xcrun, "echo 'discovery refused' >&2; exit 7"),
                _ => unreachable!(),
            }
            let fresh = probe_with(path, &host);
            assert!(matches!(
                fresh,
                CcVerdict::Unproven { .. } | CcVerdict::Broken { .. }
            ));
            assert_eq!(probe_cached(path, &host, Some(&cache)), fresh, "{fault}");
            assert_eq!(
                std::fs::read_to_string(&compiled).unwrap().lines().count(),
                1
            );
            if fault != "missing" {
                std::fs::remove_file(&xcrun).unwrap();
            }
            script(
                &xcrun,
                &format!("# repaired discovery\necho '{}'", driver.display()),
            );
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(probe_cached(path, &host, Some(&cache)), ready);
            assert_eq!(
                std::fs::read_to_string(&compiled).unwrap().lines().count(),
                2
            );
        }
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
