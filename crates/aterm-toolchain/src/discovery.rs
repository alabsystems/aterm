// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trust toolchain discovery (std-only) shared by `aterm-verify`, `atpkg`, and `aterm-release`.
//!
//! Resolution order:
//! 1. `$TRUST_STAGE2_BIN` (explicit override, never fallen back from);
//! 2. `<rustup_home>/toolchains/<channel>/bin` (resolved through atpkg's store view if applicable);
//! 3. `<atpkg_prefix>/store/trust/current/bin`;
//! 4. `PATH`.
//!
//! A rustup `trust` toolchain older than the atpkg store build ranks below the store ([`Demoted`]).
//! Every candidate is canonicalized and verified against `rust-toolchain.toml`'s pinned channel.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use crate::rustup::rustup_home;

/// Whether `path` is a runnable file: not a directory, with an execute bit.
#[cfg(unix)]
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| !m.is_dir() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// True if `path` is an `atpkg` pending-install stub script rather than a real binary.
fn is_pending_stub(path: &Path) -> bool {
    use std::io::Read as _;
    const HEAD: u64 = 256;
    const MARKER: &str = "atpkg pending-program stub";
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take(HEAD).read_to_end(&mut head).is_err() {
        return false;
    }
    String::from_utf8_lossy(&head).contains(MARKER)
}

/// Whether `path` exists and is not a directory (Windows has no execute permission bit).
#[cfg(not(unix))]
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| !m.is_dir())
        .unwrap_or(false)
}

/// Tool filename with the platform executable suffix (`<name>` on Unix, `<name>.exe` on Windows).
#[must_use]
pub fn tool_file(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Extract the `channel = "..."` value from `rust-toolchain.toml` text.
#[must_use]
pub fn pinned_channel_in(toml: &str) -> Option<String> {
    toml.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let rest = l.strip_prefix("channel")?.trim_start();
            let rest = rest.strip_prefix('=')?.trim_start();
            let rest = rest.strip_prefix('"')?;
            let end = rest.find('"')?;
            Some(rest[..end].to_string())
        })
}

/// [`pinned_channel_in`] over `<root>/rust-toolchain.toml`.
#[must_use]
pub fn pinned_channel(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join("rust-toolchain.toml"))
        .ok()
        .as_deref()
        .and_then(pinned_channel_in)
}

/// True for stock upstream channels (`stable`, `beta`, `nightly*`, `1.*`).
fn is_upstream_channel(channel: &str) -> bool {
    matches!(channel, "" | "stable" | "beta" | "nightly")
        || channel.starts_with("nightly-")
        || channel.starts_with("1.")
}

/// Check whether `dir` satisfies the `pinned` toolchain channel by verifying its
/// branded driver (`<channel>c`) reports a valid sysroot with `lib/rustlib`.
fn is_pinned_toolchain(dir: &Path, pinned: Option<&str>) -> bool {
    let channel = match pinned {
        None => return true,
        Some(channel) if is_upstream_channel(channel) => return true,
        Some(channel) => channel,
    };
    pinned_sysroot(dir, channel).is_some()
}

/// Run `<channel>c --print sysroot` in `dir` and return the sysroot if `lib/rustlib` exists.
fn pinned_sysroot(dir: &Path, channel: &str) -> Option<PathBuf> {
    let driver = [format!("{channel}c"), channel.to_string()]
        .into_iter()
        .map(|n| dir.join(tool_file(&n)))
        .find(|p| is_executable_file(p))?;
    #[expect(
        clippy::disallowed_types,
        reason = "a std-only probe: this crate takes no dependency, so it cannot reach aterm-uds"
    )]
    let Ok(out) = std::process::Command::new(&driver)
        .arg("--print")
        .arg("sysroot")
        .output()
    else {
        return None;
    };
    if !out.status.success() {
        return None;
    }
    let Ok(sysroot) = String::from_utf8(out.stdout) else {
        return None;
    };
    let sysroot = sysroot.trim();
    (!sysroot.is_empty() && Path::new(sysroot).join("lib/rustlib").is_dir())
        .then(|| PathBuf::from(sysroot))
}

/// Resolve the `atpkg` install prefix from `[packages].prefix` in `aterm.toml`,
/// falling back to [`default_atpkg_prefix`].
#[must_use]
pub fn atpkg_prefix(home: &Path, xdg_config_home: Option<&Path>) -> PathBuf {
    let cfg = xdg_config_home
        .filter(|d| !d.as_os_str().is_empty())
        .map_or_else(|| home.join(".config"), Path::to_path_buf)
        .join("aterm/aterm.toml");
    if let Ok(text) = std::fs::read_to_string(&cfg)
        && let Some(configured) = configured_prefix_in(&text, home)
    {
        return configured;
    }
    default_atpkg_prefix(home)
}

/// Platform default `atpkg` install prefix under `home`.
#[must_use]
pub fn default_atpkg_prefix(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/aterm/pkg")
    } else if cfg!(windows) {
        home.join("AppData").join("Local").join("aterm").join("pkg")
    } else {
        home.join(".local/share/aterm/pkg")
    }
}

/// Parse `[packages].prefix` from `aterm.toml` text, expanding `~` against `home`.
#[must_use]
pub fn configured_prefix_in(toml: &str, home: &Path) -> Option<PathBuf> {
    let mut in_packages = false;
    for line in toml.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_packages = line == "[packages]";
            continue;
        }
        if !in_packages {
            continue;
        }
        let Some(rest) = line.strip_prefix("prefix") else {
            continue;
        };
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        let value = rest[..rest.find('"')?].trim();
        return match value {
            "" => None,
            "~" => Some(home.to_path_buf()),
            v if v.starts_with("~/") => Some(home.join(&v[2..])),
            v if Path::new(v).is_absolute() => Some(PathBuf::from(v)),
            _ => None,
        };
    }
    None
}

/// `<prefix>/store/trust/current/bin`.
#[must_use]
pub fn store_stage2_bin(prefix: &Path) -> PathBuf {
    prefix
        .join(crate::layout::STORE_DIR)
        .join(STORE_CHANNEL)
        .join("current/bin")
}

/// Channel name managed in the `atpkg` store.
const STORE_CHANNEL: &str = "trust";

/// Timeout for a single `-vV` compiler commit-date probe.
const DATE_PROBE_BOUND: Duration = Duration::from_secs(5);

/// A rustup `trust` toolchain ranked below the `atpkg` store because its `commit-date`
/// is older than the store build's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Demoted {
    /// The rustup entry's physical `bin` directory.
    pub dir: PathBuf,
    /// Its compiler's `commit-date`.
    pub its: String,
    /// The store build's `commit-date`.
    pub store: String,
    /// True when the rustup toolchain entry is a symlink (`aterm pkg repair` can re-point it).
    pub linked: bool,
}

