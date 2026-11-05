// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed genuine-Targo recipes and original, settled root output data.
//! Neither a recipe nor an output receipt grants execution or resource authority.

use crate::epoch_controller::{RootCancellation, RootOutcome};
use std::{
    collections::BTreeSet,
    fmt,
    fs::{File, Metadata, OpenOptions},
    io::{self, Read as _},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    path::{Component, Path, PathBuf},
};

/// Bound on captured roots retained by one Owner, across active epochs and history.
/// Two original log descriptors are retained per configured root.
pub const MAX_CAPTURED_ROOTS: usize = 16;
/// Hard aggregate stdout/stderr allocation bound for one consumed output receipt.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const RECIPE_BYTES: usize = 16 * 1024;
const RECIPES_BYTES: usize = 64 * 1024;

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why)
}

/// Supported genuine-Targo profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootProfile {
    /// Cargo's development profile (`dev`, with outputs under `debug`).
    Dev,
    /// Cargo's optimized `release` profile.
    Release,
}

/// Explicit build inputs. Unsupported arbitrary arguments and environment are absent.
/// The helper supplies the private GNU transport and root-control endpoint itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootRecipe {
    /// One-use root slot within the original epoch.
    pub id: u32,
    /// Supported Cargo profile selected by the checked build plan.
    pub profile: RootProfile,
    /// Exact declared binary target to compile.
    pub bin: String,
    /// Requested driver parallelism; the inherited GNU pool remains authoritative.
    pub jobs: u32,
    /// Optional Cargo package selection, distinct from the binary name.
    pub package: Option<String>,
    /// Explicit Cargo feature arguments, in their original order.
    pub features: Vec<String>,
    /// Whether Cargo's default features remain enabled.
    pub default_features: bool,
    /// Whether the build selects all package features.
    pub all_features: bool,
    /// Whether Cargo must avoid network access for dependency resolution.
    pub offline: bool,
    /// Require checksum freshness for Rust and build-script inputs in a capable driver.
    pub content_inputs_v1: bool,
    /// Fixed build analysis with the original shared cache home, never a new cache.
    pub analysis: Option<RootAnalysis>,
    /// Exact semantic version injected as the harness build stamp.
    pub version: String,
    /// Canonical immutable SDK `trustc` executable.
    pub rustc: PathBuf,
    /// Canonical immutable SDK `trustdoc` executable beside `trustc`.
    pub rustdoc: PathBuf,
    /// Explicit absolute PATH entries, starting with the SDK bin directory.
    pub path: Vec<PathBuf>,
}

/// Analysis startup data. It supplies no executable, flags or release authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootAnalysis {
    /// The original driver's cache home, captured before dispatch.
    pub cargo_home: PathBuf,
}

fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn semver(value: &str) -> bool {
    fn numeric(value: &str) -> bool {
        !value.is_empty()
            && value.bytes().all(|b| b.is_ascii_digit())
            && (value.len() == 1 || !value.starts_with('0'))
            && value.parse::<u64>().is_ok()
    }
    fn identifiers(value: &str, prerelease: bool) -> bool {
        value.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && (!prerelease
                    || !part.bytes().all(|b| b.is_ascii_digit())
                    || part.len() == 1
                    || !part.starts_with('0'))
        })
    }
    if value.len() > 256 {
        return false;
    }
    let (core, build) = value
        .split_once('+')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    if build.is_some_and(|b| !identifiers(b, false)) {
        return false;
    }
    let (core, pre) = core
        .split_once('-')
        .map_or((core, None), |(a, b)| (a, Some(b)));
    let numbers: Vec<_> = core.split('.').collect();
    numbers.len() == 3
        && numbers.iter().all(|s| numeric(s))
        && pre.is_none_or(|p| identifiers(p, true))
}

