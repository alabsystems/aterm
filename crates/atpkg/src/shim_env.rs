// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `shim_env` — the environment a MANAGED program's shim exports before it execs the
//! store binary (design S7, `docs/DESIGN-which-copy-runs-2026-08-27.md`; §17.12).
//!
//! # Why
//!
//! A managed vendor tool may carry its own updater. Claude Code's downloads newer
//! versions into `~/.local/share/claude/versions/` that the managed shim never runs —
//! wasted bandwidth and a `claude doctor` complaint — while the signed index re-pin is
//! the only update path atpkg honours. Claude Code honours `DISABLE_AUTOUPDATER=1` (the
//! background check stops; `claude update` still works) and `DISABLE_UPDATES=1` (blocks
//! all). So the signed pkg manifest may declare
//!
//! ```toml
//! shim_env = ["DISABLE_AUTOUPDATER=1"]
//! ```
//!
//! and every shim of that program — primary AND `alab-` alias — exports those variables
//! and execs the store binary. **Only the managed copy gets the env**: a system copy on
//! `PATH` never runs through the shim, so it is never touched (rule 1 of the design doc).
//!
//! # Where it lives
//!
//! * **The signed manifest** ([`crate::manifest::PkgManifest::shim_env`]) — SIGNED
//!   metadata, validated at parse ([`ShimEnv::admit`], [`crate::sig::Reject::ShimEnv`]).
//! * **The shim itself** — the Unix `sh` wrapper carries one `export NAME='VALUE'` line
//!   per entry ahead of its `exec`; the Windows `.cmd` wrapper one `@set "NAME=VALUE"`
//!   line ahead of its `@"<target>" %*`. `resolve_shim` still reads the target off the
//!   exec line, so every sweep that keys on where a shim resolves — prune, undo,
//!   rollback, uninstall, `which`, gc — is unchanged. The env is read BACK off the shim
//!   ([`crate::platform::shim_env_of`]) by the surfaces that say so: `which` and
//!   `doctor` add a trailing fix-line ([`ShimEnv::fix_line`]), never inside the
//!   canonical state.
//! * **A sibling sidecar** `store/<program>/<build>.shim-env` (beside `<build>.ready`,
//!   the same shape as `.provenance`), written from the signed manifest before the
//!   build is staged, so the verbs that hold NO manifest — the transaction's rollback,
//!   the `rollback` verb, `unlink`'s restore — re-lay the shims of a build with the env
//!   its manifest declared, exactly. `store::discard_build` removes it with the tree.
//!
//! # The rule (validated once, at parse — and re-applied fail-closed on every read)
//!
//! At most [`MAX_SHIM_ENV`] entries, each at most [`MAX_ENTRY_BYTES`] long (so the
//! whole wrapper always fits the bounded reads every sweep and sidecar parse go
//! through — an unbounded value would make a shim `resolve_shim` cannot read, and a
//! shim nothing can prune or uninstall); each `NAME=VALUE`; `NAME` in `[A-Z0-9_]+` and
//! not digit-led (`export 1X=…` is not a valid `sh` identifier: the wrapper would fail
//! to exec); `VALUE` non-empty (an empty value is `unset` to `cmd.exe`'s `set "X="` and
//! an empty export to `sh` — two meanings, so neither), no control bytes, and no `"` or
//! `%` (nothing can embed those inertly in a `.cmd`); no duplicate name; and never a
//! name the loader, the shell or a language runtime's own loader reads ([`NEVER_SET`],
//! [`NEVER_SET_PREFIXES`]) — the shim sets a PROGRAM's own switches, not `PATH`,
//! `DYLD_INSERT_LIBRARIES` or `NODE_OPTIONS`. On the Unix wrapper the value is
//! single-quoted by the one rule the target path uses (`'` → `'\''`), so a signed value
//! with spaces, `$`, backticks or quotes is one word to `sh` and never a second command.

use std::io;
use std::path::{Path, PathBuf};

