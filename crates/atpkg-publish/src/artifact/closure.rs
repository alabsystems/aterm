// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Freezes the rustc-dev and rust-src closure into the staged Trust bundle.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::triple::Triple;

/// Why the closure could not be frozen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A path that must be a regular file is not (missing, a link, special).
    NotRegular(PathBuf),
    /// A path that must be a real directory is not (missing, or a link to one).
    NotRealDir(PathBuf),
    /// A link or special file inside a tree the closure copies.
    LinkInClosure(PathBuf),
    /// PROVENANCE names one of its fields twice.
    DuplicateField(String),
    /// The compiler does not report exactly one commit and the packaged host.
    CompilerIdentity(String),
    /// The two roots overlap.
    OverlappingRoots,
    /// The stage's `lib/` does not carry exactly one compiler driver.
    DriverCount(usize),
    /// The target lib lacks a required member (`std`, `core`, rustc-private metadata).
    MissingSurface(&'static str, PathBuf),
    /// The target-lib driver is not the selected compiler's.
    DriverDiffers,
    /// A repack's `.rustc-commit-hash` marker names another compiler.
    MarkerCommit,
    /// PROVENANCE's `reported_commit` is not the compiler's.
    SealCommit,
    /// PROVENANCE's driver fingerprint is not the staged driver.
    DriverFingerprint,
    /// PROVENANCE names no valid `private_sysroot` (missing, or a traversal).
    PrivateSysrootName(String),
    /// The private sysroot was built for another compiler.
    PrivateCommit,
    /// The private sysroot's driver is not the selected compiler's.
    PrivateDriver,
    /// A private member collides with a different file of the same name.
    Conflict(PathBuf),
    /// Frozen rust-src lacks one of its two mandatory trees.
    EmptySource(&'static str),
    /// `rustc-src/rust` is a link, but not the sealed internal alias.
    AliasNotSealed(PathBuf),
    /// A materialized `rustc-src/rust` differs from frozen rust-src.
    AliasDiffers,
    /// An I/O failure.
    Io(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self {
            Self::NotRegular(p) => format!("not a regular file: {}", p.display()),
            Self::NotRealDir(p) => format!("not a real directory: {}", p.display()),
            Self::LinkInClosure(p) => format!("link or special file in closure: {}", p.display()),
            Self::DuplicateField(k) => format!("duplicate provenance field: {k}"),
            Self::CompilerIdentity(why) => format!(
                "the Trust compiler must report one exact commit and the packaged host triple \
                 ({why})"
            ),
            Self::OverlappingRoots => "staging and source roots must be separate".into(),
            Self::DriverCount(n) => {
                format!("a Trust bundle must carry exactly one compiler driver, not {n}")
            }
            Self::MissingSurface(what, p) => format!("missing {what} in {}", p.display()),
            Self::DriverDiffers => "rustc-private driver differs from the selected compiler".into(),
            Self::MarkerCommit => "embedded private compiler commit mismatch".into(),
            Self::SealCommit => "seal/compiler commit mismatch".into(),
            Self::DriverFingerprint => "sealed compiler driver fingerprint mismatch".into(),
            Self::PrivateSysrootName(n) => {
                format!("missing or invalid sealed private_sysroot name `{n}`")
            }
            Self::PrivateCommit => "private/compiler commit mismatch".into(),
            Self::PrivateDriver => {
                "sealed private driver differs from the selected compiler".into()
            }
            Self::Conflict(p) => {
                format!("conflicting rustc-private closure member: {}", p.display())
            }
            Self::EmptySource(c) => format!("empty frozen rust-src {c}"),
            Self::AliasNotSealed(p) => {
                format!(
                    "rustc-src is not the sealed internal source alias: {}",
                    p.display()
                )
            }
            Self::AliasDiffers => "rustc-src differs from frozen rust-src".into(),
            Self::Io(e) => e.clone(),
        };
        write!(
            f,
            "REFUSING incomplete Trust closure: {what}; build and promote a complete matching \
             Trust seal"
        )
    }
}

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Self {
        Error::refused(r.to_string())
    }
}

type R<T> = std::result::Result<T, Refusal>;

fn io(what: &Path, e: &std::io::Error) -> Refusal {
    Refusal::Io(format!("{}: {e}", what.display()))
}

fn regular(p: &Path) -> R<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_file() => Ok(()),
        _ => Err(Refusal::NotRegular(p.to_path_buf())),
    }
}

fn directory(p: &Path) -> R<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        _ => Err(Refusal::NotRealDir(p.to_path_buf())),
    }
}

fn below(root: &Path, relative: &str) -> R<PathBuf> {
    directory(root)?;
    let mut p = root.to_path_buf();
    for part in relative.split('/').filter(|s| !s.is_empty()) {
        p.push(part);
        directory(&p)?;
    }
    Ok(p)
}