/// Remedy when the demoted rustup entry is a symlink.
const REMEDY_LINK: &str = "`aterm pkg repair` re-points it at the store";

macro_rules! detach_fix {
    () => {
        "remove that entry yourself (e.g. `rustup toolchain uninstall trust`), then \
         `aterm pkg repair`"
    };
}

/// Remediation message when a non-symlink toolchain directory blocks `atpkg` attach.
pub const DETACH_FIX: &str = detach_fix!();

/// Remediation message when a demoted rustup entry is a real directory.
pub const REMEDY_DIRECTORY: &str = concat!(
    "`aterm pkg repair` will not replace a directory it did not lay: ",
    detach_fix!()
);

impl Demoted {
    /// Remediation command for this demoted entry based on whether it is a symlink.
    #[must_use]
    pub const fn remedy(&self) -> &'static str {
        if self.linked {
            REMEDY_LINK
        } else {
            REMEDY_DIRECTORY
        }
    }

    /// Human-readable diagnostic explaining why this rustup entry was demoted.
    #[must_use]
    pub fn sentence(&self) -> String {
        format!(
            "rustup `trust` ({}) is a toolchain from {}, older than the atpkg store's {}: \
             ranked below the store, not used — {}",
            self.dir.display(),
            self.its,
            self.store,
            self.remedy()
        )
    }
}

/// Read `commit-date: YYYY-MM-DD` from `trustc -vV` (or `rustc -vV`) in `bin`.
fn commit_date(bin: &Path) -> Option<String> {
    ["trustc", "rustc"]
        .iter()
        .map(|n| bin.join(tool_file(n)))
        .filter(|p| is_executable_file(p))
        .find_map(|driver| {
            let text = bounded_stdout(&driver, &["-vV"], DATE_PROBE_BOUND)?;
            let date = text
                .lines()
                .find_map(|l| l.strip_prefix("commit-date: "))?
                .trim()
                .to_string();
            let shaped = date.len() == 10
                && date.bytes().enumerate().all(|(i, b)| {
                    if i == 4 || i == 7 {
                        b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }
                });
            shaped.then_some(date)
        })
}

/// Run `program` with `args` and return stdout if it exits 0 within `bound`.
fn bounded_stdout(program: &Path, args: &[&str], bound: Duration) -> Option<String> {
    use std::io::Read as _;
    use std::process::Stdio;
    #[expect(
        clippy::disallowed_types,
        reason = "a std-only probe: this crate takes no dependency, so it cannot reach aterm-uds"
    )]
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < bound => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    status.success().then_some(out)
}

/// Return `(rustup_date, store_date)` when `rustup` is older than `<store_prefix>/store/trust/current`.
fn older_than_store(rustup: &Path, store_prefix: &Path) -> Option<(String, String)> {
    if std::fs::symlink_metadata(store_prefix.join("links").join(STORE_CHANNEL)).is_ok() {
        return None;
    }
    let store = store_stage2_bin(store_prefix);
    std::fs::metadata(&store).ok()?;
    let its = commit_date(rustup)?;
    let theirs = commit_date(&store)?;
    (its < theirs).then_some((its, theirs))
}

/// Resolve an `atpkg` store view (`<store_prefix>/rustup/...`) to the underlying
/// dev-linked checkout `bin` or `<store_prefix>/store/trust/current/bin`.
fn view_presents(entry: &Path, store_prefix: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(entry).ok()?;
    let views = std::fs::canonicalize(store_prefix.join(crate::layout::VIEWS_DIR)).ok()?;
    if !resolved.starts_with(&views) {
        return None;
    }
    let store = || Some(store_stage2_bin(store_prefix));
    let Ok(marker) = std::fs::read_to_string(store_prefix.join("links").join(STORE_CHANNEL)) else {
        return store();
    };
    let Some(checkout) = marker.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix("checkout")?
            .trim_start()
            .strip_prefix('=')?;
        let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        (!value.contains(['"', '\\'])).then(|| PathBuf::from(value))
    }) else {
        return store();
    };
    let prefix = std::fs::canonicalize(store_prefix).unwrap_or_else(|_| store_prefix.into());
    let inside = std::fs::canonicalize(&checkout).is_ok_and(|c| c.starts_with(&prefix));
    if !inside && checkout.join("bin").is_dir() && checkout.join("lib").is_dir() {
        Some(checkout.join("bin"))
    } else {
        store()
    }
}

/// Resolved Trust toolchain paths and discovery diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    /// The resolved (physical) stage2 `bin` directory.
    pub stage2_dir: PathBuf,
    /// Path to the `targo` binary in `stage2_dir`.
    pub targo: PathBuf,
    /// Path to the `trustdoc` binary in `stage2_dir`.
    pub trustdoc: PathBuf,
    /// `targo-tippy` binary path, if found in the toolchain or borrowed from the managed build.
    pub tippy: Option<PathBuf>,
    /// `trustfmt` binary path, if found in the toolchain or borrowed from the managed build.
    pub trustfmt: Option<PathBuf>,
    /// Formatter/lint tools (`trustfmt`, `targo-tippy`) borrowed from the managed Trust build
    /// when absent from a non-override toolchain directory.
    pub borrowed: Vec<(&'static str, PathBuf)>,
    /// First candidate directory containing `targo` that failed the pinned-toolchain check.
    pub refused: Option<PathBuf>,
    /// `<prefix>/store/trust/current/bin` (`None` when `$TRUST_STAGE2_BIN` is set).
    pub store_bin: Option<PathBuf>,
    /// Rustup `trust` entry demoted below the store when another candidate won.
    pub demoted: Option<Demoted>,
}

impl Toolchain {
    /// Discover the toolchain using environment `XDG_CONFIG_HOME` and `RUSTUP_HOME`.
    #[must_use]
    pub fn discover(
        stage2_bin: Option<&Path>,
        home: &Path,
        path_env: &OsStr,
        pinned: Option<&str>,
    ) -> Self {
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let prefix = atpkg_prefix(home, xdg.as_deref());
        let rustup = rustup_home(std::env::var_os("RUSTUP_HOME").as_deref(), home);
        Self::discover_with_store(stage2_bin, &rustup, Some(&prefix), path_env, pinned)
    }