/// The most entries a manifest may declare. Mirrored by `tools/atpkg-publish-lib.sh`'s
/// `ATPKG_SHIM_ENV_MAX`, pinned equal by `crates/atpkg/tests/spec_coherence.rs`.
pub const MAX_SHIM_ENV: usize = 8;

/// The longest `NAME=VALUE` entry a manifest may declare, in bytes. Eight of these plus
/// the wrapper's own lines stay far inside every bounded read of a shim or its sidecar
/// (`MAX_SIDECAR_BYTES`, `platform::MAX_SHIM_BYTES`). Mirrored by
/// `tools/atpkg-publish-lib.sh`'s `ATPKG_SHIM_ENV_ENTRY_MAX`, pinned equal by
/// `crates/atpkg/tests/spec_coherence.rs`.
pub const MAX_ENTRY_BYTES: usize = 256;

/// Names a shim never sets, whatever the manifest says: the shell's own, the ones that
/// select what runs, and the language runtimes' own loader switches (`NODE_OPTIONS`
/// injects code into every Node program — Claude Code is one). A manifest naming one
/// is refused at parse.
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

/// The variables that mean "self-update off" to the vendor tools atpkg manages —
/// Claude Code reads both. An env naming one earns the `self-update off (…)` fix-line;
/// any other env is spelled as `runs with …`.
pub const SELF_UPDATE_SWITCHES: &[&str] = &["DISABLE_AUTOUPDATER", "DISABLE_UPDATES"];

/// The sidecar file name suffix: `store/<program>/<build>.shim-env`.
const SIDECAR_SUFFIX: &str = ".shim-env";

/// Bound for reading a sidecar or a shim back: eight short lines, generously.
const MAX_SIDECAR_BYTES: usize = 4 * 1024;

/// A VALIDATED shim environment: `(NAME, VALUE)` pairs that passed [`ShimEnv::admit`],
/// in manifest order. The only way to build a non-empty one is `admit`, so a writer
/// handed a `ShimEnv` never has to re-check the rule.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ShimEnv(Vec<(String, String)>);

impl ShimEnv {
    /// No environment: the shim every manifest without `shim_env` gets, byte-identical
    /// to the shim written before the key existed.
    pub const NONE: Self = Self(Vec::new());

