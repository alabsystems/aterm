// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The environment a managed program's shim exports before it execs the store binary
//! (design S7, `docs/DESIGN-which-copy-runs-2026-08-27.md`; §17.12).
//!
//! A managed vendor tool may carry its own updater, which downloads versions the managed
//! shim never runs while aterm keeps the managed copy current ([`crate::vendor_direct`]).
//! A program's policy may therefore declare
//!
//! ```toml
//! shim_env = ["DISABLE_AUTOUPDATER=1"]
//! ```
//!
//! and every shim of that program, primary and `alab-` alias, exports it. Only the managed
//! copy gets the env: a system copy on `PATH` never runs through the shim.
//! (`DISABLE_AUTOUPDATER=1` stops Claude Code's background check; a typed `claude update`
//! on the managed name is [`crate::selfupdate`]'s.)
//!
//! # Where it lives
//!
//! * The policy: the compiled vendor table for a vendor-direct program
//!   ([`crate::vendor_direct::VendorSpec::shim_env`]), the signed manifest for an index
//!   program ([`crate::manifest::PkgManifest::shim_env`]), validated at parse
//!   ([`ShimEnv::admit`], [`crate::sig::Reject::ShimEnv`]).
//! * The shim: one `export NAME='VALUE'` line per entry ahead of the `sh` wrapper's `exec`,
//!   one `@set "NAME=VALUE"` line ahead of the `.cmd` wrapper's target line. `resolve_shim`
//!   still reads the target off the exec line, so every sweep keyed on it is unaffected.
//!   `which` and `doctor` read the env back ([`crate::platform::shim_env_of`]) into a
//!   trailing fix-line ([`ShimEnv::fix_line`]), never into the canonical state.
//! * The sidecar `store/<program>/<build>.shim-env`, beside `<build>.ready`, written before
//!   the build is staged, so verbs that hold no manifest (rollback, `unlink`'s restore,
//!   `repair`) re-lay a build's shims with its declared env. It is also the standard a
//!   laid shim is held to: the pass re-lays a primary that exports otherwise
//!   ([`crate::activate::reassert_shim_env`]), `doctor` names one, and `atpkg run`, which
//!   execs past the shim, exports it itself ([`crate::ops::exec_env`]).
//!
//! # The rule (checked at parse, and again, fail-closed, on every read)
//!
//! At most [`MAX_SHIM_ENV`] entries of at most [`MAX_ENTRY_BYTES`] each, so a wrapper always
//! fits the bounded reads every sweep goes through; each `NAME=VALUE`; `NAME` in
//! `[A-Z0-9_]+` and not digit-led (a valid `sh` identifier); `VALUE` non-empty (`set "X="`
//! unsets in `cmd.exe`), with no control bytes, no edge whitespace and no `"` or `%` (not
//! embeddable in a `.cmd`); no duplicate name; and never a name in [`NEVER_SET`] or
//! [`NEVER_SET_PREFIXES`]: the shim sets a program's own switches, not `PATH` or
//! `DYLD_INSERT_LIBRARIES`. The `sh` wrapper single-quotes the value (`'` → `'\''`), so it
//! is one word to `sh` and never a second command.

use std::io;
use std::path::{Path, PathBuf};

/// The most entries a manifest may declare.
pub const MAX_SHIM_ENV: usize = 8;

/// The longest `NAME=VALUE` entry a manifest may declare, in bytes. Eight of these plus
/// the wrapper's own lines stay far inside every bounded read of a shim or its sidecar
/// (`MAX_SIDECAR_BYTES`, `platform::MAX_SHIM_BYTES`).
pub const MAX_ENTRY_BYTES: usize = 256;

/// Names a shim never sets, refused at parse: the shell's own, the ones that select what
/// runs, and the language runtimes' loader switches (`NODE_OPTIONS` injects code into every
/// Node program, Claude Code among them).
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

/// Name prefixes a shim never sets: the dynamic loader's (`LD_PRELOAD`,
/// `DYLD_INSERT_LIBRARIES`, …).
pub const NEVER_SET_PREFIXES: &[&str] = &["LD_", "DYLD_"];

/// The variables that mean "self-update off" to the vendor tools atpkg manages, each with
/// the one value that means it, or `None` where any value does (agy 1.3.6 reads its switch
/// only as `true`: `1` leaves its updater on). An entry that turns one off
/// ([`turns_updater_off`]) earns the "own updater is off here" fix-line.
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

/// A validated shim environment: `(NAME, VALUE)` pairs in manifest order. Only
/// [`ShimEnv::admit`] builds a non-empty one, so a writer never re-checks the rule.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ShimEnv(Vec<(String, String)>);

impl ShimEnv {
    /// No environment: the shim of every manifest without `shim_env`.
    pub const NONE: Self = Self(Vec::new());