fn driver(value: &Path, expected: &str) -> io::Result<()> {
    path(value)?;
    if value.file_name().and_then(|p| p.to_str()) != Some(expected)
        || value.components().any(|part| {
            let s = part.as_os_str().to_string_lossy();
            s.len() == 6 && s.starts_with("stage") && s.as_bytes()[5].is_ascii_digit()
        })
        || value.canonicalize()? != value
    {
        return Err(bad(
            "recipe compiler must name its exact immutable SDK driver",
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(value)?;
    let m = file.metadata()?;
    // SAFETY: geteuid only reads this process's account identity.
    let uid = unsafe { libc::geteuid() };
    if !m.is_file()
        || m.mode() & 0o111 == 0
        || m.mode() & 0o222 != 0
        || (m.uid() != uid && m.uid() != 0)
    {
        return Err(bad("recipe compiler must be an owned immutable executable"));
    }
    Ok(())
}

fn path(value: &Path) -> io::Result<&str> {
    let value = value
        .to_str()
        .ok_or_else(|| bad("recipe paths must be UTF-8"))?;
    if !Path::new(value).is_absolute()
        || !text(value)
        || Path::new(value)
            .components()
            .any(|p| p == Component::ParentDir)
    {
        return Err(bad("recipe paths must be absolute"));
    }
    Ok(value)
}

impl RootRecipe {
    /// Validate bounded startup data before any original admission is consumed.
    /// # Errors
    /// Invalid scope, duplicate root IDs, unsupported values or size bounds.
    pub fn validate_set(recipes: &[Self], roots: u32) -> io::Result<()> {
        encoded(recipes, roots).map(|_| ())
    }

    fn encode(&self, roots: u32) -> io::Result<String> {
        if self.id >= roots
            || !(1..=256).contains(&self.jobs)
            || !name(&self.bin)
            || !semver(&self.version)
            || self.package.as_deref().is_some_and(|s| !name(s))
            || self.features.len() > 64
            || self.features.iter().any(|s| {
                s.is_empty()
                    || s.len() > 128
                    || s.starts_with('-')
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-/.:+,".contains(&b))
            })
            || self.path.is_empty()
            || self.path.len() > 64
        {
            return Err(bad("invalid typed root recipe"));
        }
        if self.rustc.parent() != self.rustdoc.parent()
            || self.path.first().map(PathBuf::as_path) != self.rustc.parent()
        {
            return Err(bad("recipe PATH must begin with exact SDK bin directory"));
        }
        std::env::join_paths(&self.path).map_err(|_| bad("invalid recipe PATH"))?;
        driver(&self.rustc, "trustc")?;
        driver(&self.rustdoc, "trustdoc")?;
        let paths = self
            .path
            .iter()
            .map(|p| path(p))
            .collect::<io::Result<Vec<_>>>()?;
        let mut value = aterm_json::json!({
            "schema":1,"id":self.id,
            "profile":match self.profile { RootProfile::Dev=>"dev",RootProfile::Release=>"release" },
            "bin":self.bin,"jobs":self.jobs,"package":self.package,
            "features":self.features,"default_features":self.default_features,
            "all_features":self.all_features,"offline":self.offline,
            "version":self.version,"rustc":path(&self.rustc)?,
            "rustdoc":path(&self.rustdoc)?,"path":paths
        });
        if self.content_inputs_v1 {
            value
                .as_object_mut()
                .ok_or_else(|| bad("recipe object missing"))?
                .insert("content_inputs_v1".into(), true.into());
        }
        if let Some(analysis) = &self.analysis {
            value
                .as_object_mut()
                .ok_or_else(|| bad("recipe object missing"))?
                .insert(
                    "build_analysis".into(),
                    aterm_json::json!({"cargo_home":path(&analysis.cargo_home)?}),
                );
        }
        let bytes = aterm_json::to_vec(&value).map_err(|e| bad(&e.to_string()))?;
        if bytes.len() > RECIPE_BYTES {
            return Err(bad("typed root recipe exceeds startup bound"));
        }
        String::from_utf8(bytes).map_err(|_| bad("invalid recipe encoding"))
    }
}

pub(crate) fn encoded(recipes: &[RootRecipe], roots: u32) -> io::Result<Vec<String>> {
    if recipes.len() > MAX_CAPTURED_ROOTS || recipes.len() > roots as usize {
        return Err(bad("too many captured root recipes"));
    }
    let mut ids = BTreeSet::new();
    let mut total = 0usize;
    recipes
        .iter()
        .map(|recipe| {
            if !ids.insert(recipe.id) {
                return Err(bad("duplicate typed root recipe ID"));
            }
            let bytes = recipe.encode(roots)?;
            total = total
                .checked_add(bytes.len())
                .ok_or_else(|| bad("recipe size overflow"))?;
            if total > RECIPES_BYTES {
                return Err(bad("typed root recipes exceed startup bound"));
            }
            Ok(bytes)
        })
        .collect()
}

#[derive(PartialEq, Eq)]
struct Version(u64, u64, u32, u32, u32, u64, i64, i64, i64, i64);
fn version(meta: &Metadata) -> Version {
    Version(
        meta.dev(),
        meta.ino(),
        meta.mode(),
        meta.uid(),
        meta.gid(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

pub(super) fn check_log(file: &File) -> io::Result<Metadata> {
    let meta = file.metadata()?;
    // SAFETY: geteuid only reads this process's account identity.
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.mode() & 0o7777 != 0o600
        || meta.uid() != unsafe { libc::geteuid() }
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "original root log changed type, links or privacy",
        ));
    }
    Ok(meta)
}

pub(super) struct CapturedLogs {
    pub(super) stdout: File,
    pub(super) stderr: File,
}

/// Non-cloneable data custody from an original epoch whose whole cohort and CPU units settled.
/// The original files were pinned before any root could spawn. This is not artifact acceptance.
pub struct RootOutput {
    pub(super) _target_lease: crate::epoch_target::TargetLoan,
    pub(super) logs: CapturedLogs,
    pub(super) recipe: RootRecipe,
    pub(super) manifest: PathBuf,
    pub(super) target: PathBuf,
    pub(super) epoch: String,
    pub(super) outcome: RootOutcome,
    pub(super) cancellation: Option<RootCancellation>,
}

impl fmt::Debug for RootOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RootOutput")
            .field("epoch", &self.epoch)
            .field("outcome", &self.outcome)
            .field("cancellation", &self.cancellation)
            .finish_non_exhaustive()
    }
}

/// Complete bounded data, with actual native outcome and cancellation kept separate.
#[derive(Debug)]
pub struct RootOutputBytes {
    _target_lease: crate::epoch_target::TargetLoan,
    /// Original typed recipe that configured this root.
    pub recipe: RootRecipe,
    /// Exact manifest path queued with the original Spawn request.
    pub manifest: PathBuf,
    /// Exact stable target queued with the original Spawn request.
    pub target: PathBuf,
    /// Private original epoch identity, retained as diagnostic data.
    pub epoch: String,
    /// Actual validated root exit observation, independent of resource settlement.
    pub outcome: RootOutcome,
    /// Cancellation intent and acknowledgement, independent of native exit success.
    pub cancellation: Option<RootCancellation>,
    /// Complete bounded bytes from the original stdout inode.
    pub stdout: Vec<u8>,
    /// Complete bounded bytes from the original stderr inode.
    pub stderr: Vec<u8>,
}

impl RootOutput {
    /// Original recipe supplied before helper dispatch.
    #[must_use]
    pub fn recipe(&self) -> &RootRecipe {
        &self.recipe
    }
    /// Exact manifest queued with the original root request.
    #[must_use]
    pub fn manifest(&self) -> &Path {
        &self.manifest
    }
    /// Exact stable target queued with the original root request.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }
    /// Original private epoch identity, without process or release authority.
    #[must_use]
    pub fn epoch(&self) -> &str {
        &self.epoch
    }
    /// Actual validated root exit, without rewriting failures or signals.
    #[must_use]
    pub fn outcome(&self) -> RootOutcome {
        self.outcome
    }
    /// Cancellation intent and acknowledgement, kept separate from native exit.
    #[must_use]
    pub fn cancellation(&self) -> Option<RootCancellation> {
        self.cancellation
    }

    /// Consume the original receipt outside the execution owner's tick.
    /// # Errors
    /// A bound overflow, changed original regular file, or read error. Resource settlement
    /// has already completed and is unaffected; partial data never becomes a complete result.
    pub fn read(mut self, limit: usize) -> io::Result<RootOutputBytes> {
        self._target_lease.check()?;
        if limit > MAX_OUTPUT_BYTES {
            return Err(bad("root output limit exceeds hard bound"));
        }
        let stdout_before = check_log(&self.logs.stdout)?;
        let stderr_before = check_log(&self.logs.stderr)?;
        if stdout_before
            .len()
            .checked_add(stderr_before.len())
            .is_none_or(|n| n > limit as u64)
        {
            return Err(bad("root output exceeds requested bound"));
        }
        let mut stdout =
            Vec::with_capacity(usize::try_from(stdout_before.len().saturating_add(1)).unwrap_or(0));
        (&mut self.logs.stdout)
            .take(limit as u64 + 1)
            .read_to_end(&mut stdout)?;
        if stdout.len() > limit {
            return Err(bad("root stdout exceeds requested bound"));
        }
        let mut stderr =
            Vec::with_capacity(usize::try_from(stderr_before.len().saturating_add(1)).unwrap_or(0));
        (&mut self.logs.stderr)
            .take((limit - stdout.len()) as u64 + 1)
            .read_to_end(&mut stderr)?;
        if stdout.len() + stderr.len() > limit {
            return Err(bad("root output exceeds requested bound"));
        }
        if version(&stdout_before) != version(&check_log(&self.logs.stdout)?)
            || version(&stderr_before) != version(&check_log(&self.logs.stderr)?)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "original root output changed during read",
            ));
        }
        self._target_lease.check()?;
        Ok(RootOutputBytes {
            _target_lease: self._target_lease,
            recipe: self.recipe,
            manifest: self.manifest,
            target: self.target,
            epoch: self.epoch,
            outcome: self.outcome,
            cancellation: self.cancellation,
            stdout,
            stderr,
        })
    }
}

#[cfg(test)]
#[path = "epoch_recipe_tests.rs"]
mod tests;
