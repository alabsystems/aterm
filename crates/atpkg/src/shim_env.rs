// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Managed program shim environment ([`crate::vendor_direct`], [`crate::selfupdate`],
//! [`ShimEnv::admit`], [`ShimEnv::fix_line`], [`MAX_SHIM_ENV`], [`MAX_ENTRY_BYTES`],
//! [`NEVER_SET`], [`NEVER_SET_PREFIXES`]).

use std::io;
use std::path::{Path, PathBuf};

/// The most entries a manifest may declare.
pub const MAX_SHIM_ENV: usize = 8;

/// The longest `NAME=VALUE` entry a manifest may declare, in bytes.
pub const MAX_ENTRY_BYTES: usize = 256;

/// Names a shim never sets, refused at parse.
pub const NEVER_SET: &[&str] = &[
    "PATH",
    "HOME",
    "IFS",
    "ENV",
    "BASH_ENV",
    "SHELLOPTS",
    "SHELL",
    "PWD",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "PATHEXT",
    "COMSPEC",
    "SYSTEMROOT",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PERL5OPT",
    "RUBYOPT",
];

/// Name prefixes a shim never sets.
pub const NEVER_SET_PREFIXES: &[&str] = &["LD_", "DYLD_"];

/// Vendor self-update disable variables and their required values ([`turns_updater_off`]).
pub const SELF_UPDATE_SWITCHES: &[(&str, Option<&str>)] = &[
    ("DISABLE_AUTOUPDATER", None),
    ("DISABLE_UPDATES", None),
    ("AGY_CLI_DISABLE_AUTO_UPDATE", Some("true")),
    ("GROK_DISABLE_AUTOUPDATER", Some("1")),
    ("KIMI_CODE_NO_AUTO_UPDATE", Some("1")),
];

/// Whether `name=value` turns its tool's own updater off ([`SELF_UPDATE_SWITCHES`]).
#[must_use]
pub fn turns_updater_off(name: &str, value: &str) -> bool {
    SELF_UPDATE_SWITCHES
        .iter()
        .any(|(n, off)| *n == name && off.is_none_or(|v| v == value))
}

/// The sidecar file name suffix: `store/<program>/<build>.shim-env`.
pub(crate) const SIDECAR_SUFFIX: &str = ".shim-env";

/// Bound for reading a sidecar or a shim back: eight short lines, generously.
const MAX_SIDECAR_BYTES: usize = 4 * 1024;

/// A validated shim environment: `(NAME, VALUE)` pairs in manifest order ([`ShimEnv::admit`]).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ShimEnv(Vec<(String, String)>);

impl ShimEnv {
    /// No environment: the shim of every manifest without `shim_env`.
    pub const NONE: Self = Self(Vec::new());

    /// Validate `raw` (`shim_env` entries) or return the [`crate::sig::Reject::ShimEnv`] error.
    ///
    /// # Errors
    /// More than [`MAX_SHIM_ENV`] entries, or an entry that breaks the rule.
    pub fn admit(raw: &[String]) -> Result<Self, String> {
        if raw.len() > MAX_SHIM_ENV {
            let mut m = String::from("shim_env: ");
            m.push_str(&raw.len().to_string());
            m.push_str(" entries, at most ");
            m.push_str(&MAX_SHIM_ENV.to_string());
            m.push_str(" allowed");
            return Err(m);
        }
        let mut out: Vec<(String, String)> = Vec::with_capacity(raw.len());
        for entry in raw {
            let (name, value) = split_entry(entry)?;
            if out.iter().any(|(n, _)| n == name) {
                return Err(refuse(entry, "duplicate name"));
            }
            out.push((name.to_string(), value.to_string()));
        }
        Ok(Self(out))
    }

    /// Whether this is [`ShimEnv::NONE`].
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The admitted `(NAME, VALUE)` pairs, in manifest order.
    #[must_use]
    pub fn entries(&self) -> &[(String, String)] {
        &self.0
    }

    /// `NAME=VALUE, NAME2=VALUE2` — the entries as one clause.
    #[must_use]
    pub fn spelled(&self) -> String {
        let mut s = String::new();
        for (i, (n, v)) in self.0.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(n);
            s.push('=');
            s.push_str(v);
        }
        s
    }

    /// Format the `which`/`doctor` note for `program`'s shim env ([`turns_updater_off`]).
    #[must_use]
    pub fn fix_line(&self, program: &str) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        let spell = |entries: &[&(String, String)]| {
            entries
                .iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (updater, other): (Vec<_>, Vec<_>) =
            self.0.iter().partition(|(n, v)| turns_updater_off(n, v));
        if updater.is_empty() {
            return Some(format!("runs with {}", self.spelled()));
        }
        let mut s = match crate::vendor_direct::spec(program) {
            Some(spec) => format!("{}'s own updater is off here (", spec.product),
            None => String::from("its own updater is off here ("),
        };
        s.push_str(&spell(&updater));
        s.push(')');
        if !other.is_empty() {
            s.push_str("; also runs with ");
            s.push_str(&spell(&other));
        }
        Some(s)
    }

    /// The sidecar body: one `NAME=VALUE` per line, LF-terminated; empty for [`ShimEnv::NONE`].
    #[must_use]
    pub fn to_lines(&self) -> String {
        let mut s = String::new();
        for (n, v) in &self.0 {
            s.push_str(n);
            s.push('=');
            s.push_str(v);
            s.push('\n');
        }
        s
    }

    /// Parse [`ShimEnv::to_lines`] output fail-closed, returning [`ShimEnv::NONE`] on error.
    #[must_use]
    pub fn from_lines(text: &str) -> Self {
        let raw: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect();
        Self::admit(&raw).unwrap_or_default()
    }
}

