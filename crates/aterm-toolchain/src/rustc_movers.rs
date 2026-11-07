// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT MOVES A CARGO RUN OFF THE `trustc` BESIDE ITS `targo`, in cargo's own order: `$RUSTC`,
//! `$CARGO_BUILD_RUSTC`, then `build.rustc` in each directory's `.cargo/config` (or
//! `.cargo/config.toml`) from the run's directory up, nearest first, then `$CARGO_HOME`'s. The
//! first one set is the compiler every compile child of the run takes ([`Mover::resolve`] finds it
//! as cargo does). `aterm help rust` lists them; verify and `atpkg-publish adopt` hold the first to
//! a tree's `trust.lock` ([`lock_refusal`]), since a compiler beside the locked `targo` is only
//! the lock's when nothing names another.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::trust_lock::TrustLock;

/// The variables that name a cargo run's compiler, in cargo's precedence.
pub const RUSTC_VARS: [&str; 2] = ["RUSTC", "CARGO_BUILD_RUSTC"];

/// The most of a cargo config file this reads.
const MAX_CONFIG_BYTES: usize = 1 << 20;

/// One setting that names the compiler a cargo run takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mover {
    /// Where it is set: `$RUSTC`, `$CARGO_BUILD_RUSTC`, or `build.rustc in <file>`.
    pub what: String,
    /// Its value, verbatim.
    pub value: String,
    /// What a value with a path separator is relative to: the run's directory for a variable,
    /// the directory that holds a config file's directory for a file (cargo's config-relative
    /// paths).
    pub base: PathBuf,
}

impl Mover {
    /// The compiler it names, found as cargo finds it in a child whose `PATH` is `path_env`: a
    /// bare name on that `PATH`, a value with a separator joined to [`Mover::base`]. `None` when
    /// no executable is there.
    #[must_use]
    pub fn resolve(&self, path_env: &OsStr) -> Option<PathBuf> {
        if self.value.chars().any(std::path::is_separator) {
            let path = self.base.join(&self.value);
            return crate::discovery::is_executable_file(&path).then_some(path);
        }
        std::env::split_paths(path_env).find_map(|dir| {
            [
                dir.join(&self.value),
                dir.join(crate::discovery::tool_file(&self.value)),
            ]
            .into_iter()
            .find(|path| crate::discovery::is_executable_file(path))
        })
    }

    /// What takes it away: the variable to unset, or the file to drop `build.rustc` from.
    #[must_use]
    pub fn fix(&self) -> String {
        match self.what.strip_prefix('$') {
            Some(var) => format!("unset {var}"),
            None => format!("remove {}", self.what.replacen(" in ", " from ", 1)),
        }
    }
}

/// What names the compiler of a cargo run in `cwd` ([`RUSTC_VARS`] through `var`, then the
/// config files up from `cwd`, then `cargo_home`'s), in cargo's precedence: the first is the one
/// the run takes. A directory holding both `config` and `config.toml` is read for `config`, as
/// cargo reads it; each file is read once however it is reached.
#[must_use]
pub fn rustc_movers(
    cwd: Option<&Path>,
    var: &dyn Fn(&str) -> Option<String>,
    cargo_home: Option<&Path>,
) -> Vec<Mover> {
    let mut found = Vec::new();
    for key in RUSTC_VARS {
        if let Some(value) = var(key).filter(|v| !v.is_empty()) {
            found.push(Mover {
                what: format!("${key}"),
                value,
                base: cwd.map(Path::to_path_buf).unwrap_or_default(),
            });
        }
    }
    let mut dirs: Vec<PathBuf> = cwd
        .into_iter()
        .flat_map(Path::ancestors)
        .map(|d| d.join(".cargo"))
        .collect();
    dirs.extend(cargo_home.map(Path::to_path_buf));
    let mut seen = std::collections::BTreeSet::new();
    for dir in dirs {
        let Some(file) = ["config", "config.toml"]
            .into_iter()
            .map(|name| dir.join(name))
            .find(|file| file.is_file())
        else {
            continue;
        };
        if !seen.insert(file.clone()) {
            continue;
        }
        if let Some(value) = crate::discovery::read_bounded_text(&file, MAX_CONFIG_BYTES)
            .as_deref()
            .and_then(build_rustc_in)
        {
            found.push(Mover {
                what: format!("build.rustc in {}", file.display()),
                value,
                base: dir.parent().unwrap_or(&dir).to_path_buf(),
            });
        }
    }
    found
}