    /// Admit `raw` (the manifest's `shim_env` list) under the module rule, or name the
    /// first entry that breaks it; the text is the [`crate::sig::Reject::ShimEnv`] payload.
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
    pub fn is_empty(&self) -> bool {
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

    /// The fix-line after `program`'s managed row on `which` and `doctor`: `Claude Code's
    /// own updater is off here (DISABLE_AUTOUPDATER=1)` when an entry turns the updater off
    /// ([`turns_updater_off`]; `its own updater` for a program with no vendor row), with any
    /// other variable after it as `; also runs with NAME=VALUE`; else `runs with
    /// NAME=VALUE`; `None` for an empty env. It says nothing about the version, and is
    /// never part of the canonical state (`status.toml`, the Packages row).
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
        // The switches that turn the updater off are the parenthesis; any other variable
        // the shim exports is said after it, not labelled an updater setting.
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

    /// The sidecar body: one `NAME=VALUE` per line, LF-terminated; empty for
    /// [`ShimEnv::NONE`].
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

    /// The inverse of [`ShimEnv::to_lines`], fail-closed: the lines are re-admitted under
    /// the same rule, and any refusal (a hand edit, a truncated write) reads as
    /// [`ShimEnv::NONE`], never a half-parsed env. Blank lines and `#` comments are skipped.
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

/// `store/<program>/<build>.shim-env` for `build_dir`: a sibling, like `<build>.ready`, so
/// it never perturbs the build's `tree_root`. `None` when `build_dir` has no final component.
#[must_use]
pub(crate) fn sidecar_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?;
    let name = name.to_str()?;
    let mut marker = String::from(name);
    marker.push_str(SIDECAR_SUFFIX);
    Some(build_dir.with_file_name(marker))
}

/// Record `env` as the environment `build_dir`'s shims export, by temp + rename. An empty
/// env removes any sidecar a previous occupant of this build number left.
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

/// The environment `build_dir`'s sidecar records: [`ShimEnv::NONE`] when there is none or
/// it does not re-admit. Bounded, symlink-refusing read.
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
        // A value may carry `=` and a single quote (the sh writer escapes it).
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
        // Exactly the cap is admitted; one byte over is refused by name.
        let longest = format!("V={}", "x".repeat(MAX_ENTRY_BYTES - 2));
        assert_eq!(longest.len(), MAX_ENTRY_BYTES);
        assert!(ShimEnv::admit(std::slice::from_ref(&longest)).is_ok());
        let over = format!("{longest}x");
        let err = ShimEnv::admit(&[over]).unwrap_err();
        assert!(err.contains("longer than 256 bytes"), "{err}");
        // Eight entries at the bound still fit every bounded read of the sidecar.
        let eight_long: Vec<String> = (0..8)
            .map(|i| format!("V{i}={}", "x".repeat(MAX_ENTRY_BYTES - 3)))
            .collect();
        let env = ShimEnv::admit(&eight_long).unwrap();
        assert!(env.to_lines().len() < MAX_SIDECAR_BYTES);
        // The cap: nine entries is one too many, and the refusal says so.
        let nine: Vec<String> = (0..9).map(|i| format!("V{i}=1")).collect();
        let err = ShimEnv::admit(&nine).unwrap_err();
        assert_eq!(err, "shim_env: 9 entries, at most 8 allowed");
        let eight: Vec<String> = (0..8).map(|i| format!("V{i}=1")).collect();
        assert_eq!(ShimEnv::admit(&eight).unwrap().entries().len(), 8);
    }

    /// The fix-line spellings: the self-update switches earn "own updater is off here"
    /// (naming the product for a vendor-direct program), anything else `runs with …`, an
    /// empty env none.
    #[test]
    fn the_fix_line_is_exact() {
        assert_eq!(
            ShimEnv::admit(&raw(&["DISABLE_AUTOUPDATER=1"]))
                .unwrap()
                .fix_line("claude")
                .as_deref(),
            Some("Claude Code's own updater is off here (DISABLE_AUTOUPDATER=1)")
        );
        assert_eq!(
            ShimEnv::admit(&raw(&["DISABLE_UPDATES=1", "FOO=bar"]))
                .unwrap()
                .fix_line("tool")
                .as_deref(),
            Some("its own updater is off here (DISABLE_UPDATES=1); also runs with FOO=bar")
        );
        assert_eq!(
            ShimEnv::admit(&raw(&["FOO=bar"]))
                .unwrap()
                .fix_line("claude")
                .as_deref(),
            Some("runs with FOO=bar")
        );
        assert_eq!(ShimEnv::NONE.fix_line("claude"), None);
        // agy's switch is off only at its one value: `1` is no claim about its updater.
        let agy = |entry: &str| ShimEnv::admit(&raw(&[entry])).unwrap().fix_line("agy");
        assert_eq!(
            agy("AGY_CLI_DISABLE_AUTO_UPDATE=true").as_deref(),
            Some("Antigravity CLI's own updater is off here (AGY_CLI_DISABLE_AUTO_UPDATE=true)")
        );
        assert_eq!(
            agy("AGY_CLI_DISABLE_AUTO_UPDATE=1").as_deref(),
            Some("runs with AGY_CLI_DISABLE_AUTO_UPDATE=1")
        );
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
        // Hand-edited into something the rule refuses: NONE, never half of it.
        std::fs::write(&path, "DISABLE_AUTOUPDATER=1\nPATH=/evil\n").unwrap();
        assert_eq!(read_sidecar(&build_dir), ShimEnv::NONE);
        // Comments and blank lines are tolerated.
        std::fs::write(&path, "# note\n\nDISABLE_AUTOUPDATER=1\n").unwrap();
        assert_eq!(read_sidecar(&build_dir).spelled(), "DISABLE_AUTOUPDATER=1");
        // An empty env removes a stale sidecar.
        write_sidecar(&build_dir, &ShimEnv::NONE).unwrap();
        assert!(!path.exists());
        assert_eq!(read_sidecar(&build_dir), ShimEnv::NONE);
        write_sidecar(&build_dir, &env).unwrap();
        remove_sidecar(&build_dir);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