fn digest(p: &Path) -> R<String> {
    regular(p)?;
    atpkg::sha256_file(p).map_err(|e| io(p, &e))
}

fn files(root: &Path) -> R<Vec<PathBuf>> {
    directory(root)?;
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| io(root, &e))?
        .map(|e| e.map(|e| e.path()).map_err(|e| io(root, &e)))
        .collect::<R<_>>()?;
    entries.sort();
    for p in entries {
        let ft = std::fs::symlink_metadata(&p)
            .map_err(|e| io(&p, &e))?
            .file_type();
        if ft.is_dir() {
            out.extend(files(&p)?);
        } else if ft.is_file() {
            out.push(p);
        } else {
            return Err(Refusal::LinkInClosure(p));
        }
    }
    Ok(out)
}

fn fields(path: &Path) -> R<BTreeMap<String, String>> {
    regular(path)?;
    let text = std::fs::read_to_string(path).map_err(|e| io(path, &e))?;
    let selected = [
        "reported_commit",
        "private_sysroot",
        "artifact_driver_name",
        "artifact_driver_sha256",
    ];
    let mut out = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if !selected.contains(&k) {
            continue;
        }
        if out.insert(k.to_string(), v.to_string()).is_some() {
            return Err(Refusal::DuplicateField(k.to_string()));
        }
    }
    Ok(out)
}

fn compiler_identity(
    source: &Path,
    triple: Triple,
    probe: &dyn Fn(&Path) -> Result<String, String>,
) -> R<String> {
    let compiler = below(source, "bin")?.join("trustc");
    regular(&compiler)?;
    let text = probe(&compiler).map_err(Refusal::CompilerIdentity)?;
    let commits: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("commit-hash: "))
        .filter(|c| c.len() == 40 && c.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .collect();
    let hosts: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("host: "))
        .collect();
    if commits.len() == 1 && hosts == [triple.as_str()] {
        Ok(commits[0].to_string())
    } else {
        Err(Refusal::CompilerIdentity(format!(
            "commits {commits:?}, hosts {hosts:?}, packaging {triple}"
        )))
    }
}

fn any_named(dir: &Path, prefix: &str, suffix: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with(prefix) && n.ends_with(suffix)
        })
    })
}

fn require_surface(target: &Path, driver: &Path) -> R<()> {
    for (what, prefix, suffix) in [
        ("std", "libstd-", ".rlib"),
        ("core", "libcore-", ".rlib"),
        ("rustc-private metadata", "librustc_middle-", ".rmeta"),
    ] {
        if !any_named(target, prefix, suffix) {
            return Err(Refusal::MissingSurface(what, target.to_path_buf()));
        }
    }
    let name = driver.file_name().unwrap_or_default();
    if digest(&target.join(name))? == digest(driver)? {
        Ok(())
    } else {
        Err(Refusal::DriverDiffers)
    }
}

fn merge_closure(source: &Path, target: &Path) -> R<()> {
    for entry in files(source)? {
        let rel = entry
            .strip_prefix(source)
            .map_err(|e| Refusal::Io(e.to_string()))?;
        let mut parent = target.to_path_buf();
        if let Some(dirs) = rel.parent() {
            for part in dirs.components() {
                parent.push(part);
                if std::fs::symlink_metadata(&parent).is_err() {
                    std::fs::create_dir(&parent).map_err(|e| io(&parent, &e))?;
                }
                directory(&parent)?;
            }
        }
        let dest = target.join(rel);
        if std::fs::symlink_metadata(&dest).is_ok() {
            if digest(&entry)? != digest(&dest)? {
                return Err(Refusal::Conflict(rel.to_path_buf()));
            }
        } else {
            std::fs::copy(&entry, &dest).map_err(|e| io(&dest, &e))?;
        }
    }
    Ok(())
}

fn digests(root: &Path) -> R<BTreeMap<PathBuf, String>> {
    files(root)?
        .into_iter()
        .map(|p| {
            let d = digest(&p)?;
            Ok((p.strip_prefix(root).unwrap_or(&p).to_path_buf(), d))
        })
        .collect()
}