    /// Discover the toolchain across `stage2_bin`, `rustup_home`, `store_prefix`, and `path_env`.
    #[must_use]
    pub fn discover_with_store(
        stage2_bin: Option<&Path>,
        rustup_home: &Path,
        store_prefix: Option<&Path>,
        path_env: &OsStr,
        pinned: Option<&str>,
    ) -> Self {
        let physical = |dir: PathBuf| std::fs::canonicalize(&dir).unwrap_or(dir);
        let targo = tool_file("targo");
        let mut refused = None;
        let mut demoted = None;
        let (tool_dir, store_bin) = if let Some(explicit) = stage2_bin {
            let dir = physical(explicit.to_path_buf());
            if is_executable_file(&dir.join(&targo)) && !is_pinned_toolchain(&dir, pinned) {
                refused = Some(dir.clone());
            }
            (dir, None)
        } else {
            let entry = rustup_home
                .join("toolchains")
                .join(pinned.unwrap_or("trust"));
            let rustup = store_prefix
                .and_then(|prefix| view_presents(&entry, prefix))
                .unwrap_or_else(|| entry.join("bin"));
            let store_bin = store_prefix.map(store_stage2_bin);
            let stale = store_prefix
                .filter(|_| pinned.unwrap_or(STORE_CHANNEL) == STORE_CHANNEL)
                .filter(|_| is_executable_file(&rustup.join(&targo)))
                .and_then(|prefix| older_than_store(&rustup, prefix));
            let head: Vec<PathBuf> = match (&stale, &store_bin) {
                (Some(_), Some(store)) => vec![store.clone(), rustup.clone()],
                _ => std::iter::once(rustup.clone())
                    .chain(store_bin.clone())
                    .collect(),
            };
            let candidates = head.into_iter().chain(std::env::split_paths(path_env));
            let mut chosen = None;
            for dir in candidates {
                let driver = dir.join(&targo);
                if !is_executable_file(&driver) || is_pending_stub(&driver) {
                    continue;
                }
                let dir = physical(dir);
                if is_pinned_toolchain(&dir, pinned) {
                    chosen = Some(dir);
                    refused = None;
                    break;
                }
                refused.get_or_insert(dir);
            }
            let rustup_dir = physical(rustup.clone());
            let linked =
                std::fs::symlink_metadata(&entry).is_ok_and(|m| m.file_type().is_symlink());
            demoted = stale
                .filter(|_| chosen.as_ref().is_some_and(|c| *c != rustup_dir))
                .map(|(its, store)| Demoted {
                    dir: rustup_dir,
                    its,
                    store,
                    linked,
                });
            let reported = chosen.unwrap_or_else(|| store_bin.clone().unwrap_or(rustup));
            (reported, store_bin)
        };
        let explicit = stage2_bin.is_some();
        let mut borrowed = Vec::new();
        let mut tool = |name: &'static str| {
            let own = tool_dir.join(tool_file(name));
            if is_executable_file(&own) {
                return Some(own);
            }
            if explicit {
                return None;
            }
            let found = managed_tool(name, store_prefix, path_env, pinned)?;
            borrowed.push((name, found.clone()));
            Some(found)
        };
        let trustfmt = tool(TRUSTFMT);
        let tippy = tool(TIPPY);
        Self {
            targo: tool_dir.join(targo),
            trustdoc: tool_dir.join(tool_file("trustdoc")),
            tippy,
            trustfmt,
            borrowed,
            stage2_dir: tool_dir,
            refused,
            store_bin,
            demoted,
        }
    }

    /// Path from which `name` was borrowed, or `None` if native to `stage2_dir` or absent.
    #[must_use]
    pub fn borrowed(&self, name: &str) -> Option<&Path> {
        self.borrowed
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, p)| p.as_path())
    }

    /// Summary note naming tools borrowed from the managed Trust build, if any.
    #[must_use]
    pub fn borrowed_note(&self) -> Option<String> {
        if self.borrowed.is_empty() {
            return None;
        }
        let each: Vec<String> = self
            .borrowed
            .iter()
            .map(|(name, file)| format!("{name} from {}", file.display()))
            .collect();
        Some(format!(
            "{} — the managed Trust build's; {} has none",
            each.join(", "),
            self.stage2_dir.display()
        ))
    }

    /// True when an executable `targo` matching the pinned channel was found.
    #[must_use]
    pub fn have_targo(&self) -> bool {
        self.refused.is_none() && is_executable_file(&self.targo)
    }

    /// True when `trustdoc` is an executable file in `stage2_dir`.
    #[must_use]
    pub fn have_trustdoc(&self) -> bool {
        is_executable_file(&self.trustdoc)
    }

    /// Prepend `stage2_dir` to `inherited` `PATH` when [`Self::have_targo`] is true.
    #[must_use]
    pub fn path_with_stage2_first(&self, inherited: &OsStr) -> OsString {
        if !self.have_targo() {
            return inherited.to_os_string();
        }
        let mut p = OsString::from(self.stage2_dir.as_os_str());
        if !inherited.is_empty() {
            p.push(if cfg!(windows) { ";" } else { ":" });
            p.push(inherited);
        }
        p
    }

    /// Remediation instructions for installing or building the pinned Trust toolchain.
    pub const INSTALL_REMEDY: &'static str = "`aterm pkg install trust` (then `aterm pkg doctor` \
         to confirm the store); from source instead, in a Trust checkout: python3 \
         scripts/recreate_bootstrap.py --stage 2 --require-seed --fresh-seed --no-register \
         (stage0 = the pinned Trust seed; it refuses rather than wrap a stock rustc), then \
         seal it with scripts/promote-toolchain.sh";

    /// Remediation instructions when an explicit `TRUST_STAGE2_BIN` is invalid or incomplete.
    pub const OVERRIDE_REMEDY: &'static str =
        "unset TRUST_STAGE2_BIN, or point it at a sealed toolchain's bin";

    /// Select [`Self::OVERRIDE_REMEDY`] or [`Self::INSTALL_REMEDY`].
    const fn remedy(&self) -> &'static str {
        if self.store_bin.is_none() {
            Self::OVERRIDE_REMEDY
        } else {
            Self::INSTALL_REMEDY
        }
    }

    /// The diagnostic for no usable toolchain: none found, or one found and refused.
    #[must_use]
    pub fn missing_targo_label(&self) -> String {
        let explicit = self.store_bin.is_none();
        match (&self.refused, &self.store_bin) {
            (Some(dir), _) if explicit => format!(
                "TRUST_STAGE2_BIN={} is NOT the toolchain rust-toolchain.toml pins (no trustc \
                 there answers with a sysroot). Fix: {}",
                dir.display(),
                Self::OVERRIDE_REMEDY,
            ),
            (Some(dir), _) => format!(
                "targo at {} is NOT the toolchain rust-toolchain.toml pins (no trustc beside it \
                 answers with a sysroot). Fix: {}",
                dir.display(),
                Self::INSTALL_REMEDY,
            ),
            (None, Some(store)) => format!(
                "targo not found — looked in the rustup `trust` toolchain, the atpkg store at \
                 {}, and PATH. Fix: {}; or point TRUST_STAGE2_BIN at a stage2 bin",
                store.display(),
                Self::INSTALL_REMEDY,
            ),
            (None, None) => format!(
                "TRUST_STAGE2_BIN={} holds no targo. Fix: {}",
                self.stage2_dir.display(),
                Self::OVERRIDE_REMEDY,
            ),
        }
    }

    /// Diagnostic message when `trustdoc` is unavailable.
    #[must_use]
    pub fn missing_trustdoc_label(&self) -> String {
        format!(
            "no doc driver: {} is not an executable file and no trustdoc is on PATH, so no doctest ran. Fix: {}",
            self.trustdoc.display(),
            self.remedy(),
        )
    }

    /// Skip reason when `targo-tippy` is unavailable.
    #[must_use]
    pub fn missing_tippy_label(&self) -> String {
        format!("tippy lint ({})", self.missing_tool(TIPPY))
    }

    /// Skip reason when `trustfmt` is unavailable.
    #[must_use]
    pub fn missing_trustfmt_label(&self) -> String {
        format!("{TRUSTFMT} ({})", self.missing_tool(TRUSTFMT))
    }

    /// Format the missing-tool diagnostic for `name`.
    fn missing_tool(&self, name: &str) -> String {
        if self.store_bin.is_none() {
            format!(
                "no {name} in TRUST_STAGE2_BIN={}, which is never fallen back from — fix: {}",
                self.stage2_dir.display(),
                Self::OVERRIDE_REMEDY,
            )
        } else {
            format!(
                "no {name} in {}, the atpkg store or PATH — fix: {}",
                self.stage2_dir.display(),
                Self::INSTALL_REMEDY,
            )
        }
    }
}

