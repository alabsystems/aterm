// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUILD-CACHE RULE: where plain `targo` puts a workspace's build intermediates.
//!
//! On macOS atpkg's `targo`/`targo-tippy` shims (and `cargo` in an aterm tab) add
//! `--config build.build-dir=<cache><workspace>/.cargo-build` to a command that builds
//! when nothing else chose a target or build directory: binaries stay in `target/`, the
//! rest goes under `~/Library/Caches/aterm/cargo-build.noindex`, which Spotlight skips.
//! [`with_build_dir`] is that decision as a pure function of the command line and an
//! injected [`Machine`]; atpkg binds it to this process (`atpkg::build_cache::
//! with_build_dir_here`) and renders the same rule in `sh` for the shim, and
//! `aterm-verify` binds it for the children it starts itself, so a verify run and a plain
//! `targo` build share one cache.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The cache root, relative to `$HOME`.
pub const CACHE_UNDER_HOME: &str = "Library/Caches/aterm/cargo-build.noindex";

/// The last component of every workspace's build directory under the cache.
pub const LEAF: &str = ".cargo-build";

/// The subcommands that get a build directory: those that build, and `clean`, which
/// must see the same layout to remove it.
pub const BUILD_VERBS: &[&str] = &[
    "build", "b", "check", "c", "test", "t", "run", "r", "bench", "doc", "d", "rustc", "rustdoc",
    "clippy", "tippy", "fix", "clean",
];

/// `$HOME/Library/Caches/aterm/cargo-build.noindex`.
#[must_use]
pub fn cache_root(home: &Path) -> PathBuf {
    home.join(CACHE_UNDER_HOME)
}

/// The build directory Cargo resolves for the workspace at `workspace` (absolute).
#[must_use]
pub fn build_dir(root: &Path, workspace: &Path) -> PathBuf {
    let mut s = root.as_os_str().to_owned();
    s.push(workspace.as_os_str());
    PathBuf::from(s).join(LEAF)
}

/// The `--config` value both entry points pass, as a TOML literal string. `None` for a
/// root that string cannot hold (a `'` or a control character) or that Cargo would read
/// as a template (a `{` or `}`); the shim refuses the same roots.
#[must_use]
pub fn config_value(root: &Path) -> Option<String> {
    let root = root.to_str()?;
    if root
        .chars()
        .any(|c| matches!(c, '\'' | '{' | '}') || c.is_control())
    {
        return None;
    }
    Some(format!("build.build-dir='{root}{{workspace-root}}/{LEAF}'"))
}

/// Which tool reads the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Trust's `targo` (or Trust's own `cargo`, which is the same program): a build needs
    /// `--unverified`; the verified lane is left alone.
    Targo,
    /// `targo-tippy`, the lint: every argument is its own, and it takes no lane.
    Tippy,
    /// An upstream `cargo`: it has no lane flag, and a leading `+<toolchain>` is rustup's
    /// and stays first.
    Cargo,
}

/// What [`with_build_dir`] reads of the machine, injected so every rule is tested
/// without touching the real one.
pub struct Machine<'a> {
    /// `$HOME`.
    pub home: Option<&'a Path>,
    /// The working directory, as the kernel names it (no symlinks).
    pub cwd: Option<&'a Path>,
    /// One environment variable.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    /// A config file's text: `Ok(None)` when there is no such file, `Err` when one exists
    /// and cannot be read.
    pub read: &'a dyn Fn(&Path) -> Result<Option<String>, ()>,
    /// Whether this process may write the directory (`access(2)`, which a sandbox's
    /// denial answers too); `None` when it is not a directory.
    pub writable: &'a dyn Fn(&Path) -> Option<bool>,
}

