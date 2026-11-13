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
    /// Inherit project compiler configuration and retain the caller's directory for compilation.
    /// These mutable inputs cannot retain a cached success. Machine setup clears the environment
    /// and compiles in the private probe directory with its explicit probe inputs and temporary
    /// directory instead.
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
        let dir = self.selected_developer_dir()?;
        aterm_types::clt_shim::developer_content_dir(&dir).map_err(|error| {
            format!(
                "could not resolve developer directory {}: {error}",
                dir.display()
            )
        })
    }

    /// The override or system selection before resolving an Xcode application root.
    fn selected_developer_dir(&self) -> Result<PathBuf, String> {
        if let Some(dir) = self.developer_override() {
            return Ok(dir.to_path_buf());
        }
        match aterm_types::clt_shim::read_developer_selection(&self.xcode_select_link) {
            Ok(dir) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not read {}: {error}",
                    self.xcode_select_link.display()
                ));
            }
        }
        if let Some(dir) = &self.xcode_developer_dir
            && holds_clang(dir, self)?
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
/// key and discovered-compiler identity match answers `Ready` with nothing spawned (no `xcrun`,
/// no compile and link); any other verdict is probed every time. Revalidate the key before returning a
/// verdict or publishing a cached fact; an observed change leaves the current toolchain unproven.
/// The discovered compiler's metadata identity is captured before compilation, revalidated
/// afterward, and kept beside its path for each hit to verify. A driver whose executable
/// identity is unreadable, or whose native path the text cache cannot represent, is not cached.
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
        && let Some(value) = crate::probe_cache::get(&cache.file, key)
        && let Ok((driver, identity)) = aterm_json::from_str::<(String, String)>(&value)
        && crate::probe_cache::identity(Path::new(&driver)).as_ref() == Some(&identity)
    {
        let driver = PathBuf::from(driver);
        if cc_key(&located, path_var, host, cache).as_ref() != Some(key)
            || crate::probe_cache::identity(&driver).as_ref() != Some(&identity)
        {
            return CcVerdict::Unproven {
                driver,
                why: CHANGED.into(),
            };
        }
        return CcVerdict::Ready { driver };
    }
    let mut observed = None;
    let verdict = probe_located_observing(&located, path_var, host, |driver| {
        if kept.is_some() && matches!(is_executable(driver), Ok(true)) {
            observed = crate::probe_cache::identity(driver).map(|id| (driver.to_owned(), id));
        }
    });
    if let Some((cache, key)) = &kept {
        if cc_key(&located, path_var, host, cache).as_ref() != Some(key) {
            return CcVerdict::Unproven {
                driver: match &located {
                    Located::Plain(driver) | Located::Shim(driver) => driver.clone(),
                },
                why: CHANGED.into(),
            };
        }
        if let Some((driver, identity)) = &observed
            && crate::probe_cache::identity(driver).as_ref() != Some(identity)
        {
            return CcVerdict::Unproven {
                driver: driver.clone(),
                why: CHANGED.into(),
            };
        }
        if let CcVerdict::Ready { driver } = &verdict
            && let Some((_, identity)) = &observed
            && let Some(driver) = driver.to_str()
            && let Ok(value) = aterm_json::to_string(&(driver, identity))
        {
            crate::probe_cache::put(&cache.file, key, &value);
        }
    }
    verdict
}

/// The base key for a `Ready` verdict for `located`, read without spawning: the compiler as it is
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
        Located::Shim(cc) => crate::probe_cache::identity(&discovery_program(cc, host))?,
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
        Some("fresh-executable-link-v15".to_owned()),
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
    probe_located_observing(located, path_var, host, |_| {})
}