/// The `rustc` key of a cargo config's `[build]` table (or a top-level dotted `build.rustc`),
/// verbatim between its quotes; the last one wins.
#[must_use]
pub fn build_rustc_in(text: &str) -> Option<String> {
    let mut table = String::new();
    let mut found = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            table = header
                .split(']')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !((table == "build" && key == "rustc") || (table.is_empty() && key == "build.rustc")) {
            continue;
        }
        let value = value.trim_start();
        let Some(quote) = value.chars().next().filter(|q| *q == '"' || *q == '\'') else {
            continue;
        };
        if let Some(end) = value[1..].find(quote) {
            let inner = &value[1..=end];
            if !inner.is_empty() {
                found = Some(inner.to_string());
            }
        }
    }
    found
}

/// THE COMPILER A CARGO RUN TAKES, HELD TO A LOCK (A16): `None` when nothing names one (the run
/// compiles with the `trustc` beside its `targo`) or when the first of `movers`, resolved on the
/// child's `path_env`, answers `lock`'s `compiler_commit`; otherwise why, naming the setting,
/// what it resolved to and what takes it away.
#[must_use]
pub fn lock_refusal(
    movers: &[Mover],
    path_env: &OsStr,
    lock_path: &Path,
    lock: &TrustLock,
) -> Option<String> {
    let winner = movers.first()?;
    let resolved = winner.resolve(path_env);
    let reported = resolved
        .as_deref()
        .and_then(crate::trust_lock::compiler_commit_of);
    if reported.as_deref() == Some(lock.compiler_commit.as_str()) {
        return None;
    }
    let answers = match (&resolved, &reported) {
        (None, _) => "names no executable".to_string(),
        (Some(path), None) => format!("({}) answers no commit-hash", path.display()),
        (Some(path), Some(commit)) => format!("({}) answers commit {commit}", path.display()),
    };
    Some(format!(
        "{} = {} {answers}, and {} names compiler commit {}: every compile child would build \
         with it. Fix: {}",
        winner.what,
        winner.value,
        lock_path.display(),
        lock.compiler_commit,
        winner.fix()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build.rustc` is read the way cargo's config names it: under `[build]`, or dotted at the
    /// top level; never a wrapper, never another table's key.
    #[test]
    fn build_rustc_is_read_from_the_build_table_only() {
        assert_eq!(
            build_rustc_in("[build]\nrustc = \"/x/rustc\"\n").as_deref(),
            Some("/x/rustc")
        );
        assert_eq!(
            build_rustc_in("build.rustc = '/y/rustc' # stock\n").as_deref(),
            Some("/y/rustc")
        );
        assert_eq!(
            build_rustc_in("[build]\nrustc = \"/a/b#c\" # a comment\n").as_deref(),
            Some("/a/b#c"),
            "a # inside the quotes is the value's"
        );
        for none in [
            "[build]\nrustc-wrapper = \"sccache\"\n",
            "[target.aarch64-apple-darwin]\nrustc = \"/z\"\n",
            "# [build]\n# rustc = \"/z\"\n",
            "[build]\nrustflags = [\"-Ztrust-verify=off\"]\n",
            "[build]\nrustc = \"\"\n",
            "",
        ] {
            assert_eq!(build_rustc_in(none), None, "{none:?}");
        }
    }

    /// What can move targo off trustc here, in cargo's precedence: `$RUSTC`, then
    /// `$CARGO_BUILD_RUSTC`, then the nearest config file's `build.rustc`, then `$CARGO_HOME`'s,
    /// each file read once however it is reached, `config` over `config.toml` in one directory.
    #[test]
    fn what_moves_targo_off_trustc_is_read_in_cargos_order() {
        let hold = aterm_tempfile::tempdir().expect("scratch");
        let root = hold.path().canonicalize().expect("canonical");
        let repo = root.join("repo");
        let deep = repo.join("crates").join("x");
        let home = root.join("cargo-home");
        for dir in [&deep, &repo.join(".cargo"), &home] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        let none = |_: &str| None;
        assert!(
            rustc_movers(Some(&deep), &none, Some(&home)).is_empty(),
            "nothing set, nothing moves it"
        );
        std::fs::write(
            home.join("config.toml"),
            "[build]\nrustc = \"/home/rustc\"\n",
        )
        .expect("write");
        std::fs::write(
            repo.join(".cargo").join("config.toml"),
            "[build]\nrustc = \"bin/rustc\"\n",
        )
        .expect("write");
        let env = |k: &str| (k == "RUSTC").then(|| "/env/rustc".to_string());
        let got = rustc_movers(Some(&deep), &env, Some(&home));
        let said: Vec<String> = got
            .iter()
            .map(|m| format!("{}={}", m.what, m.value))
            .collect();
        assert_eq!(
            said,
            [
                "$RUSTC=/env/rustc".to_string(),
                format!(
                    "build.rustc in {}=bin/rustc",
                    repo.join(".cargo/config.toml").display()
                ),
                format!(
                    "build.rustc in {}=/home/rustc",
                    home.join("config.toml").display()
                ),
            ]
        );
        assert_eq!(got[0].base, deep, "a variable is relative to the run");
        assert_eq!(
            got[1].base, repo,
            "a file to the directory above its `.cargo`"
        );
        assert_eq!(got[0].fix(), "unset RUSTC");
        assert_eq!(
            got[1].fix(),
            format!(
                "remove build.rustc from {}",
                repo.join(".cargo/config.toml").display()
            )
        );
        // An empty variable is unset.
        let empty = |k: &str| (k == "RUSTC").then(String::new);
        assert_eq!(rustc_movers(Some(&deep), &empty, Some(&home)).len(), 2);
        // `config` is the file cargo reads where both are.
        std::fs::write(
            repo.join(".cargo").join("config"),
            "[build]\nrustc = \"/plain/rustc\"\n",
        )
        .expect("write");
        assert_eq!(
            rustc_movers(Some(&deep), &none, Some(&home))[0].value,
            "/plain/rustc"
        );
        // $CARGO_HOME reached twice, as an ancestor's `.cargo` and as itself, is one row.
        let nested_home = repo.join(".cargo");
        assert_eq!(
            rustc_movers(Some(&deep), &none, Some(&nested_home)).len(),
            1
        );
    }

    /// A mover resolves as cargo resolves it in the child: a bare name on the child's `PATH`, a
    /// path with a separator against its base; and only a first mover that answers the lock's
    /// commit leaves the run on the lock's compiler.
    #[cfg(unix)]
    #[test]
    fn the_first_mover_is_held_to_the_lock() {
        let hold = aterm_tempfile::tempdir().expect("scratch");
        let root = hold.path().canonicalize().expect("canonical");
        let rustc = |dir: &Path, commit: &str| {
            std::fs::create_dir_all(dir).expect("mkdir");
            let path = dir.join("rustc");
            crate::test_script::script(
                &path,
                &format!("#!/bin/sh\nprintf 'commit-hash: {commit}\\n'\n"),
            );
            path
        };
        let locked = "a".repeat(40);
        let other = "b".repeat(40);
        let on_path = root.join("locked-bin");
        rustc(&on_path, &locked);
        rustc(&root.join("repo/bin"), &other);
        let lock = TrustLock {
            build: 16387,
            tree_root: "4".repeat(64),
            compiler_commit: locked.clone(),
            receipt: "SEED-MINT sha256:00".to_string(),
            stock_toolchain: "1.99.1".to_string(),
        };
        let lock_path = root.join("repo/trust.lock");
        let path_env = on_path.as_os_str();
        let mover = |what: &str, value: &str| Mover {
            what: what.to_string(),
            value: value.to_string(),
            base: root.join("repo"),
        };

        assert_eq!(lock_refusal(&[], path_env, &lock_path, &lock), None);
        assert_eq!(
            lock_refusal(&[mover("$RUSTC", "rustc")], path_env, &lock_path, &lock),
            None,
            "a bare name is the child's PATH's: the locked build's own"
        );
        let why = lock_refusal(&[mover("$RUSTC", "bin/rustc")], path_env, &lock_path, &lock)
            .expect("a relative path is the base's: another compiler");
        assert!(
            why.contains(&format!("answers commit {other}")) && why.contains("Fix: unset RUSTC"),
            "{why}"
        );
        let why = lock_refusal(
            &[
                mover("build.rustc in /x/.cargo/config.toml", "/nowhere/rustc"),
                mover("$RUSTC", "rustc"),
            ],
            path_env,
            &lock_path,
            &lock,
        )
        .expect("the first decides");
        assert!(
            why.contains("names no executable")
                && why.contains("Fix: remove build.rustc from /x/.cargo/config.toml"),
            "{why}"
        );
    }
}