    /// Admit `raw` (the manifest's `shim_env` list) under the module rule, or name the
    /// first entry that breaks it. The `Err` text is the [`crate::sig::Reject::ShimEnv`]
    /// payload, so it spells the entry and the reason for the publisher's own
    /// `atpkg verify-pkg`.
    ///
    /// # Errors
    /// The list is longer than [`MAX_SHIM_ENV`], or an entry is not `NAME=VALUE` under
    /// the rule (module doc).
    pub fn admit(raw: &[String]) -> Result<Self, String> {
        if raw.len() > MAX_SHIM_ENV {
            let mut m = String::from("shim_env: ");
            m.push_str(&crate::dec_u64(raw.len() as u64));
            m.push_str(" entries, at most ");
            m.push_str(&crate::dec_u64(MAX_SHIM_ENV as u64));
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

    /// The fix-line that rides AFTER a managed row on the surfaces that speak to a
    /// person (`which`, `doctor`): `self-update off (DISABLE_AUTOUPDATER=1)` when an
    /// entry names one of [`SELF_UPDATE_SWITCHES`], else `runs with NAME=VALUE`. `None`
    /// for an empty env. NEVER part of the canonical state string — `status.toml` and
    /// the Packages row carry the state alone, and `state.rs`'s parsers never see this.
    #[must_use]
    pub fn fix_line(&self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        let self_update = self
            .0
            .iter()
            .any(|(n, _)| SELF_UPDATE_SWITCHES.contains(&n.as_str()));
        let mut s = String::from(if self_update {
            "self-update off ("
        } else {
            "runs with "
        });
        s.push_str(&self.spelled());
        if self_update {
            s.push(')');
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

    /// The inverse of [`ShimEnv::to_lines`], FAIL-CLOSED: the lines are re-admitted
    /// through the same rule, and any refusal (a hand-edited sidecar, a truncated
    /// write) reads as [`ShimEnv::NONE`] — a shim laid with no env, never one laid
    /// with a half-parsed one. Blank lines and `#` comments are skipped.
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
        why.push_str(&crate::dec_u64(MAX_ENTRY_BYTES as u64));
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

/// `shim_env entry "<entry>": <why>` — control bytes in the entry are rendered as their
/// escaped `Debug` form so a refusal line never carries them raw.
fn refuse(entry: &str, why: &str) -> String {
    let mut m = String::from("shim_env entry ");
    m.push_str(&format!("{entry:?}"));
    m.push_str(": ");
    m.push_str(why);
    m
}

/// `store/<program>/<build>.shim-env` for `build_dir` — a SIBLING, like `<build>.ready`,
/// so it never perturbs the build's `tree_root`. `None` when `build_dir` has no final
/// component (never, for a real build dir).
#[must_use]
pub(crate) fn sidecar_path(build_dir: &Path) -> Option<PathBuf> {
    let name = crate::call1(std::path::Path::file_name, build_dir)?;
    let name = crate::call1(std::ffi::OsStr::to_str, name)?;
    let mut marker = String::from(name);
    marker.push_str(SIDECAR_SUFFIX);
    Some(build_dir.with_file_name(marker))
}

/// Record `env` as the environment `build_dir`'s shims export — temp + rename, so a
/// crash leaves no half-written sidecar. An EMPTY env removes any sidecar a previous
/// occupant of this build number left, so a reinstall never inherits a stale one.
///
/// # Errors
/// The sidecar could not be written (its parent — `store/<program>/` — does not exist,
/// or the volume refuses).
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
    std::fs::create_dir_all(parent)?;
    let mut tmp_name = String::from(".shim-env.tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = parent.join(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    crate::call2(std::fs::write, &tmp, env.to_lines().as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// The environment `build_dir`'s manifest declared, read back off its sidecar —
/// [`ShimEnv::NONE`] when there is none, and (fail-closed) when it does not re-admit.
/// Bounded, symlink-refusing read.
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

/// Remove `build_dir`'s sidecar, if any — [`crate::store::discard_build`]'s half.
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

    /// The manifest validation, entry by entry: the accepted shape, then every refusal
    /// the rule names — each naming the entry.
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
        // The entry bound: exactly the cap is admitted, one byte over is refused by
        // name — and the refusal text does not echo a value that long unbounded either
        // (it is the `Debug` form of the entry; 257 bytes is what the publisher sees).
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

    /// The fix-line spellings: the self-update switches earn `self-update off (…)`,
    /// anything else `runs with …`, an empty env none at all.
    #[test]
    fn the_fix_line_is_exact() {
        assert_eq!(
            ShimEnv::admit(&raw(&["DISABLE_AUTOUPDATER=1"]))
                .unwrap()
                .fix_line()
                .as_deref(),
            Some("self-update off (DISABLE_AUTOUPDATER=1)")
        );
        assert_eq!(
            ShimEnv::admit(&raw(&["DISABLE_UPDATES=1", "FOO=bar"]))
                .unwrap()
                .fix_line()
                .as_deref(),
            Some("self-update off (DISABLE_UPDATES=1, FOO=bar)")
        );
        assert_eq!(
            ShimEnv::admit(&raw(&["FOO=bar"]))
                .unwrap()
                .fix_line()
                .as_deref(),
            Some("runs with FOO=bar")
        );
        assert_eq!(ShimEnv::NONE.fix_line(), None);
    }

    /// The sidecar round-trips, an empty env removes it, a hand-edited one that breaks
    /// the rule reads as NONE (fail-closed), and `remove_sidecar` takes it away.
    #[test]
    fn the_sidecar_round_trips_and_fails_closed() {
        let root = std::env::temp_dir().join(format!("atpkg-shim-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
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
        // An empty env REMOVES a stale sidecar.
        write_sidecar(&build_dir, &ShimEnv::NONE).unwrap();
        assert!(!path.exists());
        assert_eq!(read_sidecar(&build_dir), ShimEnv::NONE);
        write_sidecar(&build_dir, &env).unwrap();
        remove_sidecar(&build_dir);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
