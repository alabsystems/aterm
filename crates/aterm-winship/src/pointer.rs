// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two records of the Windows owner loop: `BUILD.json`, which
//! `apps\aterm-win\build.ps1` writes into a dist folder, and the staged-build
//! pointer `%LOCALAPPDATA%\aterm\dist\latest.json`, which `update` writes and
//! the installer and the window read.

use std::path::{Path, PathBuf};

use aterm_json::Value;

/// The schema both records carry.
pub const SCHEMA: u64 = 1;

/// A dist folder's `BUILD.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// The workspace version, `0.199.0`.
    pub version: String,
    /// The first twelve hex digits of [`Build::commit_full`].
    pub commit: String,
    /// The commit the build was made from, forty hex digits.
    pub commit_full: String,
    /// The number `aterm --version` prints as `build:`.
    pub build_number: u64,
    /// `x64` or `arm64`.
    pub arch: String,
    /// `stock 1.97.1` or `trust <version>`.
    pub toolchain: String,
    /// Whether the tree had uncommitted changes.
    pub dirty: bool,
    /// When the build finished, RFC 3339.
    pub built_at: String,
}

/// What the smoke said of a staged build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Smoke {
    /// Every row passed.
    Pass,
    /// A row failed.
    Fail,
    /// The smoke could not run.
    NotRun,
}

impl Smoke {
    /// The word the pointer carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotRun => "not-run",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [Self::Pass, Self::Fail, Self::NotRun]
            .into_iter()
            .find(|s| s.word() == word)
    }
}

/// The staged-build pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    /// The staged build's version.
    pub version: String,
    /// The first twelve hex digits of [`Pointer::commit_full`], and the name of
    /// the dist folder.
    pub commit: String,
    /// The staged build's commit, forty hex digits.
    pub commit_full: String,
    /// The staged build's `build:` number.
    pub build_number: u64,
    /// The dist folder, absolute.
    pub dir: PathBuf,
    /// When it was staged, RFC 3339.
    pub staged_at: String,
    /// The smoke's verdict on it.
    pub smoke: Smoke,
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `name` is a dist folder's name: twelve lowercase hex digits.
#[must_use]
pub fn is_commit12(name: &str) -> bool {
    is_hex(name, 12)
}

fn object(text: &str, what: &str) -> Result<Value, String> {
    // A file PowerShell wrote can lead with a byte-order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value: Value = aterm_json::from_str(text).map_err(|e| format!("{what}: not JSON: {e}"))?;
    match value.get("schema").and_then(Value::as_u64) {
        Some(SCHEMA) => Ok(value),
        Some(other) => Err(format!(
            "{what}: schema {other}, this tool reads schema {SCHEMA}"
        )),
        None => Err(format!("{what}: no schema")),
    }
}

fn string(value: &Value, key: &str, what: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{what}: no {key}"))
}

fn commits(value: &Value, what: &str) -> Result<(String, String), String> {
    let commit = string(value, "commit", what)?;
    let full = string(value, "commit_full", what)?;
    if !is_hex(&commit, 12) || !is_hex(&full, 40) || !full.starts_with(&commit) {
        return Err(format!(
            "{what}: commit {commit:?} is not the first twelve hex digits of commit_full {full:?}"
        ));
    }
    Ok((commit, full))
}

fn number(value: &Value, key: &str, what: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{what}: no {key}"))
}

impl Build {
    /// Parses a `BUILD.json`.
    ///
    /// # Errors
    /// Not JSON, another schema, or a field missing or malformed.
    pub fn parse(text: &str) -> Result<Self, String> {
        let what = "BUILD.json";
        let v = object(text, what)?;
        let (commit, commit_full) = commits(&v, what)?;
        Ok(Self {
            version: string(&v, "version", what)?,
            commit,
            commit_full,
            build_number: number(&v, "build_number", what)?,
            arch: string(&v, "arch", what)?,
            toolchain: string(&v, "toolchain", what)?,
            dirty: v
                .get("dirty")
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("{what}: no dirty"))?,
            built_at: string(&v, "built_at", what)?,
        })
    }

    /// Reads `dir`'s `BUILD.json`; `Ok(None)` where the folder has none (a dist
    /// older than the record).
    ///
    /// # Errors
    /// The file is there and does not parse.
    pub fn read(dir: &Path) -> Result<Option<Self>, String> {
        match std::fs::read_to_string(dir.join("BUILD.json")) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", dir.join("BUILD.json").display())),
        }
    }
}

fn quoted(text: &str) -> String {
    // `quoted_string` fails only on a string whose escaped length overflows `usize`.
    aterm_json::quoted_string(text)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| "\"\"".to_string())
}

impl Pointer {
    /// The pointer for `build`, staged in `dir` at `staged_at` with verdict `smoke`.
    #[must_use]
    pub fn of(build: &Build, dir: &Path, staged_at: String, smoke: Smoke) -> Self {
        Self {
            version: build.version.clone(),
            commit: build.commit.clone(),
            commit_full: build.commit_full.clone(),
            build_number: build.build_number,
            dir: dir.to_path_buf(),
            staged_at,
            smoke,
        }
    }

    /// The pointer as `latest.json`'s text, keys in the documented order.
    #[must_use]
    pub fn to_json(&self) -> String {
        format!(
            "{{\"schema\":{SCHEMA},\"version\":{},\"commit\":{},\"commit_full\":{},\"build_number\":{},\"dir\":{},\"staged_at\":{},\"smoke\":{}}}\n",
            quoted(&self.version),
            quoted(&self.commit),
            quoted(&self.commit_full),
            self.build_number,
            quoted(&self.dir.to_string_lossy()),
            quoted(&self.staged_at),
            quoted(self.smoke.word()),
        )
    }

    /// Parses a `latest.json`.
    ///
    /// # Errors
    /// Not JSON, another schema, a field missing or malformed, or a `dir` that
    /// is not absolute.
    pub fn parse(text: &str) -> Result<Self, String> {
        let what = "latest.json";
        let v = object(text, what)?;
        let (commit, commit_full) = commits(&v, what)?;
        let dir = PathBuf::from(string(&v, "dir", what)?);
        if !dir.is_absolute() {
            return Err(format!("{what}: dir {} is not absolute", dir.display()));
        }
        let word = string(&v, "smoke", what)?;
        Ok(Self {
            version: string(&v, "version", what)?,
            commit,
            commit_full,
            build_number: number(&v, "build_number", what)?,
            dir,
            staged_at: string(&v, "staged_at", what)?,
            smoke: Smoke::parse(&word).ok_or_else(|| format!("{what}: smoke {word:?}"))?,
        })
    }

    /// Reads the pointer at `path`; `Ok(None)` where there is none yet.
    ///
    /// # Errors
    /// The file is there and does not parse.
    pub fn read(path: &Path) -> Result<Option<Self>, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Publishes the pointer at `path` whole: a temporary beside it, then a
    /// rename, so a reader finds the old pointer or this one.
    ///
    /// # Errors
    /// The directory could not be made or the write failed; `path` is as it was.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        aterm_uds::atomic_file::write(path, self.to_json().as_bytes(), 0o644)
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// `secs` after the Unix epoch as an RFC 3339 UTC time, `2026-11-11T19:14:22Z`.
#[must_use]
pub fn rfc3339(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days to a civil date in the proleptic Gregorian calendar, eras of 400 years
    // counted from 0000-03-01 so the leap day is an era's last.
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// Now, as [`rfc3339`] writes it.
#[must_use]
pub fn now_rfc3339() -> String {
    rfc3339(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default(),
    )
}

#[cfg(test)]
#[path = "pointer_tests.rs"]
mod tests;