fn freeze_source(stage: &Path) -> R<()> {
    let source = below(stage, "lib/rustlib/src/rust")?;
    let contents = files(&source)?;
    for component in ["library", "compiler"] {
        let tree = below(&source, component)?;
        if !contents.iter().any(|p| p.starts_with(&tree)) {
            return Err(Refusal::EmptySource(component));
        }
    }
    regular(&source.join("library/core/src/lib.rs"))?;
    regular(&source.join("compiler/rustc_driver/src/lib.rs"))?;
    let alias_parent = below(stage, "lib/rustlib")?.join("rustc-src");
    if std::fs::symlink_metadata(&alias_parent).is_err() {
        std::fs::create_dir(&alias_parent).map_err(|e| io(&alias_parent, &e))?;
    }
    directory(&alias_parent)?;
    let alias = alias_parent.join("rust");
    match std::fs::symlink_metadata(&alias) {
        Ok(m) if m.file_type().is_symlink() => {
            let target = std::fs::read_link(&alias).map_err(|e| io(&alias, &e))?;
            if target != Path::new("../src/rust") {
                return Err(Refusal::AliasNotSealed(alias));
            }
            std::fs::remove_file(&alias).map_err(|e| io(&alias, &e))?;
        }
        Ok(_) => {
            return if digests(&alias)? == digests(&source)? {
                Ok(())
            } else {
                Err(Refusal::AliasDiffers)
            };
        }
        Err(_) => {}
    }
    for file in contents {
        let rel = file
            .strip_prefix(&source)
            .map_err(|e| Refusal::Io(e.to_string()))?;
        let dest = alias.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(parent, &e))?;
        }
        std::fs::hard_link(&file, &dest).map_err(|e| io(&dest, &e))?;
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

fn valid_private_name(n: &str) -> bool {
    let mut b = n.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        && b.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

/// Freezes the sealed closure of `source` into `stage` for `triple` and returns its commit hash.
/// # Errors
/// Returns the first [`Refusal`] encountered without packing `stage`.
pub fn prepare(
    source: &Path,
    stage: &Path,
    triple: Triple,
    probe: &dyn Fn(&Path) -> Result<String, String>,
) -> R<String> {
    let source = std::fs::canonicalize(source).map_err(|e| io(source, &e))?;
    let stage = std::fs::canonicalize(stage).map_err(|e| io(stage, &e))?;
    if source == stage || stage.starts_with(&source) || source.starts_with(&stage) {
        return Err(Refusal::OverlappingRoots);
    }
    let commit = compiler_identity(&source, triple, probe)?;
    let library = below(&stage, "lib")?;
    let mut drivers: Vec<PathBuf> = std::fs::read_dir(&library)
        .map_err(|e| io(&library, &e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("librustc_driver-") && (n.ends_with(".so") || n.ends_with(".dylib"))
            })
        })
        .collect();
    drivers.sort();
    let [driver] = drivers.as_slice() else {
        return Err(Refusal::DriverCount(drivers.len()));
    };
    let driver_name = driver
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let driver_hash = digest(driver)?;
    let target = below(&stage, &format!("lib/rustlib/{triple}/lib"))?;
    let marker = target.join(".rustc-commit-hash");
    if std::fs::symlink_metadata(&marker).is_ok() {
        regular(&marker)?;
        let named = std::fs::read_to_string(&marker).map_err(|e| io(&marker, &e))?;
        if named.trim() != commit {
            return Err(Refusal::MarkerCommit);
        }
        files(&target)?;
        require_surface(&target, driver)?;
    } else {
        let prov = fields(&source.join("PROVENANCE"))?;
        let get = |k: &str| prov.get(k).map_or("", String::as_str);
        if get("reported_commit") != commit {
            return Err(Refusal::SealCommit);
        }
        if get("artifact_driver_name") != driver_name
            || get("artifact_driver_sha256") != driver_hash
        {
            return Err(Refusal::DriverFingerprint);
        }
        let name = get("private_sysroot");
        if !valid_private_name(name) {
            return Err(Refusal::PrivateSysrootName(name.to_string()));
        }
        let parent = source
            .parent()
            .ok_or_else(|| Refusal::NotRealDir(source.clone()))?;
        let private = below(parent, &format!("rustc-private-sysroots/{name}"))?;
        let private_marker = private.join(".rustc-commit-hash");
        regular(&private_marker)?;
        let named =
            std::fs::read_to_string(&private_marker).map_err(|e| io(&private_marker, &e))?;
        if named.trim() != commit {
            return Err(Refusal::PrivateCommit);
        }
        let private_target = below(&private, triple.as_str())?;
        if digest(&private_target.join(&driver_name))? != driver_hash {
            return Err(Refusal::PrivateDriver);
        }
        if !any_named(&target, "libstd-", ".rlib") {
            return Err(Refusal::MissingSurface(
                "selected compiler std",
                target.clone(),
            ));
        }
        merge_closure(&private_target, &target)?;
        merge_closure(&below(&private, "host")?, &target)?;
        require_surface(&target, driver)?;
        std::fs::write(&marker, format!("{commit}\n")).map_err(|e| io(&marker, &e))?;
    }
    let target_driver = target.join(&driver_name);
    if !same_file(&target_driver, driver) {
        std::fs::remove_file(&target_driver).map_err(|e| io(&target_driver, &e))?;
        std::fs::hard_link(driver, &target_driver).map_err(|e| io(&target_driver, &e))?;
    }
    freeze_source(&stage)?;
    Ok(commit)
}
