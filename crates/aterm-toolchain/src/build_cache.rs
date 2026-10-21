// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUILD-CACHE RULE: where `targo` puts a workspace's build intermediates.
//!
//! On macOS atpkg's `targo`/`targo-tippy` shims (and `cargo` in an aterm tab) hand a
//! command that builds a build directory Spotlight skips ([`decide`]): an `--unverified`
//! build, the lint or `clean` into Cargo's default target gets
//! `<cache><workspace>/.cargo-build` under `~/Library/Caches/aterm/cargo-build.noindex`
//! (binaries stay in `target/`); a build into a target directory the caller chose, and
//! Trust's verified lane (`targo trust`), get `<target>/build.noindex`. Only a caller who
//! chose the build directory itself keeps cargo's layout. atpkg binds the rule to this
//! process (`atpkg::build_cache::with_build_dir_here`) and renders it in `sh` for the
//! shim; `aterm-verify` binds it for the children it starts, so a verify run and a plain
//! `targo` build share one cache.
//!
//! EVERY BUILD DIRECTORY A TOOL OF THIS REPOSITORY CHOOSES IS NAMED `*.noindex`
//! ([`NOINDEX`]). On macOS Spotlight (`mds_stores`) indexes every file a check run
//! writes into a plain folder; a folder named `*.noindex` is skipped with everything
//! under it, while a `.metadata_never_index` file inside a plain folder is not honoured
//! (its files are indexed within seconds). A tool that pins a build to a target directory
//! of its own keeps the intermediates in `<target>/build.noindex` too
//! ([`target_build_dir`]), so only the artifacts cargo uplifts (`<target>/<profile>/aterm`)
//! land in the plain folder.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The cache root, relative to `$HOME`.
pub const CACHE_UNDER_HOME: &str = "Library/Caches/aterm/cargo-build.noindex";

/// The last component of every workspace's build directory under the cache.
pub const LEAF: &str = ".cargo-build";

/// The suffix of every build directory a tool of this repository chooses: Spotlight
/// skips a folder so named with all it holds (the module doc has the measurement).
pub const NOINDEX: &str = ".noindex";

/// `name` named for Spotlight to skip: `tippy` is `tippy.noindex`.
#[must_use]
pub fn noindex(name: &str) -> String {
    format!("{name}{NOINDEX}")
}

/// Where under a target directory that is not the cache a build's intermediates go
/// ([`BuildDir::Target`], [`target_build_dir`]).
pub const TARGET_BUILD_LEAF: &str = "build.noindex";

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
    /// Trust's `targo` (or Trust's own `cargo`, which is the same program): a build names
    /// its lane, `--unverified` or `trust` (Trust refuses a bare `targo build`).
    Targo,
    /// `targo-tippy`, the lint: every argument is its own, and it takes no lane.
    Tippy,
    /// An upstream `cargo`: it has no lane flag, and a leading `+<toolchain>` is rustup's
    /// and stays first.
    Cargo,
}

/// What [`decide`] reads of the machine, injected so every rule is tested without
/// touching the real one.
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

/// The environment variable that carries a build directory to `targo trust`
/// ([`BuildDir::TrustEnv`]).
pub const BUILD_DIR_ENV: &str = "CARGO_BUILD_BUILD_DIR";

/// Where [`decide`] puts a command's build intermediates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildDir {
    /// The shared cache, `<cache><workspace>/.cargo-build`: this `--config` value first.
    Cache(String),
    /// `<target>/build.noindex` under the target directory the command builds into — the
    /// one it chose, else Cargo's `{workspace-root}/target` — when the cache is not for it:
    /// this `--config` value first.
    Target(String),
    /// The same folder for Trust's `targo trust`, whose cargo reads no `--config` (measured:
    /// its intermediates stayed in `<target>/debug`): this directory as [`BUILD_DIR_ENV`]
    /// on its exec, which its builds inherit.
    TrustEnv(String),
}