/// `NAME=VALUE` → `(NAME, VALUE)` under the module rule, or the refusal naming why.
fn split_entry(entry: &str) -> Result<(&str, &str), String> {
    if entry.len() > MAX_ENTRY_BYTES {
        let mut why = String::from("longer than ");
        why.push_str(&MAX_ENTRY_BYTES.to_string());
        why.push_str(" bytes");
        return Err(refuse(entry, &why));
    }
    let Some((name, value)) = entry.split_once('=') else {
        return Err(refuse(entry, "not NAME=VALUE"));
    };
    if name.is_empty() {
        return Err(refuse(entry, "empty name"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(refuse(entry, "name is not [A-Z0-9_]+"));
    }
    if name.as_bytes()[0].is_ascii_digit() {
        return Err(refuse(
            entry,
            "name is digit-led (not a valid sh identifier)",
        ));
    }
    if NEVER_SET.contains(&name) || NEVER_SET_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Err(refuse(
            entry,
            "names a variable the shim never sets (the shell's, the loader's)",
        ));
    }
    if value.is_empty() {
        return Err(refuse(entry, "empty value"));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(refuse(entry, "control byte in value"));
    }
    if value.trim() != value {
        // The sidecar and the shim are read back line by line, trimmed: a value with
        // whitespace at either edge would not survive the round trip as declared.
        return Err(refuse(entry, "value starts or ends with whitespace"));
    }
    if value.contains('"') || value.contains('%') {
        return Err(refuse(
            entry,
            "value carries a quote or a percent sign (not embeddable in a .cmd shim)",
        ));
    }
    Ok((name, value))
}

/// `shim_env entry "<entry>": <why>`, the entry in its `Debug` form so a refusal line never
/// carries control bytes raw.
fn refuse(entry: &str, why: &str) -> String {
    let mut m = String::from("shim_env entry ");
    m.push_str(&format!("{entry:?}"));
    m.push_str(": ");
    m.push_str(why);
    m
}

/// `store/<program>/<build>.shim-env` for `build_dir`.
#[must_use]
pub(crate) fn sidecar_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?;
    let name = name.to_str()?;
    let mut marker = String::from(name);
    marker.push_str(SIDECAR_SUFFIX);
    Some(build_dir.with_file_name(marker))
}

/// Record `env` as the environment `build_dir`'s shims export, by temp + rename.
///
/// # Errors
/// The sidecar could not be written, or its parent made ([`crate::store::make_build_parent`]).
pub(crate) fn write_sidecar(build_dir: &Path, env: &ShimEnv) -> io::Result<()> {
    let dest = sidecar_path(build_dir)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "build dir has no name"))?;
    if env.is_empty() {
        match std::fs::remove_file(&dest) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    crate::store::make_build_parent(build_dir)?;
    crate::metadata_io::write_record_atomic(&dest, env.to_lines().as_bytes())
}

/// Read `build_dir`'s sidecar environment, or [`ShimEnv::NONE`] when absent or invalid.
#[must_use]
pub(crate) fn read_sidecar(build_dir: &Path) -> ShimEnv {
    let Some(path) = sidecar_path(build_dir) else {
        return ShimEnv::NONE;
    };
    match crate::metadata_io::read_bounded_regular_utf8(&path, MAX_SIDECAR_BYTES) {
        Ok(text) => ShimEnv::from_lines(&text),
        Err(_) => ShimEnv::NONE,
    }
}

/// The environment the shims of the store build holding `exe` export, from that build's sidecar
/// ([`read_sidecar`]): what a launcher that types `exe` by its own path must give it for it to run
/// as it does under its managed name.
///
/// `exe` is an absolute path inside `<prefix>/store/<program>/<build>/`, or inside that build's
/// laid-aside tree `<build>.noindex` (the path a process image reads as). Any other path (a
/// relative one, or one with a `..` step, among them), or a build with no valid sidecar, is
/// [`ShimEnv::NONE`]. The path is read by its shape alone, never resolved, so the sidecar read is
/// the one beside the build the path names. Its build directory is read as
/// [`crate::ops::store_build_of`] reads one ([`crate::ops::build_dir_name`]), but with no prefix
/// (a launcher holds only the path) and only by a name atpkg writes
/// ([`crate::store::parse_build_name`]), since that name picks the file read.
#[must_use]
pub fn of_store_binary(exe: &Path) -> ShimEnv {
    store_build_of(exe).map_or(ShimEnv::NONE, |build_dir| read_sidecar(&build_dir))
}

/// The `store/<program>/<build>` directory nearest above `exe` ([`of_store_binary`]).
pub(crate) fn store_build_of(exe: &Path) -> Option<PathBuf> {
    let plain = exe.components().all(|c| {
        matches!(
            c,
            std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::Normal(_)
        )
    });
    if !exe.is_absolute() || !plain {
        return None;
    }
    exe.ancestors().skip(1).find_map(|dir| {
        let build = crate::ops::build_dir_name(dir.file_name()?)?;
        crate::store::parse_build_name(build)?;
        let program = dir.parent()?;
        let store = program.parent()?;
        (store.file_name() == Some(std::ffi::OsStr::new(aterm_toolchain::layout::STORE_DIR)))
            .then(|| program.join(build))
    })
}

/// Remove `build_dir`'s sidecar, if any ([`crate::store::discard_build`]'s half).
pub(crate) fn remove_sidecar(build_dir: &Path) {
    if let Some(path) = sidecar_path(build_dir) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
#[path = "shim_env_tests.rs"]
mod tests;