/// The Trust formatter's file name.
pub const TRUSTFMT: &str = "trustfmt";

/// The Trust lint driver's file name.
pub const TIPPY: &str = "targo-tippy";

/// Locate `name` (`trustfmt` or `targo-tippy`) in the `atpkg` store or `path_env`
/// belonging to a validated `pinned` toolchain sysroot.
fn managed_tool(
    name: &str,
    store_prefix: Option<&Path>,
    path_env: &OsStr,
    pinned: Option<&str>,
) -> Option<PathBuf> {
    store_prefix
        .map(store_stage2_bin)
        .into_iter()
        .chain(std::env::split_paths(path_env))
        .find_map(|dir| {
            let tool = dir.join(tool_file(name));
            if !is_executable_file(&tool) || is_pending_stub(&tool) {
                return None;
            }
            let tool = std::fs::canonicalize(&tool).unwrap_or(tool);
            let Some(channel) = pinned.filter(|channel| !is_upstream_channel(channel)) else {
                return Some(tool);
            };
            let sysroot = pinned_sysroot(tool.parent()?, channel)?;
            let physical = std::fs::canonicalize(sysroot.join("bin").join(tool_file(name))).ok()?;
            (is_executable_file(&physical) && !is_pending_stub(&physical)).then_some(physical)
        })
}