/// THE RULE, as a pure function of the command line and the machine. `None` keeps the
/// command as typed: it builds nothing, it is outside `$HOME` or reached through a link,
/// or it chose its own build directory (`CARGO_BUILD_BUILD_DIR`, even empty; a cargo
/// config in the working directory's ancestors or `CARGO_HOME` naming `build-dir`; a
/// `--config` naming either directory; `-C`). Otherwise an `--unverified` build, the lint
/// (an alias whose first word is a building verb, as a cargo config in the working
/// directory's ancestors or `CARGO_HOME` defines it, counts as one: `targo --unverified
/// verify` is `run -p aterm-verify`) or `clean` into Cargo's default target directory gets
/// the shared cache
/// ([`BuildDir::Cache`]); one into a target directory it chose (`--target-dir`, then
/// `CARGO_TARGET_DIR`, then `CARGO_BUILD_TARGET_DIR`, relative to the working directory),
/// `targo trust`, and any build where the cache cannot be written get `<target>/
/// build.noindex` instead ([`BuildDir::Target`], [`BuildDir::TrustEnv`]) — unless that
/// target already lies in a `*.noindex` folder, or is the default while a config names
/// `target-dir`. atpkg's shim block is the same rule in `sh`.
#[must_use]
pub fn decide(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<BuildDir> {
    let home = m
        .home
        .filter(|h| h.is_absolute() && !h.as_os_str().as_encoded_bytes().ends_with(b"/"))?;
    let cwd = m.cwd?;
    let root = cache_root(home);
    let cache = config_value(&root)?;
    if !cwd.starts_with(home) || cwd == home || cwd.starts_with(&root) || has_brace(cwd) {
        return None;
    }
    // A `$PWD` that is not the real folder reached it through a link: the shim sees only
    // `$PWD`, so both halves leave it alone.
    if (m.env)("PWD").is_some_and(|p| Path::new(&p) != cwd) {
        return None;
    }
    if (m.env)(BUILD_DIR_ENV).is_some() {
        return None;
    }
    let at = usize::from(tool == Tool::Cargo && args.first().is_some_and(|a| a.starts_with('+')));
    let mut line = Line::read(&args[at..], tool, home, &root)?;
    let cargo_home = (m.env)("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".cargo"), PathBuf::from);
    let mut target_named = false;
    let mut alias_builds = false;
    for dir in cwd
        .ancestors()
        .map(|d| d.join(".cargo"))
        .chain([cargo_home])
    {
        let named = config_scan(&dir, line.alias.as_deref(), m);
        if named.build {
            return None;
        }
        target_named |= named.target;
        alias_builds |= named.alias_builds;
    }
    if line.alias.is_some() && !alias_builds {
        return None;
    }
    let chosen = line
        .target_dir
        .take()
        .map(OsString::from)
        .or_else(|| (m.env)("CARGO_TARGET_DIR"))
        .or_else(|| (m.env)("CARGO_BUILD_TARGET_DIR"));
    let Some(target) = chosen else {
        if target_named {
            return None;
        }
        if line.cache && cache_writable(home, &root, m) {
            return Some(BuildDir::Cache(cache));
        }
        if in_noindex(cwd) {
            return None;
        }
        return Some(line.place(format!("{{workspace-root}}/target/{TARGET_BUILD_LEAF}")));
    };
    let target = target.to_str()?;
    if target.is_empty() {
        return None;
    }
    let target = if Path::new(target).is_absolute() {
        target.to_owned()
    } else {
        format!("{}/{target}", cwd.to_str()?)
    };
    if in_noindex(Path::new(&target))
        || target
            .chars()
            .any(|c| matches!(c, '\'' | '{' | '}') || c.is_control())
    {
        return None;
    }
    Some(line.place(format!("{target}/{TARGET_BUILD_LEAF}")))
}

/// `args` with the `--config` [`decide`] adds, or `None` when it adds none: the command
/// keeps its layout, or it is `targo trust`, whose folder goes in the environment.
#[must_use]
pub fn with_build_dir(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<Vec<String>> {
    let (BuildDir::Cache(value) | BuildDir::Target(value)) = decide(args, tool, m)? else {
        return None;
    };
    let at = usize::from(tool == Tool::Cargo && args.first().is_some_and(|a| a.starts_with('+')));
    let mut out = Vec::with_capacity(args.len() + 2);
    out.extend_from_slice(&args[..at]);
    out.push("--config".to_owned());
    out.push(value);
    out.extend_from_slice(&args[at..]);
    Some(out)
}

/// Whether the cache, or the folder it will be made in, may be written.
fn cache_writable(home: &Path, root: &Path, m: &Machine<'_>) -> bool {
    (m.writable)(root)
        .or_else(|| root.parent().and_then(|p| (m.writable)(p)))
        .or_else(|| (m.writable)(&home.join("Library/Caches")))
        .unwrap_or(false)
}

/// Whether a folder of `p`'s is named `*.noindex`: Spotlight skips it already.
fn in_noindex(p: &Path) -> bool {
    p.components().any(|c| {
        c.as_os_str()
            .as_encoded_bytes()
            .ends_with(NOINDEX.as_bytes())
    })
}

/// Whether a path holds a `{` or `}`, which Cargo would read as a template once
/// `{workspace-root}` is expanded.
fn has_brace(p: &Path) -> bool {
    p.as_os_str()
        .as_encoded_bytes()
        .iter()
        .any(|b| matches!(b, b'{' | b'}'))
}

/// What the verb and flags half of the rule reads of a command line.
struct Line {
    /// An `--unverified` build (any build, for an upstream cargo), the lint or `clean`:
    /// the shared cache is for it.
    cache: bool,
    /// `targo trust`, which takes its build directory from the environment.
    trust: bool,
    /// A `--target-dir` before `--`.
    target_dir: Option<String>,
    /// A verb cargo does not build by itself, which builds only as an alias that does.
    alias: Option<String>,
}

impl Line {
    /// A building verb, or one an alias may make one (for `targo`, `--unverified …` or
    /// `trust`; `clean` and the lint take no lane), with nothing before `--` that names its
    /// own build directory; `None` otherwise.
    fn read(args: &[String], tool: Tool, home: &Path, root: &Path) -> Option<Self> {
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
        let mut target_dir = None;
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
                "--target-dir" => it.next().map(|v| target_dir = Some(v.to_owned())).is_some(),
                "-Z" | "--color" | "--explain" => verb.is_some() || it.next().is_some(),
                _ => {
                    if let Some(v) = a.strip_prefix("--config=") {
                        config_ok(v)
                    } else if let Some(v) = a.strip_prefix("--manifest-path=") {
                        manifest_ok(v)
                    } else if let Some(v) = a.strip_prefix("--target-dir=") {
                        target_dir = Some(v.to_owned());
                        true
                    } else if a.starts_with("-C") {
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
                return None;
            }
        }
        let lane = tool == Tool::Cargo || unverified;
        let (cache, trust, alias) = match verb {
            Some("clean" | "tippy" | "clippy") => (true, false, None),
            Some("trust") if tool == Tool::Targo => (false, true, None),
            Some(v) if BUILD_VERBS.contains(&v) && lane => (true, false, None),
            Some(v) if lane && !BUILD_VERBS.contains(&v) => (true, false, Some(v.to_owned())),
            _ => return None,
        };
        Some(Self {
            cache,
            trust,
            target_dir,
            alias,
        })
    }

    /// `dir` (`<target>/build.noindex`) handed over the way this command reads it.
    fn place(&self, dir: String) -> BuildDir {
        if self.trust {
            BuildDir::TrustEnv(dir)
        } else {
            BuildDir::Target(format!("build.build-dir='{dir}'"))
        }
    }
}

/// Whether a config value or line names a target or build directory.
fn names_a_dir(s: &str) -> bool {
    s.contains("target-dir") || s.contains("build-dir")
}

/// What `dir/config.toml` or `dir/config` says on lines that are not comments: whether a
/// line names a build or a target directory, and whether it defines the alias asked for
/// as a build. A file that exists and cannot be read names both directories. Read line by
/// line, as the shim reads it: an alias is `<name> = …` under `[alias]` or `alias.<name> =
/// …` anywhere, and builds when the first word of its value (a string or an array) is a
/// building verb.
#[derive(Default)]
struct Named {
    build: bool,
    target: bool,
    alias_builds: bool,
}

/// [`Named`] for the config files in `dir`, `alias` the verb to look up.
fn config_scan(dir: &Path, alias: Option<&str>, m: &Machine<'_>) -> Named {
    let mut named = Named::default();
    for name in ["config.toml", "config"] {
        match (m.read)(&dir.join(name)) {
            Err(()) => {
                return Named {
                    build: true,
                    target: true,
                    alias_builds: false,
                };
            }
            Ok(None) => {}
            Ok(Some(text)) => {
                let mut in_alias = false;
                for line in text.lines() {
                    let x = line.trim_start_matches([' ', '\t']);
                    if x.starts_with('#') {
                        continue;
                    }
                    named.build |= line.contains("build-dir");
                    named.target |= line.contains("target-dir");
                    if x.starts_with('[') {
                        in_alias = x.starts_with("[alias]");
                    } else if let Some(verb) = alias {
                        named.alias_builds |= alias_builds(x, verb, in_alias);
                    }
                }
            }
        }
    }
    named
}

/// Whether the line `x` (leading blanks gone) defines `verb` as an alias whose first word
/// is a building verb.
fn alias_builds(x: &str, verb: &str, in_alias: bool) -> bool {
    let rest = match x.strip_prefix("alias.").and_then(|r| r.strip_prefix(verb)) {
        Some(rest) => rest,
        None if in_alias => match x.strip_prefix(verb) {
            Some(rest) => rest,
            None => return false,
        },
        None => return false,
    };
    if !rest.starts_with([' ', '\t', '=']) {
        return false;
    }
    let Some((_, value)) = x.split_once('=') else {
        return false;
    };
    let value = value.trim_start_matches([' ', '\t', '[', '"', '\'']);
    let end = value
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(value.len());
    BUILD_VERBS.contains(&&value[..end])
}

/// Whether `dir/config.toml` or `dir/config` holds a line `names` holds that is not a
/// comment. A file that exists and cannot be read counts as one.
fn config_names(dir: &Path, m: &Machine<'_>, names: fn(&str) -> bool) -> bool {
    ["config.toml", "config"]
        .iter()
        .any(|name| match (m.read)(&dir.join(name)) {
            Err(()) => true,
            Ok(None) => false,
            Ok(Some(text)) => text
                .lines()
                .any(|line| names(line) && !line.trim_start_matches([' ', '\t']).starts_with('#')),
        })
}

/// THE FALLBACK: the `--config` value that keeps the intermediates of a build a tool
/// pinned to `target` (deps, incremental, fingerprints, build scripts) in
/// `<target>/build.noindex` ([`TARGET_BUILD_LEAF`]), out of Spotlight, while cargo still
/// uplifts the artifacts (`<target>/debug/aterm`, `examples/`) to `<target>/<profile>/`.
/// For a build a tool pins to `target` itself, as `aterm-verify` does where the shared
/// cache is not for its run.
///
/// `None` leaves cargo's layout alone: the caller chose a build directory
/// (`CARGO_BUILD_BUILD_DIR` set, even empty, or a cargo config in the working directory's
/// ancestors or in `CARGO_HOME`, else `$HOME/.cargo`, naming `build-dir` on a line that
/// is not a comment, or one that cannot be read), the working directory is unknown, or
/// `target` is not absolute or is a path the TOML literal cannot hold or Cargo would
/// read as a template ([`config_value`]'s refusals).
#[must_use]
pub fn target_build_dir(target: &Path, m: &Machine<'_>) -> Option<String> {
    if !target.is_absolute() || (m.env)("CARGO_BUILD_BUILD_DIR").is_some() {
        return None;
    }
    let names_build_dir = |line: &str| line.contains("build-dir");
    let cwd = m.cwd?;
    if cwd
        .ancestors()
        .any(|d| config_names(&d.join(".cargo"), m, names_build_dir))
    {
        return None;
    }
    let cargo_home = (m.env)("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| m.home.map(|h| h.join(".cargo")));
    if cargo_home.is_some_and(|h| config_names(&h, m, names_build_dir)) {
        return None;
    }
    let dir = target.join(TARGET_BUILD_LEAF);
    let dir = dir.to_str()?;
    if dir
        .chars()
        .any(|c| matches!(c, '\'' | '{' | '}') || c.is_control())
    {
        return None;
    }
    Some(format!("build.build-dir='{dir}'"))
}

/// [`Machine::read`] on the real filesystem: a regular file (followed, as Cargo does)
/// read whole; anything else is no file.
///
/// # Errors
/// A file that exists and cannot be read, which [`decide`] counts as a choice.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A machine whose environment is `env` and whose config files are `files`.
    fn decide(
        target: &str,
        cwd: Option<&str>,
        env: &[(&str, &str)],
        files: &[(&str, &str)],
    ) -> Option<String> {
        let env: BTreeMap<String, OsString> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        let files: BTreeMap<PathBuf, Result<String, ()>> = files
            .iter()
            .map(|(p, text)| {
                let text = if *text == "<unreadable>" {
                    Err(())
                } else {
                    Ok((*text).to_owned())
                };
                (PathBuf::from(p), text)
            })
            .collect();
        let lookup = |k: &str| env.get(k).cloned();
        let read = |p: &Path| files.get(p).cloned().transpose();
        let writable = |_: &Path| Some(true);
        let cwd = cwd.map(Path::new);
        let m = Machine {
            home: Some(Path::new("/h")),
            cwd,
            env: &lookup,
            read: &read,
            writable: &writable,
        };
        target_build_dir(Path::new(target), &m)
    }

    /// A build pinned to a target directory keeps its intermediates in
    /// `<target>/build.noindex`; a caller's own build directory, an unknown working
    /// directory and a path the literal cannot hold leave the layout alone.
    #[test]
    fn a_pinned_target_keeps_its_intermediates_out_of_spotlight() {
        let w = Some("/w/repo");
        assert_eq!(
            decide("/w/repo/target", w, &[], &[]).as_deref(),
            Some("build.build-dir='/w/repo/target/build.noindex'")
        );
        // A config naming only the target directory is not a build-dir choice: the
        // tool's CARGO_TARGET_DIR outranks it anyway.
        assert!(
            decide(
                "/t",
                w,
                &[],
                &[("/w/repo/.cargo/config.toml", "[build]\ntarget-dir = 'x'\n")]
            )
            .is_some()
        );
        // A commented line is no choice either.
        assert!(
            decide(
                "/t",
                w,
                &[],
                &[("/w/.cargo/config.toml", "# build-dir = 'x'\n")]
            )
            .is_some()
        );
        for (why, got) in [
            ("relative", decide("target", w, &[], &[])),
            ("no cwd", decide("/t", None, &[], &[])),
            (
                "env",
                decide("/t", w, &[("CARGO_BUILD_BUILD_DIR", "")], &[]),
            ),
            (
                "ancestor config",
                decide(
                    "/t",
                    w,
                    &[],
                    &[("/w/.cargo/config.toml", "[build]\nbuild-dir = 'x'\n")],
                ),
            ),
            (
                "home config",
                decide("/t", w, &[], &[("/h/.cargo/config", "build.build-dir='x'")]),
            ),
            (
                "CARGO_HOME config",
                decide(
                    "/t",
                    w,
                    &[("CARGO_HOME", "/ch")],
                    &[("/ch/config.toml", "  build-dir = 'x'")],
                ),
            ),
            (
                "unreadable config",
                decide("/t", w, &[], &[("/w/repo/.cargo/config", "<unreadable>")]),
            ),
            ("quote", decide("/t'q", w, &[], &[])),
            ("brace", decide("/t{workspace-root}", w, &[], &[])),
            ("control", decide("/t\nx", w, &[], &[])),
        ] {
            assert_eq!(got, None, "{why}");
        }
        // CARGO_HOME moves the home config the rule reads: ~/.cargo is then not read.
        assert!(
            decide(
                "/t",
                w,
                &[("CARGO_HOME", "/ch")],
                &[("/h/.cargo/config.toml", "build-dir = 'x'")]
            )
            .is_some()
        );
    }
}