/// `args` with `--config build.build-dir=…` added, or `None` when the command keeps its
/// own layout (atpkg's shim block is the same rule in `sh`).
#[must_use]
pub fn with_build_dir(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<Vec<String>> {
    let home = m
        .home
        .filter(|h| h.is_absolute() && !h.as_os_str().as_encoded_bytes().ends_with(b"/"))?;
    let cwd = m.cwd?;
    let root = cache_root(home);
    let value = config_value(&root)?;
    if !cwd.starts_with(home) || cwd == home || cwd.starts_with(&root) || has_brace(cwd) {
        return None;
    }
    // A `$PWD` that is not the real folder reached it through a link: the shim sees only
    // `$PWD`, so both halves leave it alone.
    if (m.env)("PWD").is_some_and(|p| Path::new(&p) != cwd) {
        return None;
    }
    let chosen = [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
    ];
    if chosen.iter().any(|v| (m.env)(v).is_some()) {
        return None;
    }
    let at = usize::from(tool == Tool::Cargo && args.first().is_some_and(|a| a.starts_with('+')));
    if !command_takes_it(&args[at..], tool, home, &root) {
        return None;
    }
    if cwd
        .ancestors()
        .any(|d| config_chooses(&d.join(".cargo"), m))
    {
        return None;
    }
    let cargo_home = (m.env)("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".cargo"), PathBuf::from);
    if config_chooses(&cargo_home, m) {
        return None;
    }
    let parent = root.parent()?;
    let can_write = (m.writable)(&root)
        .or_else(|| (m.writable)(parent))
        .or_else(|| (m.writable)(&home.join("Library/Caches")))
        .unwrap_or(false);
    if !can_write {
        return None;
    }
    let mut out = Vec::with_capacity(args.len() + 2);
    out.extend_from_slice(&args[..at]);
    out.push("--config".to_owned());
    out.push(value);
    out.extend_from_slice(&args[at..]);
    Some(out)
}

/// Whether a path holds a `{` or `}`, which Cargo would read as a template once
/// `{workspace-root}` is expanded.
fn has_brace(p: &Path) -> bool {
    p.as_os_str()
        .as_encoded_bytes()
        .iter()
        .any(|b| matches!(b, b'{' | b'}'))
}

/// The verb and flags half of the rule: a building verb (for `targo`, with
/// `--unverified`; `clean` and the lint take no lane), and nothing on the line before
/// `--` that names its own layout.
fn command_takes_it(args: &[String], tool: Tool, home: &Path, root: &Path) -> bool {
    let config_ok = |v: &str| v.contains('=') && !names_a_dir(v);
    let manifest_ok = |v: &str| {
        let p = Path::new(v);
        !v.is_empty()
            && !v.contains("..")
            && !has_brace(p)
            && !p.starts_with(root)
            && (!v.starts_with('/') || p.starts_with(home))
    };
    let mut verb = (tool == Tool::Tippy).then_some("tippy");
    let mut unverified = false;
    let mut it = args.iter().map(String::as_str);
    while let Some(a) = it.next() {
        let ok = match a {
            "--" => break,
            "--unverified" => {
                unverified = true;
                true
            }
            "--config" => it.next().is_some_and(config_ok),
            "--manifest-path" => it.next().is_some_and(manifest_ok),
            "-Z" | "--color" | "--explain" => verb.is_some() || it.next().is_some(),
            _ => {
                if let Some(v) = a.strip_prefix("--config=") {
                    config_ok(v)
                } else if let Some(v) = a.strip_prefix("--manifest-path=") {
                    manifest_ok(v)
                } else if a == "--target-dir"
                    || a.starts_with("--target-dir=")
                    || a.starts_with("-C")
                {
                    false
                } else {
                    if !a.starts_with('-') && verb.is_none() {
                        verb = Some(a);
                    }
                    true
                }
            }
        };
        if !ok {
            return false;
        }
    }
    match verb {
        Some("clean" | "tippy" | "clippy") => true,
        Some(v) if BUILD_VERBS.contains(&v) => tool == Tool::Cargo || unverified,
        _ => false,
    }
}

/// Whether a config value or line names a target or build directory.
fn names_a_dir(s: &str) -> bool {
    s.contains("target-dir") || s.contains("build-dir")
}

/// Whether `dir/config.toml` or `dir/config` chooses a layout: a line naming
/// `target-dir` or `build-dir` that is not a comment. A file that exists and cannot be
/// read counts as a choice.
fn config_chooses(dir: &Path, m: &Machine<'_>) -> bool {
    ["config.toml", "config"]
        .iter()
        .any(|name| match (m.read)(&dir.join(name)) {
            Err(()) => true,
            Ok(None) => false,
            Ok(Some(text)) => text.lines().any(|line| {
                names_a_dir(line) && !line.trim_start_matches([' ', '\t']).starts_with('#')
            }),
        })
}

/// [`Machine::read`] on the real filesystem: a regular file (followed, as Cargo does)
/// read whole; anything else is no file.
///
/// # Errors
/// A file that exists and cannot be read, which [`with_build_dir`] counts as a choice.
#[expect(
    clippy::result_unit_err,
    reason = "Machine::read's shape: a file that cannot be read is a choice, whatever the error"
)]
pub fn read_config(path: &Path) -> Result<Option<String>, ()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => std::fs::read(path)
            .map(|b| Some(String::from_utf8_lossy(&b).into_owned()))
            .map_err(|_| ()),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}
