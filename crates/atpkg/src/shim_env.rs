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
    let parent = dest.parent().unwrap_or(build_dir);
    crate::store::make_build_parent(build_dir)?;
    let mut tmp_name = String::from(".shim-env.tmp-");
    tmp_name.push_str(&std::process::id().to_string());
    let tmp = parent.join(tmp_name);
    crate::store::write_record_temp(&tmp, env.to_lines().as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
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

/// Remove `build_dir`'s sidecar, if any ([`crate::store::discard_build`]'s half).
pub(crate) fn remove_sidecar(build_dir: &Path) {
    if let Some(path) = sidecar_path(build_dir) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|s| (*s).to_string()).collect()
    }

    /// The accepted shape, then every refusal the rule names, each naming the entry.
    #[test]
    fn admit_accepts_the_rule_and_refuses_everything_else_by_name() {
        let env = ShimEnv::admit(&raw(&["DISABLE_AUTOUPDATER=1"])).unwrap();
        assert_eq!(
            env.entries(),
            &[("DISABLE_AUTOUPDATER".to_string(), "1".to_string())]
        );
        assert!(!env.is_empty());
        assert!(ShimEnv::admit(&[]).unwrap().is_empty());
        let env = ShimEnv::admit(&raw(&["A_1=x=y", "B=it's fine"])).unwrap();
        assert_eq!(env.spelled(), "A_1=x=y, B=it's fine");

        let refusals: &[(&[&str], &str)] = &[
            (&["DISABLE_AUTOUPDATER"], "not NAME=VALUE"),
            (&["=1"], "empty name"),
            (&["disable_autoupdater=1"], "name is not [A-Z0-9_]+"),
            (&["DISABLE-AUTOUPDATER=1"], "name is not [A-Z0-9_]+"),
            (&["1X=1"], "digit-led"),
            (&["PATH=/tmp"], "never sets"),
            (&["BASH_ENV=/tmp/rc"], "never sets"),
            (&["SHELLOPTS=xtrace"], "never sets"),
            (&["NODE_OPTIONS=--require /x.js"], "never sets"),
            (&["PYTHONPATH=/x"], "never sets"),
            (&["DYLD_INSERT_LIBRARIES=/x.dylib"], "never sets"),
            (&["LD_PRELOAD=/x.so"], "never sets"),
            (&["X="], "empty value"),
            (&["X=a\nb"], "control byte"),
            (&["X=a\u{7f}"], "control byte"),
            (&["X=a "], "starts or ends with whitespace"),
            (&["X= a"], "starts or ends with whitespace"),
            (&["X=\"1\""], "quote or a percent"),
            (&["X=%TEMP%"], "quote or a percent"),
            (&["X=1", "X=2"], "duplicate name"),
        ];
        for (entries, why) in refusals {
            let err = ShimEnv::admit(&raw(entries)).unwrap_err();
            assert!(err.contains(why), "{entries:?}: {err}");
            assert!(
                err.starts_with("shim_env entry "),
                "the refusal names the entry: {err}"
            );
        }
        let longest = format!("V={}", "x".repeat(MAX_ENTRY_BYTES - 2));
        assert_eq!(longest.len(), MAX_ENTRY_BYTES);
        assert!(ShimEnv::admit(std::slice::from_ref(&longest)).is_ok());
        let over = format!("{longest}x");
        let err = ShimEnv::admit(&[over]).unwrap_err();
        assert!(err.contains("longer than 256 bytes"), "{err}");
        let eight_long: Vec<String> = (0..8)
            .map(|i| format!("V{i}={}", "x".repeat(MAX_ENTRY_BYTES - 3)))
            .collect();
        let env = ShimEnv::admit(&eight_long).unwrap();
        assert!(env.to_lines().len() < MAX_SIDECAR_BYTES);
        let nine: Vec<String> = (0..9).map(|i| format!("V{i}=1")).collect();
        assert!(ShimEnv::admit(&nine).is_err());
        let eight: Vec<String> = (0..8).map(|i| format!("V{i}=1")).collect();
        assert_eq!(ShimEnv::admit(&eight).unwrap().entries().len(), 8);
    }

    /// The sidecar round-trips, an empty env removes it, a hand-edited one that breaks the
    /// rule reads as NONE, and `remove_sidecar` takes it away.
    #[test]
    fn the_sidecar_round_trips_and_fails_closed() {
        let root = crate::testdir::dir("atpkg-shim-env-");
        let build_dir = root.join("store").join("claude").join("2026082701");
        std::fs::create_dir_all(&build_dir).unwrap();
        let env = ShimEnv::admit(&raw(&["DISABLE_AUTOUPDATER=1", "B=2"])).unwrap();
        write_sidecar(&build_dir, &env).unwrap();
        let path = sidecar_path(&build_dir).unwrap();
        assert_eq!(
            path,
            root.join("store")
                .join("claude")
                .join("2026082701.shim-env"),
            "a SIBLING of the build dir, like <build>.ready"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "DISABLE_AUTOUPDATER=1\nB=2\n"
        );
        assert_eq!(read_sidecar(&build_dir), env);
        std::fs::write(&path, "DISABLE_AUTOUPDATER=1\nPATH=/evil\n").unwrap();
        assert_eq!(read_sidecar(&build_dir), ShimEnv::NONE);
        std::fs::write(&path, "# note\n\nDISABLE_AUTOUPDATER=1\n").unwrap();
        assert_eq!(read_sidecar(&build_dir).spelled(), "DISABLE_AUTOUPDATER=1");
        write_sidecar(&build_dir, &ShimEnv::NONE).unwrap();
        assert!(!path.exists());
        assert_eq!(read_sidecar(&build_dir), ShimEnv::NONE);
        write_sidecar(&build_dir, &env).unwrap();
        remove_sidecar(&build_dir);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