/// Observe the discovered driver's identity before compilation can use or replace it.
fn probe_located_observing(
    located: &Located,
    path_var: Option<&OsStr>,
    host: &CcHost,
    before_build: impl FnOnce(&Path),
) -> CcVerdict {
    let (cc, bin, query) = match located {
        Located::Plain(cc) => (cc, cc.clone(), CcQuery::Version),
        Located::Shim(cc) => (cc, discovery_program(cc, host), CcQuery::Find),
    };
    match ask(&bin, cc, query, host) {
        CcVerdict::Ready { driver } => {
            before_build(&driver);
            build_probe(cc, driver, path_var, host)
        }
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

/// Literal matches remain meaningful when an entry is absent or unreadable; readable aliases
/// compare by resolved path without rewriting the selected invocation path.
fn same_path(selected: &Path, known: &Path) -> bool {
    selected == known
        || matches!(
            (std::fs::canonicalize(selected), std::fs::canonicalize(known)),
            (Ok(selected), Ok(known)) if selected == known
        )
}

fn is_system_shim(cc: &Path, host: &CcHost) -> bool {
    host.shim_dir.as_deref().is_some_and(|shim| {
        cc.parent().is_some_and(|dir| same_path(dir, shim))
            || same_path(cc, &shim.join("cc"))
            || std::fs::canonicalize(cc)
                .ok()
                .and_then(|path| path.parent().map(Path::to_owned))
                .is_some_and(|dir| same_path(&dir, shim))
    })
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
                .any(|shim| same_path(dir, shim))
        })
    {
        let candidate = dir.join("cc");
        match is_executable(&candidate) {
            Ok(true) => {
                if setup_shims
                    .iter()
                    .flatten()
                    .any(|shim| same_path(&candidate, &shim.join("cc")))
                {
                    continue;
                }
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
    if !is_system_shim(&cc, host) {
        return Ok(Located::Plain(cc));
    }
    let unread = |why| CcVerdict::Unproven {
        driver: cc.clone(),
        why,
    };
    let developer_dir = host.developer_dir().map_err(unread)?;
    if holds_clang(&developer_dir, host).map_err(unread)? {
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

fn holds_clang(developer: &Path, host: &CcHost) -> Result<bool, String> {
    for clang in clang_paths(developer) {
        if is_executable(&clang)? && !is_system_shim(&clang, host) {
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

/// Keep source and output in the caller's fresh private directory; a retry creates its own
/// exclusive output directory instead of depending on removal of a failed attempt's artifact.
/// Project probes retain their caller's working directory so relative configuration resolves
/// as it would in a build. Machine probes run in the private directory.
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
        cmd.arg("-o").arg(program).arg(&source);
        if !host.inherit_environment {
            cmd.current_dir(dir);
        }
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
    if matches!(&result, Ok(out) if out.status.code().is_some() && !out.status.success())
        && host.sdkroot().is_none()
        && is_system_shim(cc, host)
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
                Ok(metadata) => match is_executable_file(&program, &metadata) {
                    Ok(wrote) => wrote,
                    Err(why) => {
                        return CcVerdict::Unproven {
                            driver: resolved,
                            why,
                        };
                    }
                },
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
        Ok(out) => compiler_failure(cc, &out),
    }
}

/// A nonzero exit is a compiler refusal; termination without an exit code proves no
/// toolchain defect (for example, another process may have killed the probe).
fn compiler_failure(driver: &Path, out: &std::process::Output) -> CcVerdict {
    if out.status.code().is_none() {
        CcVerdict::Unproven {
            driver: driver.to_path_buf(),
            why: format!(
                "compiler process ended without an exit code: {}",
                out.status
            ),
        }
    } else {
        CcVerdict::Broken {
            driver: driver.to_path_buf(),
            why: first_stderr_line(out),
        }
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

/// A compiler symlink outside the shim directory does not bring an adjacent `xcrun` with it.
fn discovery_program(cc: &Path, host: &CcHost) -> PathBuf {
    host.shim_dir
        .as_deref()
        .or_else(|| cc.parent())
        .unwrap_or(Path::new("/usr/bin"))
        .join("xcrun")
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
/// requires one absolute native path. A captured nonzero exit is Broken with the first stderr
/// line; a process terminated without an exit code leaves tool state unproven.
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
        Some(out) => compiler_failure(driver, &out),
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
        Ok(metadata) => is_executable_file(path, &metadata),
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

fn is_executable_file(path: &Path, meta: &std::fs::Metadata) -> Result<bool, String> {
    if !meta.is_file() || meta.len() == 0 {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o111 == 0 {
            return Ok(false);
        }
        crate::platform::executable_for_current_user(path)
            .ok_or_else(|| format!("could not verify execute access to {}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(true)
    }
}

#[cfg(all(test, unix))]
#[path = "prereq_tests.rs"]
mod tests;