#[cfg(test)]
#[allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a temporary directory that the caller removes.
    fn mktemp_dir(tag: &str) -> std::io::Result<PathBuf> {
        Ok(aterm_tempfile::Builder::new()
            .prefix(&format!("{tag}."))
            .tempdir()?
            .keep())
    }

    /// Append the platform executable suffix to `path`.
    fn runnable(path: &Path) -> PathBuf {
        let mut named = path.as_os_str().to_os_string();
        named.push(std::env::consts::EXE_SUFFIX);
        PathBuf::from(named)
    }

    /// Mark `path` executable on Unix.
    fn make_runnable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
    }

    /// Write a stub executable that exits 0 without printing a sysroot.
    fn exec_stub(path: &Path) {
        let path = &runnable(path);
        fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write");
        make_runnable(path);
    }

    /// Write a stub driver that prints `<bin>/..` for `--print sysroot` and creates `lib/rustlib`.
    fn driver_stub(path: &Path) {
        let path = &runnable(path);
        let sysroot = path.parent().expect("bin dir").parent().expect("sysroot");
        fs::create_dir_all(sysroot.join("lib/rustlib")).expect("mkdir rustlib");
        #[cfg(windows)]
        fs::copy(std::env::current_exe().expect("this test binary"), path).expect("copy");
        #[cfg(not(windows))]
        fs::write(
            path,
            format!("#!/bin/sh\necho '{}'\n", sysroot.display()).as_bytes(),
        )
        .expect("write");
        make_runnable(path);
    }

    /// CRT initializer so a copied test binary on Windows answers `--print sysroot` before libtest.
    #[cfg(windows)]
    #[used]
    #[unsafe(link_section = ".CRT$XCU")]
    static ANSWER_AS_A_DRIVER: extern "C" fn() = answer_as_a_driver;

    #[cfg(windows)]
    extern "C" fn answer_as_a_driver() {
        use std::io::Write as _;
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        if args != [OsStr::new("--print"), OsStr::new("sysroot")] {
            return;
        }
        let Ok(me) = std::env::current_exe() else {
            std::process::exit(1);
        };
        let Some(sysroot) = me.parent().and_then(Path::parent) else {
            std::process::exit(1);
        };
        let mut out = std::io::stdout();
        let wrote = writeln!(out, "{}", sysroot.display()).and_then(|()| out.flush());
        std::process::exit(i32::from(wrote.is_err()));
    }

    #[test]
    fn the_reported_location_when_nothing_resolves_is_the_store() {
        // When no candidate exists, report the store (or rustup entry if store is None).
        let home = Path::new("/nonexistent-home");
        let prefix = default_atpkg_prefix(home);
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, home),
            Some(&prefix),
            OsStr::new(""),
            None,
        );
        assert_eq!(t.targo, store_stage2_bin(&prefix).join(tool_file("targo")));
        assert_eq!(
            t.trustdoc,
            store_stage2_bin(&prefix).join(tool_file("trustdoc"))
        );
        assert!(!t.have_targo());
        assert!(t.tippy.is_none());
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, home),
            None,
            OsStr::new(""),
            None,
        );
        assert_eq!(
            t.targo,
            Path::new("/nonexistent-home/.rustup/toolchains/trust/bin").join(tool_file("targo"))
        );
    }

    /// Pending `atpkg` stubs on `PATH` are skipped in favor of real toolchains.
    #[cfg(unix)]
    #[test]
    fn a_pending_stub_on_path_is_passed_over() {
        let home = mktemp_dir("atv-pending").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let stubs = home.join("pkg-bin");
        fs::create_dir_all(&stubs).expect("mkdir");
        for name in ["targo", "trustc"] {
            fs::write(
                stubs.join(name),
                b"#!/bin/sh\n# atpkg pending-program stub v1\nexit 127\n",
            )
            .expect("write");
            make_runnable(&stubs.join(name));
        }
        let real = pinned_bin(&home.join("real/bin"));
        let path = std::env::join_paths([&stubs, &real]).expect("join");
        for pinned in [None, Some("trust")] {
            let t = Toolchain::discover_with_store(
                None,
                &rustup_home(None, &home),
                Some(&prefix),
                &path,
                pinned,
            );
            assert_eq!(t.targo, real.join("targo"), "pinned={pinned:?}");
            assert!(t.have_targo(), "pinned={pinned:?}");
            let t = Toolchain::discover_with_store(
                None,
                &rustup_home(None, &home),
                Some(&prefix),
                stubs.as_os_str(),
                pinned,
            );
            assert_eq!(
                t.targo,
                store_stage2_bin(&prefix).join("targo"),
                "pinned={pinned:?}"
            );
            assert!(!t.have_targo(), "pinned={pinned:?}");
        }
        let _ = fs::remove_dir_all(&home);
    }

    /// Create a valid pinned toolchain `bin` directory under `root` and return its canonical path.
    #[cfg(unix)]
    fn pinned_bin(root: &Path) -> PathBuf {
        fs::create_dir_all(root).expect("mkdir");
        exec_stub(&root.join("targo"));
        driver_stub(&root.join("trustc"));
        fs::canonicalize(root).expect("canonicalize")
    }

    /// Resolution order: explicit override > rustup > store > PATH (ignoring unprobed build trees).
    #[cfg(unix)]
    #[test]
    fn one_resolution_order_and_no_build_tree() {
        let home = mktemp_dir("atv-order").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = pinned_bin(&prefix.join("store/trust/6808/bin"));
        std::os::unix::fs::symlink(
            prefix.join("store/trust/6808"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let linked = pinned_bin(&home.join("sealed/bin"));
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(home.join("sealed"), home.join(".rustup/toolchains/trust"))
            .expect("ln rustup");
        let on_path = pinned_bin(&home.join("elsewhere/bin"));
        let tree = pinned_bin(&home.join("trust/build/host/stage2/bin"));
        let promoted = pinned_bin(&home.join("toolchains/trust-current/bin"));
        let path = std::env::join_paths([&tree, &promoted, &on_path]).expect("join");
        let found = |explicit: Option<&Path>, path: &OsStr| {
            Toolchain::discover_with_store(
                explicit,
                &rustup_home(None, &home),
                Some(&prefix),
                path,
                Some("trust"),
            )
        };

        let t = found(Some(&on_path), &path);
        assert_eq!(
            (t.stage2_dir.as_path(), t.store_bin.is_none()),
            (on_path.as_path(), true)
        );
        let t = found(None, &path);
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, linked);
        assert_eq!(t.store_bin, Some(store_stage2_bin(&prefix)));
        fs::remove_file(home.join(".rustup/toolchains/trust")).expect("rm rustup");
        assert_eq!(found(None, &path).stage2_dir, store);
        fs::remove_file(prefix.join("store/trust/current")).expect("rm current");
        assert_eq!(found(None, &path).stage2_dir, tree);
        let t = found(None, OsStr::new(""));
        assert!(
            !t.have_targo(),
            "a build tree under home is not a candidate: {}",
            t.stage2_dir.display()
        );
        assert_eq!(t.stage2_dir, store_stage2_bin(&prefix));
        fs::remove_dir_all(&home).ok();
    }

    /// `rustup_home` uses non-empty `$RUSTUP_HOME`, falling back to `<home>/.rustup`.
    #[test]
    fn rustup_home_is_the_variable_when_set_and_home_dot_rustup_otherwise() {
        let home = Path::new("/h");
        assert_eq!(rustup_home(None, home), Path::new("/h/.rustup"));
        assert_eq!(
            rustup_home(Some(OsStr::new("")), home),
            Path::new("/h/.rustup")
        );
        assert_eq!(
            rustup_home(Some(OsStr::new("/opt/rustup")), home),
            Path::new("/opt/rustup")
        );
    }

    /// Discovery resolves the pinned toolchain under a relocated `$RUSTUP_HOME`.
    #[cfg(unix)]
    #[test]
    fn a_relocated_rustup_home_is_where_the_pinned_toolchain_is_found() {
        let home = mktemp_dir("atv-rustup-home").expect("mktemp");
        let relocated = home.join("elsewhere/rustup");
        let linked = pinned_bin(&home.join("sealed/bin"));
        fs::create_dir_all(relocated.join("toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(home.join("sealed"), relocated.join("toolchains/trust"))
            .expect("ln rustup");
        let found = |rustup: &Path| {
            Toolchain::discover_with_store(None, rustup, None, OsStr::new(""), Some("trust"))
        };
        let t = found(&rustup_home(Some(relocated.as_os_str()), &home));
        assert!(t.have_targo(), "{}", t.stage2_dir.display());
        assert_eq!(t.stage2_dir, linked);
        let t = found(&rustup_home(None, &home));
        assert!(
            !t.have_targo(),
            "the default home holds no entry: {}",
            t.stage2_dir.display()
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn a_store_targo_that_is_not_the_pin_is_refused_and_the_search_goes_on() {
        let home = mktemp_dir("atv-storepin").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = store_stage2_bin(&prefix);
        fs::create_dir_all(&store).expect("mkdir");
        exec_stub(&store.join("targo"));
        let real = home.join("elsewhere/bin");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&real.join("targo"));
        driver_stub(&real.join("trustc"));

        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, &home),
            Some(&prefix),
            OsStr::new(real.to_string_lossy().as_ref()),
            Some("trust"),
        );
        assert!(t.have_targo(), "PATH still wins past a refused store");
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));

        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, &home),
            Some(&prefix),
            OsStr::new(""),
            Some("trust"),
        );
        assert!(!t.have_targo(), "fail-closed: the impostor is not adopted");
        assert_eq!(
            t.refused,
            Some(fs::canonicalize(&store).expect("canonicalize"))
        );
        fs::remove_dir_all(&home).ok();
    }

    /// Create a dated toolchain `bin` whose `trustc` answers both `--print sysroot` and `-vV`.
    #[cfg(unix)]
    fn dated_bin(fixture: &Path, sysroot: &Path, date: &str) -> PathBuf {
        let script = fixture.join("dated-trustc");
        if !script.is_file() {
            fs::write(
                &script,
                "#!/bin/sh\nroot=$(cd \"$(dirname \"$0\")/..\" && pwd -P)\n\
                 case \"$1\" in\n  --print) echo \"$root\" ;;\n  \
                 -vV) d=$(cat \"$root/commit-date\"); echo \"rustc 1.99.0-dev (0000000 $d)\"; \
                 echo \"commit-date: $d\" ;;\nesac\n",
            )
            .expect("write");
            make_runnable(&script);
            let _ = std::process::Command::new(&script).output();
        }
        let bin = sysroot.join("bin");
        fs::create_dir_all(sysroot.join("lib/rustlib")).expect("mkdir rustlib");
        fs::create_dir_all(&bin).expect("mkdir bin");
        let _ = fs::remove_file(bin.join("trustc"));
        fs::hard_link(&script, bin.join("trustc")).expect("link trustc");
        if !bin.join("targo").is_file() {
            exec_stub(&bin.join("targo"));
        }
        fs::write(sysroot.join("commit-date"), date).expect("date");
        fs::canonicalize(&bin).expect("canonicalize")
    }

    /// A rustup `trust` older than the store ranks below it and populates `demoted`.
    #[cfg(unix)]
    #[test]
    fn a_rustup_trust_older_than_the_store_ranks_below_it_and_is_named() {
        let home = mktemp_dir("atv-demote").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = dated_bin(&home, &prefix.join("store/trust/9192"), "2026-09-17");
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9192"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let stage2 = home.join("trust/build/aarch64-apple-darwin/stage2");
        let tree = dated_bin(&home, &stage2, "2026-08-20");
        fs::create_dir_all(home.join("trust/build")).expect("mkdir");
        std::os::unix::fs::symlink(
            home.join("trust/build/aarch64-apple-darwin"),
            home.join("trust/build/host"),
        )
        .expect("ln host");
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(
            home.join("trust/build/host/stage2"),
            home.join(".rustup/toolchains/trust"),
        )
        .expect("ln rustup");
        let found = || {
            Toolchain::discover_with_store(
                None,
                &home.join(".rustup"),
                Some(&prefix),
                OsStr::new(""),
                Some("trust"),
            )
        };

        let t = found();
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, store, "the older rustup entry must not win");
        let d = t.demoted.clone().expect("the demotion is named");
        assert_eq!(
            (d.dir.as_path(), d.its.as_str(), d.store.as_str()),
            (tree.as_path(), "2026-08-20", "2026-09-17")
        );
        assert!(d.linked, "the measured entry is a link");
        assert!(d.sentence().ends_with(REMEDY_LINK), "{}", d.sentence());

        let entry = home.join(".rustup/toolchains/trust");
        fs::remove_file(&entry).expect("rm the link");
        let entry_bin = dated_bin(&home, &entry, "2026-08-20");
        let t = found();
        assert_eq!(t.stage2_dir, store, "a directory entry is demoted too");
        let d = t.demoted.clone().expect("and named");
        assert_eq!((d.dir.as_path(), d.linked), (entry_bin.as_path(), false));
        let said = d.sentence();
        assert!(
            said.ends_with(REMEDY_DIRECTORY) && !said.contains(REMEDY_LINK),
            "{said}"
        );
        fs::remove_dir_all(&entry).expect("rm the directory");
        std::os::unix::fs::symlink(home.join("trust/build/host/stage2"), &entry)
            .expect("ln rustup again");

        for (date, why) in [
            ("2026-09-18", "newer"),
            ("2026-09-17", "same day"),
            ("unknown", "undated"),
        ] {
            dated_bin(&home, &stage2, date);
            let t = found();
            assert_eq!(t.stage2_dir, tree, "{why}: the rustup entry keeps its rank");
            assert_eq!(t.demoted, None, "{why}");
        }
        dated_bin(&home, &stage2, "2026-08-20");
        fs::create_dir_all(prefix.join("links")).expect("mkdir links");
        fs::write(prefix.join("links/trust"), "a dev-link marker").expect("marker");
        let t = found();
        assert_eq!(
            t.stage2_dir, tree,
            "dev-linked: nothing is stale against the store"
        );
        assert_eq!(t.demoted, None);
        fs::remove_file(prefix.join("links/trust")).expect("rm marker");

        fs::write(prefix.join("store/trust/9192/commit-date"), "2026-09-17").expect("date");
        fs::remove_dir_all(prefix.join("store/trust/9192/lib")).expect("rm sysroot");
        let on_path = pinned_bin(&home.join("elsewhere/bin"));
        let t = Toolchain::discover_with_store(
            None,
            &home.join(".rustup"),
            Some(&prefix),
            on_path.as_os_str(),
            Some("trust"),
        );
        assert_eq!(
            t.stage2_dir, tree,
            "demoted below the store, never below PATH"
        );
        assert_eq!(t.demoted, None, "it won, so nothing was demoted in effect");
        assert!(t.refused.is_none());
        fs::remove_dir_all(&home).ok();
    }

    /// An `atpkg` rustup view resolves through to the store build or dev-linked checkout it presents.
    #[cfg(unix)]
    #[test]
    fn atpkgs_view_is_read_through_to_the_build_it_presents() {
        let home = mktemp_dir("atv-view").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = dated_bin(&home, &prefix.join("store/trust/9200"), "2026-09-24");
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9200"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let view = dated_bin(&home, &prefix.join("rustup/trust"), "2026-09-17");
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        let entry = home.join(".rustup/toolchains/trust");
        std::os::unix::fs::symlink(prefix.join("rustup/trust"), &entry).expect("ln rustup");
        let found = || {
            Toolchain::discover_with_store(
                None,
                &home.join(".rustup"),
                Some(&prefix),
                OsStr::new(""),
                Some("trust"),
            )
        };
        let t = found();
        assert_eq!(
            t.stage2_dir, store,
            "a lagging view presents the store's build"
        );
        assert_ne!(t.stage2_dir, view, "never the view's own clones");
        assert_eq!(t.demoted, None, "read through, so never demoted");
        dated_bin(&home, &prefix.join("rustup/trust"), "2026-09-24");
        assert_eq!(
            found().stage2_dir,
            store,
            "a current view: the store's own directory"
        );
        let checkout = home.join("trust-checkout");
        let linked = dated_bin(&home, &checkout, "2026-09-20");
        fs::create_dir_all(prefix.join("links")).expect("mkdir links");
        fs::write(
            prefix.join("links/trust"),
            format!(
                "program = \"trust\"\ncheckout = \"{}\"\n",
                checkout.display()
            ),
        )
        .expect("marker");
        assert_eq!(
            found().stage2_dir,
            linked,
            "a linked view presents its checkout"
        );
        fs::remove_dir_all(checkout.join("lib")).expect("rm lib");
        assert_eq!(
            found().stage2_dir,
            store,
            "an unpresentable link: the store"
        );
        fs::write(prefix.join("links/trust"), "not a marker").expect("garbage");
        assert_eq!(found().stage2_dir, store, "an unreadable marker is no link");
        fs::remove_file(prefix.join("links/trust")).expect("rm marker");
        let tree = dated_bin(&home, &home.join("stage2"), "2026-09-17");
        fs::remove_file(&entry).expect("rm link");
        std::os::unix::fs::symlink(home.join("stage2"), &entry).expect("ln foreign");
        let d = found().demoted.expect("a foreign older link is said");
        assert_eq!(d.dir, tree);
        assert!(d.sentence().ends_with(REMEDY_LINK));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn the_prefix_mirror_honours_a_configured_packages_prefix() {
        let home = Path::new("/fake-home");
        let opt = |rest: &str| {
            if cfg!(windows) {
                format!("C:/opt/{rest}")
            } else {
                format!("/opt/{rest}")
            }
        };
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"~/lab/pkg\"\n", home),
            Some(PathBuf::from("/fake-home/lab/pkg"))
        );
        assert_eq!(
            configured_prefix_in(
                &format!(
                    "[packages]\n# prefix = \"~/decoy\"\nprefix = \"{}\"\n",
                    opt("aterm/pkg")
                ),
                home
            ),
            Some(PathBuf::from(opt("aterm/pkg")))
        );
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"relative/pkg\"\n", home),
            None
        );
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"~user/pkg\"\n", home),
            None
        );
        assert_eq!(
            configured_prefix_in("[other]\nprefix = \"/opt/x\"\n", home),
            None
        );
        assert_eq!(configured_prefix_in("", home), None);

        let tmp = mktemp_dir("atv-prefix").expect("mktemp");
        assert_eq!(atpkg_prefix(&tmp, None), default_atpkg_prefix(&tmp));
        let xdg = tmp.join("xdg");
        fs::create_dir_all(xdg.join("aterm")).expect("mkdir");
        fs::write(
            xdg.join("aterm/aterm.toml"),
            format!("[packages]\nprefix = \"{}\"\n", opt("lab")),
        )
        .expect("write");
        assert_eq!(atpkg_prefix(&tmp, Some(&xdg)), PathBuf::from(opt("lab")));
        fs::create_dir_all(tmp.join(".config/aterm")).expect("mkdir");
        fs::write(
            tmp.join(".config/aterm/aterm.toml"),
            "[packages]\nprefix = \"~/mine\"\n",
        )
        .expect("write");
        assert_eq!(atpkg_prefix(&tmp, None), tmp.join("mine"));
        assert_eq!(
            store_stage2_bin(&default_atpkg_prefix(Path::new("/h"))),
            Path::new(if cfg!(target_os = "macos") {
                "/h/Library/Application Support/aterm/pkg/store/trust/current/bin"
            } else if cfg!(windows) {
                "/h/AppData/Local/aterm/pkg/store/trust/current/bin"
            } else {
                "/h/.local/share/aterm/pkg/store/trust/current/bin"
            })
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// Symlinked toolchain directories are canonicalized to their physical path.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_stage2_resolves_to_its_physical_path() {
        let tmp = mktemp_dir("atv-tc").expect("mktemp");
        let real = tmp.join("aarch64-apple-darwin/stage2/bin");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&real.join("targo"));
        std::os::unix::fs::symlink(tmp.join("aarch64-apple-darwin"), tmp.join("host")).expect("ln");

        let via_link = tmp.join("host/stage2/bin");
        let t = Toolchain::discover(Some(&via_link), Path::new("/unused"), OsStr::new(""), None);
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));
        assert!(
            !t.stage2_dir.to_string_lossy().contains("/host/"),
            "the symlinked spelling must not survive into the tool paths"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn tippy_is_found_by_its_trust_name_only() {
        let tmp = mktemp_dir("atv-tippy").expect("mktemp");
        let real = fs::canonicalize(&tmp).expect("canonicalize");
        exec_stub(&tmp.join("targo-clippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert_eq!(
            t.tippy, None,
            "the retired pre-rebrand name is not a tippy: the lane is NOT RUN"
        );

        exec_stub(&tmp.join("targo-tippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert_eq!(
            t.tippy.as_deref(),
            Some(real.join(tool_file("targo-tippy")).as_path())
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// A non-override toolchain missing `trustfmt`/`targo-tippy` borrows them from the managed build.
    #[cfg(unix)]
    #[test]
    fn a_toolchain_lacking_the_formatter_and_lint_takes_the_managed_builds() {
        let home = mktemp_dir("atv-borrow").expect("mktemp");
        let home = fs::canonicalize(&home).expect("canonicalize");
        let prefix = default_atpkg_prefix(&home);
        let store = pinned_bin(&prefix.join("store/trust/9192/bin"));
        for tool in [TRUSTFMT, TIPPY] {
            exec_stub(&store.join(tool));
        }
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9192"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let stage2 = pinned_bin(&home.join("trust/build/host/stage2/bin"));
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(
            home.join("trust/build/host/stage2"),
            home.join(".rustup/toolchains/trust"),
        )
        .expect("ln rustup");
        let stray = home.join("stray/bin");
        fs::create_dir_all(&stray).expect("mkdir");
        exec_stub(&stray.join(TRUSTFMT));
        let found = |explicit: Option<&Path>, path: &OsStr| {
            Toolchain::discover_with_store(
                explicit,
                &rustup_home(None, &home),
                Some(&prefix),
                path,
                Some("trust"),
            )
        };
        let t = found(None, OsStr::new(""));
        assert_eq!(
            t.stage2_dir, stage2,
            "the toolchain is still the rustup entry"
        );
        assert!(t.have_targo());
        assert_eq!(t.trustfmt.as_deref(), Some(store.join(TRUSTFMT).as_path()));
        assert_eq!(t.tippy.as_deref(), Some(store.join(TIPPY).as_path()));
        assert_eq!(t.borrowed(TRUSTFMT), Some(store.join(TRUSTFMT).as_path()));
        let note = t.borrowed_note().expect("said");
        assert!(
            note.contains(&format!("trustfmt from {}", store.join(TRUSTFMT).display()))
                && note.contains(&format!("targo-tippy from {}", store.join(TIPPY).display()))
                && note.contains(&format!("{} has none", stage2.display())),
            "{note}"
        );
        let t = found(Some(&stage2), OsStr::new(""));
        assert_eq!(
            (t.stage2_dir.as_path(), t.store_bin.is_none()),
            (stage2.as_path(), true)
        );
        assert_eq!((t.trustfmt.as_deref(), t.tippy.as_deref()), (None, None));
        assert!(t.borrowed.is_empty());
        let skip = t.missing_tippy_label();
        assert!(
            skip.contains("TRUST_STAGE2_BIN=") && skip.contains(Toolchain::OVERRIDE_REMEDY),
            "{skip}"
        );

        let moved = home.join("store-away");
        fs::rename(prefix.join("store"), &moved).expect("move the store away");
        let t = found(None, stray.as_os_str());
        assert_eq!((t.trustfmt.as_deref(), t.tippy.as_deref()), (None, None));
        assert!(t.borrowed.is_empty() && t.borrowed_note().is_none());
        assert!(
            t.missing_trustfmt_label()
                .contains("the atpkg store or PATH")
                && t.missing_tippy_label().contains("the atpkg store or PATH"),
            "the skip names everywhere it looked: {}",
            t.missing_tippy_label()
        );
        fs::rename(&moved, prefix.join("store")).expect("move the store back");
        let current = prefix.join("store/trust/current");
        let hidden = prefix.join("store/trust/hidden-current");
        fs::rename(&current, &hidden).expect("hide current");
        let shims = pinned_bin(&prefix.join("bin"));
        fs::write(
            shims.join("trustc"),
            format!(
                "#!/bin/sh\necho '{}'\n",
                store.parent().expect("sysroot").display()
            ),
        )
        .expect("write the shim's sysroot response");
        exec_stub(&shims.join(TIPPY));
        let path = std::env::join_paths([&stray, &shims]).expect("join");
        for pin in [None, Some("stable")] {
            assert_eq!(
                managed_tool(TIPPY, None, shims.as_os_str(), pin),
                Some(shims.join(TIPPY))
            );
        }
        let t = found(None, &path);
        assert_eq!(
            t.tippy.as_deref(),
            Some(store.join(TIPPY).as_path()),
            "BORROWED_TOOL_MUST_NAME_THE_REPORTED_PHYSICAL_BUILD"
        );
        assert_eq!(t.trustfmt, None, "the stray one is still not taken");
        let parked = store.join("tippy-away");
        fs::rename(store.join(TIPPY), &parked).expect("hide the physical tool");
        assert_eq!(
            found(None, &path).tippy,
            None,
            "a shim alone is not the tool"
        );
        fs::rename(&parked, store.join(TIPPY)).expect("restore the physical tool");
        fs::rename(&hidden, &current).expect("restore current");

        for tool in [TRUSTFMT, TIPPY] {
            exec_stub(&stage2.join(tool));
        }
        let t = found(None, OsStr::new(""));
        assert_eq!(t.trustfmt.as_deref(), Some(stage2.join(TRUSTFMT).as_path()));
        assert_eq!(t.tippy.as_deref(), Some(stage2.join(TIPPY).as_path()));
        assert!(t.borrowed.is_empty() && t.borrowed_note().is_none());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn path_is_only_rewritten_when_a_targo_is_really_there() {
        let tmp = mktemp_dir("atv-path").expect("mktemp");
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert_eq!(
            t.path_with_stage2_first(OsStr::new("/usr/bin")),
            OsString::from("/usr/bin")
        );

        exec_stub(&tmp.join("targo"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        let want =
            std::env::join_paths([t.stage2_dir.as_os_str(), OsStr::new("/usr/bin")]).expect("join");
        assert_eq!(t.path_with_stage2_first(OsStr::new("/usr/bin")), want);
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_pinned_channel_is_the_one_line_the_manifest_declares() {
        assert_eq!(
            pinned_channel_in("# channel = \"decoy\"\n[toolchain]\nchannel = \"trust\"\n")
                .as_deref(),
            Some("trust"),
            "a commented-out channel must not be mistaken for the pin"
        );
        assert_eq!(pinned_channel_in("[toolchain]\n").as_deref(), None);
        assert_eq!(
            pinned_channel(Path::new("/nonexistent-repo")).as_deref(),
            None
        );
    }

    #[test]
    fn a_targo_that_is_not_the_pinned_toolchain_is_refused_not_adopted() {
        // A `targo` without a matching `trustc` answering `--print sysroot` is refused.
        let tmp = mktemp_dir("atv-pin").expect("mktemp");
        let bin = tmp.join("bin");
        fs::create_dir_all(&bin).expect("mkdir");
        exec_stub(&bin.join("targo"));

        let t = Toolchain::discover(
            Some(&bin),
            Path::new("/unused"),
            OsStr::new(""),
            Some("trust"),
        );
        assert!(!t.have_targo(), "fail-closed: not the pinned toolchain");
        assert!(t.refused.is_some());

        exec_stub(&bin.join("trustc"));
        let t = Toolchain::discover(
            Some(&bin),
            Path::new("/unused"),
            OsStr::new(""),
            Some("trust"),
        );
        assert!(
            !t.have_targo(),
            "a driver that names no sysroot is not the pin (mid-rebuild stage tree)"
        );
        assert!(t.refused.is_some());

        driver_stub(&bin.join("trustc"));
        let t = Toolchain::discover(
            Some(&bin),
            Path::new("/unused"),
            OsStr::new(""),
            Some("trust"),
        );
        assert!(t.have_targo());
        assert!(t.refused.is_none());

        fs::remove_file(bin.join(tool_file("trustc"))).expect("rm");
        let t = Toolchain::discover(
            Some(&bin),
            Path::new("/unused"),
            OsStr::new(""),
            Some("stable"),
        );
        assert!(t.have_targo(), "upstream channels are a passthrough");
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_path_fallback_adopts_only_the_pinned_toolchain() {
        // PATH fallback skips unpinned `targo` binaries and adopts the first pinned one.
        let tmp = mktemp_dir("atv-pinpath").expect("mktemp");
        let impostor = tmp.join("impostor");
        let real = tmp.join("stage/bin");
        fs::create_dir_all(&impostor).expect("mkdir");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&impostor.join("targo"));
        exec_stub(&real.join("targo"));
        driver_stub(&real.join("trustc"));
        let path = std::env::join_paths([&impostor, &real]).expect("join");
        let home = Path::new("/nonexistent-home");
        let discover = |path: &OsStr| {
            Toolchain::discover_with_store(
                None,
                &rustup_home(None, home),
                Some(&default_atpkg_prefix(home)),
                path,
                Some("trust"),
            )
        };

        let t = discover(OsStr::new(&path));
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));

        let t = discover(OsStr::new(impostor.to_string_lossy().as_ref()));
        assert!(!t.have_targo());
        assert_eq!(
            t.refused,
            Some(fs::canonicalize(&impostor).expect("canonicalize"))
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_directory_named_targo_is_not_a_driver() {
        let tmp = mktemp_dir("atv-dir").expect("mktemp");
        fs::create_dir_all(tmp.join("targo")).expect("mkdir");
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None);
        assert!(
            !t.have_targo(),
            "fail-closed: a directory is not the verified driver"
        );
        fs::remove_dir_all(&tmp).ok();
    }
}
